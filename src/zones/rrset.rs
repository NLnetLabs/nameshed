
use std::ops;
use std::sync::Arc;
use bytes::Bytes;
use domain::base::iana::Rtype;
use domain::base::name::Dname;
use domain::base::record::Record;
use domain::base::rdata::RecordData;
use domain::rdata::ZoneRecordData;
use serde::{Deserialize, Serialize};

//------------ Type Aliases --------------------------------------------------

pub type StoredDname = Dname<Bytes>;
pub type StoredRecordData = ZoneRecordData<Bytes, StoredDname>;
pub type StoredRecord = Record<StoredDname, StoredRecordData>;


//------------ SharedRr ------------------------------------------------------

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharedRr {
    ttl: u32,
    data: StoredRecordData,
}

impl SharedRr {
    pub fn new(ttl: u32, data: StoredRecordData) -> Self {
        SharedRr { ttl, data }
    }

    pub fn rtype(&self) -> Rtype {
        self.data.rtype()
    }

    pub fn ttl(&self) -> u32 {
        self.ttl
    }

    pub fn data(&self) -> &StoredRecordData {
        &self.data
    }
}


//------------ Rrset ---------------------------------------------------------

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Rrset {
    rtype: Rtype,
    ttl: u32,
    data: Vec<StoredRecordData>,
    additional: Vec<StoredRecord>,
}

impl Rrset {
    /*
    fn is_delegation(rtype: Rtype) -> bool {
        matches!(rtype, Rtype::Ns | Rtype::Ds)
    }
    */

    pub fn new(rtype: Rtype, ttl: u32) -> Self {
        Rrset {
            rtype,
            ttl,
            data: Vec::new(),
            additional: Vec::new(),
        }
    }

    pub fn rtype(&self) -> Rtype {
        self.rtype
    }

    pub fn ttl(&self) -> u32 {
        self.ttl
    }

    pub fn data(&self) -> &[StoredRecordData] {
        &self.data
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn first(&self) -> Option<SharedRr> {
        self.data.first().map(|data| {
            SharedRr { ttl: self.ttl, data: data.clone() }
        })
    }

    pub fn additional(&self) -> &[StoredRecord] {
        &self.additional
    }

    pub fn set_ttl(&mut self, ttl: u32) {
        self.ttl = ttl;
    }

    pub fn push_data(&mut self, data: StoredRecordData) {
        assert_eq!(data.rtype(), self.rtype);
        self.data.push(data);
    }

    pub fn push_additional(&mut self, record: StoredRecord) {
        self.additional.push(record);
    }

    pub fn shared(self) -> SharedRrset {
        SharedRrset::new(self)
    }
}


//------------ SharedRrset ---------------------------------------------------

/// An RRset behind an arc.
#[derive(Clone, Eq, PartialEq)]
pub struct SharedRrset(Arc<Rrset>);

impl SharedRrset {
    pub fn new(rrset: Rrset) -> Self {
        SharedRrset(Arc::new(rrset))
    }

    pub fn as_rrset(&self) -> &Rrset {
        self.0.as_ref()
    }
}


//--- Deref, AsRef, Borrow

impl ops::Deref for SharedRrset {
    type Target = Rrset;

    fn deref(&self) -> &Self::Target {
        self.as_rrset()
    }
}

impl AsRef<Rrset> for SharedRrset {
    fn as_ref(&self) -> &Rrset {
        self.as_rrset()
    }
}


//--- Deserialize and Serialize

impl<'de> Deserialize<'de> for SharedRrset {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D
    ) -> Result<Self, D::Error> {
        Rrset::deserialize(deserializer).map(SharedRrset::new)
    }
}

impl Serialize for SharedRrset {
    fn serialize<S: serde::Serializer>(
        &self, serializer: S
    ) -> Result<S::Ok, S::Error> {
        self.as_rrset().serialize(serializer)
    }
}

