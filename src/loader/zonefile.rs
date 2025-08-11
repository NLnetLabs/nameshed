//! Loading zones from zonefiles.

use std::{cmp::Ordering, fmt, fs::File, io::BufReader, sync::Arc};

use camino::Utf8Path;
use domain::{
    new::{
        base::{RClass, RType, Record},
        rdata::RecordData,
        zonefile::simple::{Entry, ZonefileError, ZonefileScanner},
    },
    utils::dst::UnsizedCopy,
};
use log::trace;

use crate::zone::{
    contents::{RegularRecord, SoaRecord, Uncompressed},
    Zone,
};

use super::RefreshError;

//----------- refresh() --------------------------------------------------------

/// Refresh a zone from a zonefile.
///
/// See [`super::refresh()`].
pub fn refresh(
    zone: &Arc<Zone>,
    path: &Utf8Path,
    latest: Option<Arc<Uncompressed>>,
) -> Result<Option<super::Refresh>, RefreshError> {
    // Open the zonefile.
    let file =
        BufReader::new(File::open(path).map_err(|err| RefreshError::Zonefile(Error::Open(err)))?);
    let mut scanner = ZonefileScanner::new(file, Some(&zone.name));

    // Assume the first record is the SOA.
    let soa = match scanner.scan().map_err(Error::Misformatted)? {
        Some(Entry::Record(Record {
            rname,
            rtype: rtype @ RType::SOA,
            rclass: rclass @ RClass::IN,
            ttl,
            rdata: RecordData::Soa(rdata),
        })) => {
            if rname != &*zone.name {
                return Err(Error::MismatchedOrigin.into());
            }

            SoaRecord(Record {
                rname: zone.name.unsized_copy_into(),
                rtype,
                rclass,
                ttl,
                rdata: rdata.map_names(|n| n.unsized_copy_into()),
            })
        }

        Some(Entry::Include { .. }) => return Err(Error::UnsupportedInclude.into()),
        _ => return Err(Error::MissingStartSoa.into()),
    };

    // Check whether the SOA has changed.
    let local_serial = latest.as_ref().map(|l| l.soa.rdata.serial);
    let remote_serial = soa.rdata.serial;
    match local_serial.partial_cmp(&Some(remote_serial)) {
        // The local copy is outdated.
        Some(Ordering::Less) => {}

        // The local copy is up-to-date.
        Some(Ordering::Equal) => return Ok(None),

        // The remote copy is outdated.
        _ => {
            return Err(RefreshError::OutdatedRemote {
                local_serial: local_serial.unwrap(),
                remote_serial,
            })
        }
    }

    // Fetch the rest of the zonefile.
    let mut all = Vec::<RegularRecord>::new();
    while let Some(entry) = scanner.scan().map_err(Error::Misformatted)? {
        let record = match entry {
            Entry::Record(record) => record,
            Entry::Include { .. } => return Err(Error::UnsupportedInclude.into()),
        };

        all.push(RegularRecord(
            record.transform(|name| name.unsized_copy_into(), |data| data.into()),
        ));
    }

    // Finalize the remote copy.
    all.sort_unstable();
    let all = all.into_boxed_slice();
    let uncompressed = Arc::new(Uncompressed { soa, all });

    // Compress the local copy using the remote copy.
    let compressed = latest.map(|latest| (Arc::new(latest.compress(&uncompressed)), latest));

    Ok(Some(super::Refresh {
        uncompressed,
        compressed,
    }))
}

//----------- load() -----------------------------------------------------------

/// Load a zone from a zonefile.
pub fn load(zone: &Arc<Zone>, path: &Utf8Path) -> Result<Uncompressed, Error> {
    trace!("Reloading {:?} from zonefile {path}", zone.name);

    // Open the zonefile.
    let file = BufReader::new(File::open(path).map_err(Error::Open)?);
    let mut scanner = ZonefileScanner::new(file, Some(&zone.name));

    // Assume the first record is the SOA.
    let soa = match scanner.scan()? {
        Some(Entry::Record(Record {
            rname,
            rtype: rtype @ RType::SOA,
            rclass: rclass @ RClass::IN,
            ttl,
            rdata: RecordData::Soa(rdata),
        })) => {
            if rname != &*zone.name {
                return Err(Error::MismatchedOrigin);
            }

            SoaRecord(Record {
                rname: zone.name.unsized_copy_into(),
                rtype,
                rclass,
                ttl,
                rdata: rdata.map_names(|n| n.unsized_copy_into()),
            })
        }

        Some(Entry::Include { .. }) => return Err(Error::UnsupportedInclude),
        _ => return Err(Error::MissingStartSoa),
    };

    // Parse the remaining records.
    let mut all = Vec::<RegularRecord>::new();
    while let Some(entry) = scanner.scan()? {
        let record = match entry {
            Entry::Record(record) => record,
            Entry::Include { .. } => return Err(Error::UnsupportedInclude),
        };

        all.push(RegularRecord(
            record.transform(|name| name.unsized_copy_into(), |data| data.into()),
        ));
    }

    // Sort the records.
    all.sort_unstable();

    let all = all.into_boxed_slice();
    Ok(Uncompressed { soa, all })
}

//----------- Error ------------------------------------------------------------

/// An error in loading a zone from a zonefile.
#[derive(Debug)]
pub enum Error {
    /// The zonefile could not be opened.
    Open(std::io::Error),

    /// The zonefile was misformatted.
    Misformatted(ZonefileError),

    /// The zonefile starts with a SOA record for a different zone.
    MismatchedOrigin,

    /// The zonefile did not start with a SOA record.
    MissingStartSoa,

    /// Zonefile include directories are not supported.
    UnsupportedInclude,
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Open(error) => Some(error),
            Error::Misformatted(error) => Some(error),
            Error::MismatchedOrigin => None,
            Error::MissingStartSoa => None,
            Error::UnsupportedInclude => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open(error) => error.fmt(f),
            Error::Misformatted(error) => error.fmt(f),
            Error::MismatchedOrigin => write!(f, "the zonefile has the wrong origin name"),
            Error::MissingStartSoa => write!(f, "the zonefile does not start with a SOA record"),
            Error::UnsupportedInclude => write!(f, "zonefile include directives are not supported"),
        }
    }
}

//--- Conversion

impl From<ZonefileError> for Error {
    fn from(value: ZonefileError) -> Self {
        Self::Misformatted(value)
    }
}
