//! Storing the contents of a zone.

use std::{cmp::Ordering, collections::VecDeque, sync::Arc};

use domain::{
    new::{
        base::{
            name::{Name, RevName},
            CanonicalRecordData, Record,
        },
        rdata::{BoxedRecordData, Soa},
    },
    utils::dst::UnsizedCopy,
};

/// The contents of a zone.
///
/// This stores the resource records making up the zone, across different
/// versions, while distinguishing unsigned and signed contents.
///
/// Internally, only the latest version of the zone is stored in full.  All
/// previous versions are stored as diffs, to improve memory efficiency.
pub struct ZoneContents {
    /// The latest version of the zone.
    pub latest: Arc<Uncompressed>,

    /// The previous versions of the zone.
    ///
    /// This is a sliding window of the history of the zone.  Each element is a
    /// particular version of the zone, represented as the diff between itself
    /// and the next version.
    pub previous: VecDeque<Arc<Compressed>>,
}

/// An uncompressed representation of the contents of a version of a zone.
pub struct Uncompressed {
    /// The SOA record of the zone.
    pub soa: Record<Box<RevName>, Soa<Box<Name>>>,

    /// The resource records of the zone.
    ///
    /// The records are in ascending order of owner name and record type.
    pub all: Box<[Record<Box<RevName>, BoxedRecordData>]>,
    //
    // TODO: Separate storage for DNSSEC-related records.
    // TODO: Separate storage for glue records.
}

impl Uncompressed {
    /// Compress this zone relative to the next version.
    ///
    /// # Panics
    ///
    /// Panics if the next version does not have a higher serial number than
    /// the current version (as per serial number arithmetic).
    pub fn compress(&self, next: &Uncompressed) -> Compressed {
        #[inline]
        fn clone_record(
            record: &Record<Box<RevName>, BoxedRecordData>,
        ) -> Record<Box<RevName>, BoxedRecordData> {
            record.transform_ref(|name| (*name).unsized_copy_into(), |data| data.clone())
        }

        assert!(self.soa.rdata.serial < next.soa.rdata.serial);

        let soa = self.soa.transform_ref(
            |n| (*n).unsized_copy_into(),
            |d| d.map_names_by_ref(|n| (*n).unsized_copy_into()),
        );
        let next_soa = next.soa.transform_ref(
            |n| (*n).unsized_copy_into(),
            |d| d.map_names_by_ref(|n| (*n).unsized_copy_into()),
        );

        let mut all_this = self.all.iter().peekable();
        let mut all_next = next.all.iter().peekable();

        let mut only_this = Vec::new();
        let mut only_next = Vec::new();

        while let (Some(rt), Some(rn)) = (all_this.peek(), all_next.peek()) {
            // Compare the records.
            let ord = (&rt.rname, rt.rtype, rt.ttl)
                .cmp(&(&rn.rname, rn.rtype, rn.ttl))
                .then_with(|| rt.rdata.cmp_canonical(&rn.rdata));

            match ord {
                Ordering::Less => {
                    // 'rt' does not appear in 'all_next'.
                    only_this.push(clone_record(all_this.next().unwrap()));
                }
                Ordering::Equal => {
                    // This record appears in both versions.
                }
                Ordering::Greater => {
                    // 'rn' does not appear in 'all_this'.
                    only_next.push(clone_record(all_next.next().unwrap()));
                }
            }
        }

        // At most one of 'all_this' and 'all_next' has any records, which
        // accordingly belong in 'only_this' and 'only_next' respectively.
        only_this.extend(all_this.map(clone_record));
        only_next.extend(all_next.map(clone_record));

        Compressed {
            soa,
            next_soa,
            only_this: only_this.into_boxed_slice(),
            only_next: only_next.into_boxed_slice(),
        }
    }
}

/// A compressed representation of a zone.
pub struct Compressed {
    /// The SOA record of this version of the zone.
    pub soa: Record<Box<RevName>, Soa<Box<Name>>>,

    /// The SOA record of the next version of the zone.
    pub next_soa: Record<Box<RevName>, Soa<Box<Name>>>,

    /// The resource records only present in this version of the zone.
    ///
    /// These records are present in this version of the zone and not in the
    /// next version.
    pub only_this: Box<[Record<Box<RevName>, BoxedRecordData>]>,

    /// The resource records only present in the next version of the zone.
    ///
    /// These records are present in the next version of the zone and not in
    /// this version.
    pub only_next: Box<[Record<Box<RevName>, BoxedRecordData>]>,
    //
    // TODO: Separate storage for DNSSEC-related records.
    // TODO: Separate storage for glue records.
}
