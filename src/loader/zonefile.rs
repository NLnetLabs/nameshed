//! Loading zones from zonefiles.

use std::{cmp::Ordering, fmt, fs::File, io::BufReader, sync::Arc};

use camino::Utf8Path;
use domain::{
    new::{
        base::{RClass, RType, Record, Serial},
        rdata::RecordData,
        zonefile::simple::{Entry, ZonefileError, ZonefileScanner},
    },
    utils::dst::UnsizedCopy,
};

use crate::zone::{
    contents::{self, RegularRecord, SoaRecord, Uncompressed},
    Zone, ZoneContents,
};

use super::{RefreshError, ReloadError};

//----------- refresh() --------------------------------------------------------

/// Refresh a zone from a zonefile.
///
/// See [`super::refresh()`].
pub fn refresh(
    zone: &Arc<Zone>,
    path: &Utf8Path,
    latest: Option<Arc<Uncompressed>>,
) -> Result<Option<Serial>, RefreshError> {
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
    let local_serial = latest.map(|l| l.soa.rdata.serial);
    let remote_serial = soa.rdata.serial;
    if local_serial.partial_cmp(&Some(remote_serial)) != Some(Ordering::Less) {
        // The local copy is up-to-date.
        return Ok(None);
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
    let remote = Uncompressed { soa, all };

    // A compressed copy of the local version of the zone.
    let mut compressed_local: Option<(contents::Compressed, Arc<contents::Uncompressed>)> = None;

    // Loop while the zone contents change from under us.
    loop {
        // Load the latest local copy of the zone.
        let mut data = zone.data.lock().unwrap();
        let Some(contents) = &mut data.contents else {
            // There were no previous contents to the zone.
            //
            // Save the freshly loaded version and return.

            data.contents = Some(ZoneContents {
                latest: Arc::new(remote),
                previous: Default::default(),
            });

            return Ok(Some(remote_serial));
        };

        // Compare the local and remote copies.
        let local_serial = contents.latest.soa.rdata.serial;
        match local_serial.partial_cmp(&remote_serial) {
            Some(Ordering::Less) => {
                // We need to compress the local copy.  Check if we
                // have already done so.

                let Some((local, _)) = compressed_local
                    .take()
                    .filter(|(_, latest)| Arc::ptr_eq(&contents.latest, latest))
                else {
                    let local = contents.latest.clone();
                    std::mem::drop(data);

                    // Compress the local copy using the remote copy.
                    compressed_local = Some((local.compress(&remote), local));

                    continue;
                };

                // Update the zone contents.
                contents.latest = Arc::new(remote);
                contents.previous.push_back(Arc::new(local));
                return Ok(Some(remote_serial));
            }

            _ => {
                // The local copy is up-to-date.
                return Ok(None);
            }
        }
    }
}

//----------- reload() ---------------------------------------------------------

/// Reload a zone from a zonefile.
///
/// See [`super::reload()`].
pub fn reload(zone: &Arc<Zone>, path: &Utf8Path) -> Result<Option<Serial>, ReloadError> {
    // Load the complete zone.
    let remote = load(zone, path)?;
    let remote_serial = remote.soa.rdata.serial;

    // A compressed copy of the local version of the zone.
    let mut compressed_local: Option<(contents::Compressed, Arc<contents::Uncompressed>)> = None;

    // Loop while the zone contents change from under us.
    loop {
        // Load the latest local copy of the zone.
        let mut data = zone.data.lock().unwrap();
        let Some(contents) = &mut data.contents else {
            // There were no previous contents to the zone.
            //
            // Save the freshly loaded version and return.

            data.contents = Some(ZoneContents {
                latest: Arc::new(remote),
                previous: Default::default(),
            });

            return Ok(Some(remote_serial));
        };

        // Compare the local and remote copies.
        let local_serial = contents.latest.soa.rdata.serial;
        match local_serial.partial_cmp(&remote_serial) {
            Some(Ordering::Less) => {
                // We need to compress the local copy.  Check if we
                // have already done so.

                let Some((local, _)) = compressed_local
                    .take()
                    .filter(|(_, latest)| Arc::ptr_eq(&contents.latest, latest))
                else {
                    // We need to perform the compression.  Unlock
                    // the zone, as this is an expensive operation.
                    let local = contents.latest.clone();
                    std::mem::drop(data);
                    compressed_local = Some((local.compress(&remote), local));
                    continue;
                };

                // Update the zone contents.
                contents.latest = Arc::new(remote);
                contents.previous.push_back(Arc::new(local));
                return Ok(Some(remote_serial));
            }

            Some(Ordering::Equal) => {
                // Verify the consistency of the two copies.
                let local = contents.latest.clone();
                std::mem::drop(data);
                if local.eq_unsigned(&remote) {
                    return Ok(None);
                } else {
                    return Err(ReloadError::Inconsistent);
                }
            }

            _ => {
                // The remote copy is outdated.
                return Err(ReloadError::OutdatedRemote {
                    local_serial,
                    remote_serial,
                });
            }
        }
    }
}

//----------- load() -----------------------------------------------------------

/// Load a zone from a zonefile.
pub fn load(zone: &Arc<Zone>, path: &Utf8Path) -> Result<Uncompressed, Error> {
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
