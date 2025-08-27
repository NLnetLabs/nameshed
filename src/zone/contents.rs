//! Storing the contents of a zone.

use std::{
    cmp::Ordering,
    collections::VecDeque,
    fmt,
    iter::Peekable,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use domain::{
    new::{
        base::{
            name::{Name, NameBuf, RevName, RevNameBuf},
            wire::{BuildBytes, ParseBytes},
            CanonicalRecordData, Record,
        },
        rdata::{BoxedRecordData, Soa},
    },
    utils::dst::UnsizedCopy,
    zonetree::{self, types::ZoneUpdate, update::ZoneUpdater},
};

//----------- ZoneContents -----------------------------------------------------

/// The contents of a zone.
///
/// This stores the resource records making up the zone, across different
/// versions, while distinguishing unsigned and signed contents.
///
/// Internally, only the latest version of the zone is stored in full.  All
/// previous versions are stored as diffs, to improve memory efficiency.
#[derive(Debug)]
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

//----------- Uncompressed -----------------------------------------------------

/// An uncompressed representation of the contents of a version of a zone.
#[derive(Clone)]
pub struct Uncompressed {
    /// The SOA record of the zone.
    pub soa: SoaRecord,

    /// The resource records of the zone.
    ///
    /// The records are in ascending order of owner name and record type.
    pub all: Box<[RegularRecord]>,
    //
    // TODO: Separate storage for DNSSEC-related records.
    // TODO: Separate storage for glue records.
}

impl Uncompressed {
    /// Compress this zone relative to the next version.
    pub fn compress(&self, next: &Uncompressed) -> Compressed {
        let soa = self.soa.clone();
        let next_soa = next.soa.clone();

        let mut only_this = Vec::new();
        let mut only_next = Vec::new();
        for [rt, rn] in merge([&self.all, &next.all]) {
            match [rt, rn] {
                // The record is unchanged.
                [Some(_), Some(_)] => {}
                [None, Some(rn)] => only_next.push(rn.clone()),
                [Some(rt), None] => only_this.push(rt.clone()),
                [None, None] => unreachable!(),
            }
        }

        Compressed {
            soa,
            next_soa,
            only_this: only_this.into_boxed_slice(),
            only_next: only_next.into_boxed_slice(),
        }
    }

    /// Compare this to another zone version by unsigned records.
    ///
    /// If the two have all the same records, with the same data (up to case
    /// sensitivity for embededd domain names as per convention), `true` is
    /// returned.
    pub fn eq_unsigned(&self, other: &Self) -> bool {
        // Check the SOA records.
        if self.soa != other.soa {
            return false;
        }

        // TODO: Separate the signing-related records from others.
        self.all.iter().eq(&other.all)
    }

    /// Forward this zone using the given [`Compressed`] version of it.
    pub fn forward(&self, compressed: &Compressed) -> Result<Self, ForwardError> {
        // Verify that the forward can happen.
        if self.soa.rname != compressed.soa.rname {
            return Err(ForwardError::MismatchedZones);
        } else if self.soa.rdata.serial != compressed.soa.rdata.serial {
            return Err(ForwardError::MismatchedVersions);
        }

        // Check the SOA record.
        if self.soa != compressed.soa {
            return Err(ForwardError::Inconsistent);
        }

        // Update the remaining records.
        let mut next = Vec::new();
        for [a, ot, on] in merge([&self.all, &compressed.only_this, &compressed.only_next]) {
            match [a, ot, on] {
                // Remove the record.
                [Some(_), Some(_), None] => {}
                // Add the record ... but it is already present.
                [Some(_), None, Some(_)] => return Err(ForwardError::Inconsistent),
                // Leave the record unchanged.
                [Some(a), None, None] => next.push(a.clone()),
                // Remove the record ... but it was not present.
                [None, Some(_), None] => return Err(ForwardError::Inconsistent),
                // Add the record.
                [None, None, Some(on)] => next.push(on.clone()),

                [_, Some(_), Some(_)] => panic!("duplicate record in 'only_this' and 'only_next'"),
                [None, None, None] => unreachable!(),
            }
        }

        Ok(Self {
            soa: compressed.next_soa.clone(),
            all: next.into_boxed_slice(),
        })
    }

    /// Forward this zone using the given [`Compressed`] version of it.
    pub fn forward_from(&mut self, compressed: Compressed) -> Result<(), ForwardError> {
        // Verify that the forward can happen.
        if self.soa.rname != compressed.soa.rname {
            return Err(ForwardError::MismatchedZones);
        } else if self.soa.rdata.serial != compressed.soa.rdata.serial {
            return Err(ForwardError::MismatchedVersions);
        }

        // Check the SOA record.
        if self.soa != compressed.soa {
            return Err(ForwardError::Inconsistent);
        }

        // Update the remaining records.
        let mut next = Vec::new();
        let all = std::mem::take(&mut self.all);
        for [a, ot, on] in merge([all, compressed.only_this, compressed.only_next]) {
            match [a, ot, on] {
                // Remove the record.
                [Some(_), Some(_), None] => {}
                // Add the record ... but it is already present.
                [Some(_), None, Some(_)] => return Err(ForwardError::Inconsistent),
                // Leave the record unchanged.
                [Some(a), None, None] => next.push(a),
                // Remove the record ... but it was not present.
                [None, Some(_), None] => return Err(ForwardError::Inconsistent),
                // Add the record.
                [None, None, Some(on)] => next.push(on),

                [_, Some(_), Some(_)] => panic!("duplicate record in 'only_this' and 'only_next'"),
                [None, None, None] => unreachable!(),
            }
        }

        // Update 'self'.
        self.soa = compressed.next_soa;
        self.all = next.into_boxed_slice();
        Ok(())
    }

    /// Write these contents into the given zonetree.
    pub async fn write_into_zonetree(&self, zone: &zonetree::Zone) {
        let mut updater = ZoneUpdater::new(zone.clone(), false).await.unwrap();

        // Clear all existing records.
        updater.apply(ZoneUpdate::DeleteAllRecords).await.unwrap();

        // Add every record in turn.
        for record in &self.all {
            let record: OldRecord = record.clone().into();
            updater.apply(ZoneUpdate::AddRecord(record)).await.unwrap();
        }

        // Commit the update with the SOA record.
        let soa: OldRecord = self.soa.clone().into();
        updater.apply(ZoneUpdate::Finished(soa)).await.unwrap();
    }
}

impl fmt::Debug for Uncompressed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Uncompressed")
            .field("serial", &self.soa.rdata.serial)
            .finish()
    }
}

//----------- Compressed -------------------------------------------------------

/// A compressed representation of a zone.
#[derive(Clone)]
pub struct Compressed {
    /// The SOA record of this version of the zone.
    pub soa: SoaRecord,

    /// The SOA record of the next version of the zone.
    pub next_soa: SoaRecord,

    /// The resource records only present in this version of the zone.
    ///
    /// These records are present in this version of the zone and not in the
    /// next version.
    pub only_this: Box<[RegularRecord]>,

    /// The resource records only present in the next version of the zone.
    ///
    /// These records are present in the next version of the zone and not in
    /// this version.
    pub only_next: Box<[RegularRecord]>,
    //
    // TODO: Separate storage for DNSSEC-related records.
    // TODO: Separate storage for glue records.
}

impl Compressed {
    /// Merge the next [`Compressed`] zone with this one.
    ///
    /// # Errors
    ///
    /// Fails if an inconsistency is detected.
    pub fn merge_from_next(&mut self, next: &Compressed) -> Result<(), MergeError> {
        // Verify that the merge can happen.
        if self.next_soa.rname != next.soa.rname {
            return Err(MergeError::MismatchedZones);
        } else if self.next_soa.rdata.serial != next.soa.rdata.serial {
            return Err(MergeError::NotAdjacentVersions);
        } else if self.next_soa != next.soa {
            return Err(MergeError::MismatchedSoa);
        } else if self
            .soa
            .rdata
            .serial
            .partial_cmp(&next.next_soa.rdata.serial)
            != Some(Ordering::Less)
        {
            return Err(MergeError::SerialDiffOverflow);
        }

        // Proceed with merging the zones.
        let mut only_this = Vec::new();
        let mut only_next = Vec::new();
        for [tot, ton, not, non] in merge([
            &self.only_this,
            &self.only_next,
            &next.only_this,
            &next.only_next,
        ]) {
            match [tot, ton, not, non] {
                // Remove the record twice.
                [Some(_), None, Some(_), None] => return Err(MergeError::Inconsistent),
                // Remove then add the record.
                [Some(_), None, None, Some(_)] => {}
                // Remove the record.
                [Some(tot), None, None, None] => only_this.push(tot.clone()),
                // Add then remove the record.
                [None, Some(_), Some(_), None] => {}
                // Add the record twice.
                [None, Some(_), None, Some(_)] => return Err(MergeError::Inconsistent),
                // Add the record.
                [None, Some(ton), None, None] => only_next.push(ton.clone()),
                // Remove the record.
                [None, None, Some(not), None] => only_this.push(not.clone()),
                // Add the record.
                [None, None, None, Some(non)] => only_next.push(non.clone()),

                [Some(_), Some(_), _, _] | [_, _, Some(_), Some(_)] => {
                    panic!("duplicate record in 'only_this' and 'only_next'")
                }
                [None, None, None, None] => unreachable!(),
            }
        }

        // Update 'self'.
        self.next_soa = next.next_soa.clone();
        self.only_this = only_this.into_boxed_slice();
        self.only_next = only_next.into_boxed_slice();
        Ok(())
    }

    /// Write these changes into the given zonetree.
    pub async fn write_into_zonetree(&self, zone: &zonetree::Zone) {
        let mut updater = ZoneUpdater::new(zone.clone(), false).await.unwrap();

        // First remove the 'only-this' records.
        let this_soa: OldRecord = self.soa.clone().into();
        updater
            .apply(ZoneUpdate::BeginBatchDelete(this_soa))
            .await
            .unwrap();
        for record in &self.only_this {
            let record: OldRecord = record.clone().into();
            updater
                .apply(ZoneUpdate::DeleteRecord(record))
                .await
                .unwrap();
        }

        // Then add the 'only-next' records.
        let next_soa: OldRecord = self.next_soa.clone().into();
        updater
            .apply(ZoneUpdate::BeginBatchAdd(next_soa))
            .await
            .unwrap();
        for record in &self.only_next {
            let record: OldRecord = record.clone().into();
            updater.apply(ZoneUpdate::AddRecord(record)).await.unwrap();
        }

        // Commit the update with the new SOA record.
        let next_soa: OldRecord = self.next_soa.clone().into();
        updater.apply(ZoneUpdate::Finished(next_soa)).await.unwrap();
    }
}

impl fmt::Debug for Compressed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Compressed")
            .field("serial", &self.soa.rdata.serial)
            .field("next_serial", &self.next_soa.rdata.serial)
            .finish()
    }
}

//============ Helpers =========================================================

pub type OldName = domain::base::ParsedName<bytes::Bytes>;
pub type OldRecordData = domain::rdata::ZoneRecordData<bytes::Bytes, OldName>;
pub type OldRecord = domain::base::Record<OldName, OldRecordData>;

//----------- SoaRecord --------------------------------------------------------

/// A SOA record.
#[derive(Clone, Debug)]
pub struct SoaRecord(pub Record<Box<RevName>, Soa<Box<Name>>>);

impl PartialEq for SoaRecord {
    fn eq(&self, other: &Self) -> bool {
        (&self.rname, self.rtype, self.ttl) == (&other.rname, other.rtype, other.ttl)
            && self.rdata.cmp_canonical(&other.rdata).is_eq()
    }
}

impl Eq for SoaRecord {}

impl PartialOrd for SoaRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SoaRecord {
    fn cmp(&self, other: &Self) -> Ordering {
        (&self.rname, self.rtype, self.ttl)
            .cmp(&(&other.rname, other.rtype, other.ttl))
            .then_with(|| self.rdata.cmp_canonical(&other.rdata))
    }
}

impl Deref for SoaRecord {
    type Target = Record<Box<RevName>, Soa<Box<Name>>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for SoaRecord {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<OldRecord> for SoaRecord {
    fn from(value: OldRecord) -> Self {
        let mut bytes = Vec::new();
        value.compose(&mut bytes).unwrap();
        let record = Record::parse_bytes(&bytes)
            .expect("'Record' serializes records correctly")
            .transform(
                |name: RevNameBuf| name.unsized_copy_into(),
                |data: Soa<NameBuf>| data.map_names(|name| name.unsized_copy_into()),
            );
        SoaRecord(record)
    }
}

impl From<SoaRecord> for OldRecord {
    fn from(value: SoaRecord) -> Self {
        let mut bytes = vec![0u8; value.0.built_bytes_size()];
        value.0.build_bytes(&mut bytes).unwrap();
        let bytes = bytes::Bytes::from(bytes);
        let mut parser = octseq::Parser::from_ref(&bytes);
        OldRecord::parse(&mut parser).unwrap().unwrap()
    }
}

//----------- RegularRecord ----------------------------------------------------

/// A regular record.
#[derive(Clone, Debug)]
pub struct RegularRecord(pub Record<Box<RevName>, BoxedRecordData>);

impl PartialEq for RegularRecord {
    fn eq(&self, other: &Self) -> bool {
        (&self.rname, self.rtype, self.ttl) == (&other.rname, other.rtype, other.ttl)
            && self.rdata.cmp_canonical(&other.rdata).is_eq()
    }
}

impl Eq for RegularRecord {}

impl PartialOrd for RegularRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RegularRecord {
    fn cmp(&self, other: &Self) -> Ordering {
        (&self.rname, self.rtype, self.ttl)
            .cmp(&(&other.rname, other.rtype, other.ttl))
            .then_with(|| self.rdata.cmp_canonical(&other.rdata))
    }
}

impl Deref for RegularRecord {
    type Target = Record<Box<RevName>, BoxedRecordData>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RegularRecord {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<OldRecord> for RegularRecord {
    fn from(value: OldRecord) -> Self {
        let mut bytes = Vec::new();
        value.compose(&mut bytes).unwrap();
        let record = Record::parse_bytes(&bytes)
            .expect("'Record' serializes records correctly")
            .transform(|name: RevNameBuf| name.unsized_copy_into(), |data| data);
        RegularRecord(record)
    }
}

impl From<RegularRecord> for OldRecord {
    fn from(value: RegularRecord) -> Self {
        let mut bytes = vec![0u8; value.0.built_bytes_size()];
        value.0.build_bytes(&mut bytes).unwrap();
        let bytes = bytes::Bytes::from(bytes);
        let mut parser = octseq::Parser::from_ref(&bytes);
        OldRecord::parse(&mut parser).unwrap().unwrap()
    }
}

//----------- merge() ----------------------------------------------------------

/// Merge sorted lists.
fn merge<T: Ord, I: IntoIterator<Item = T>, const N: usize>(
    iters: [I; N],
) -> impl Iterator<Item = [Option<T>; N]> {
    struct Merge<T: Ord, I: Iterator<Item = T>, const N: usize>([Peekable<I>; N]);

    impl<T: Ord, I: Iterator<Item = T>, const N: usize> Iterator for Merge<T, I, N> {
        type Item = [Option<T>; N];

        fn next(&mut self) -> Option<Self::Item> {
            let set = self.0.each_mut().map(|e| e.peek());
            let min = set.iter().cloned().flatten().min()?;
            let used = set.map(|e| e == Some(min));
            let mut index = 0usize;
            Some(self.0.each_mut().map(|i| {
                let used = used[index];
                index += 1;
                i.next_if(|_| used)
            }))
        }
    }

    Merge(iters.map(|i| i.into_iter().peekable()))
}

//============ Errors ==========================================================

//----------- ForwardError -----------------------------------------------------

/// An error when forwarding a zone using a [`Compressed`].
#[derive(Debug)]
pub enum ForwardError {
    /// The [`Compressed`] covers a different zone.
    MismatchedZones,

    /// The [`Compressed`] copy is of a different version of the zone.
    MismatchedVersions,

    /// The [`Compressed`] does not have the right contents.
    Inconsistent,
}

impl std::error::Error for ForwardError {}

impl fmt::Display for ForwardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForwardError::MismatchedZones => write!(
                f,
                "the uncompressed contents and the diff belong to different zones"
            ),
            ForwardError::MismatchedVersions => {
                write!(f, "the diff applies to a different zone serial")
            }
            ForwardError::Inconsistent => {
                write!(f, "the diff expects different records in the zone")
            }
        }
    }
}

//----------- MergeError -------------------------------------------------------

/// An error when merging consecutive [`Compressed`] zones.
#[derive(Debug)]
pub enum MergeError {
    /// The two versions cover different zones.
    MismatchedZones,

    /// The two versions are not adjacent.
    NotAdjacentVersions,

    /// The resulting SOA serial difference would overflow.
    SerialDiffOverflow,

    /// The SOA records do not line up correctly.
    MismatchedSoa,

    /// The zones are inconsistent with each other.
    Inconsistent,
}

impl std::error::Error for MergeError {}

impl fmt::Display for MergeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MismatchedZones => write!(f, "the diffs belong to different zones"),
            Self::NotAdjacentVersions => write!(f, "the diffs have incompatible serials"),
            Self::SerialDiffOverflow => write!(f, "merging these diffs would overflow the serial"),
            Self::MismatchedSoa | Self::Inconsistent => {
                write!(f, "the diffs expected different records of each other")
            }
        }
    }
}
