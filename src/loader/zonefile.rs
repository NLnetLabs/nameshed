//! Loading zones from zonefiles.

use std::{cmp::Ordering, fs::File, io::BufReader, sync::Arc};

use camino::Utf8Path;
use domain::{
    new::{
        base::{CanonicalRecordData, RClass, RType, Record, Serial},
        rdata::RecordData,
        zonefile::simple::{Entry, ZonefileError, ZonefileScanner},
    },
    utils::dst::UnsizedCopy,
};

use crate::zone::{
    contents::{self, RegularRecord, SoaRecord, Uncompressed},
    Zone, ZoneContents,
};

use super::ReloadError;

//----------- reload() ---------------------------------------------------------

/// Reload a zone from a zonefile.
///
/// See [`super::reload()`].
pub fn reload(zone: &Arc<Zone>, path: &Utf8Path) -> Result<Option<Serial>, ReloadError> {
    // Load the complete zone.
    let remote = load(zone, path)?;
    let remote_serial = remote.soa.rdata.serial;

    let mut compressed_local = None::<contents::Compressed>;

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

                let Some(local) = compressed_local.take_if(|l| l.soa.rdata.serial == local_serial)
                else {
                    // We need to perform the compression.  Unlock
                    // the zone, as this is an expensive operation.
                    let local = contents.latest.clone();
                    std::mem::drop(data);
                    compressed_local = Some(local.compress(&remote));
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

//----------- refresh() --------------------------------------------------------

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

            SoaRecord {
                rname: zone.name.unsized_copy_into(),
                rtype,
                rclass,
                ttl,
                rdata: rdata.map_names(|n| n.unsized_copy_into()),
            }
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

        all.push(record.transform(|name| name.unsized_copy_into(), |data| data.into()));
    }

    // Sort the records.
    all.sort_unstable_by(|l, r| {
        (&l.rname, l.rtype)
            .cmp(&(&r.rname, r.rtype))
            .then_with(|| l.rdata.cmp_canonical(&r.rdata))
    });

    let all = all.into_boxed_slice();
    Ok(Uncompressed { soa, all })
}

//----------- Error ------------------------------------------------------------

/// An error in loading a zone from a zonefile.
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

impl From<ZonefileError> for Error {
    fn from(value: ZonefileError) -> Self {
        Self::Misformatted(value)
    }
}
