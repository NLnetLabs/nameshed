//! Loading zones from zonefiles.

use std::{fs::File, io::BufReader, sync::Arc};

use camino::Utf8Path;
use domain::{
    new::{
        base::{CanonicalRecordData, RClass, RType, Record},
        rdata::RecordData,
        zonefile::simple::{Entry, ZonefileError, ZonefileScanner},
    },
    utils::dst::UnsizedCopy,
};

use crate::zone::{
    contents::{RegularRecord, SoaRecord, Uncompressed},
    Zone,
};

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
