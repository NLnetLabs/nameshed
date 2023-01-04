//! The known set of zones.

use std::io;
use std::collections::hash_map;
use std::ops::{Deref, DerefMut};
use std::collections::HashMap;
use std::sync::Arc;
use domain::base::iana::Class;
use domain::base::name::{Label, OwnedLabel, ToDname, ToLabelIter};
use tokio::sync::RwLock;
use crate::store::Store;
use super::zone::Zone;


//------------ ZoneSet -------------------------------------------------------

/// The set of zones we are authoritative for.
pub struct ZoneSet {
    roots: Roots,
    store: Store,
}

impl ZoneSet {
    fn new(store: Store) -> Self {
        ZoneSet {
            roots: Default::default(),
            store,
        }
    }

    async fn load(&mut self) -> Result<(), io::Error> {
        let zones: stored::ZoneList =
            stored::area(&self.store)?.read_data().await?.deserialize()?;
        for (apex_name, class) in zones.zones {
            let zone = Zone::load(&self.store, apex_name, class).await?;
            self.insert_zone_only(zone)?;
        }
        Ok(())
    }

    pub fn get_zone(
        &self,
        apex_name: &impl ToDname,
        class: Class,
    ) -> Option<&Zone> {
        self.roots.get(class)?.get_zone(apex_name.iter_labels().rev())
    }

    pub async fn insert_zone(
        &mut self,
        zone: Zone,
    ) -> Result<(), InsertZoneError> {
        self.insert_zone_only(zone)?;
        self.update_zone_list().await?;
        Ok(())
    }

    fn insert_zone_only(
        &mut self,
        zone: Zone,
    ) -> Result<(), InsertZoneError> {
        self.roots.get_or_insert(zone.apex().class()).insert_zone(
            &mut zone.apex_name().clone().iter_labels().rev(), zone
        )
    }

    async fn update_zone_list(&self) -> Result<(), io::Error> {
        let mut store = stored::area(&self.store)?.replace_data().await?;
        store.serialize(
            stored::ZoneList {
                zones: self.iter_zones().map(|zone| {
                    (zone.apex_name().clone(), zone.class())
                }).collect()
            }
        )?;
        store.commit()
    }

    pub fn find_zone(
        &self,
        qname: &impl ToDname,
        class: Class
    ) -> Option<&Zone> {
        self.roots.get(class)?.find_zone(qname.iter_labels().rev())
    }

    pub fn iter_zones(&self) -> ZoneSetIter {
        ZoneSetIter::new(self)
    }
}


//------------ SharedZoneSet -------------------------------------------------

/// The set of zones we are authoritative for.
#[derive(Clone)]
pub struct SharedZoneSet {
    zones: Arc<RwLock<ZoneSet>>,
}

impl SharedZoneSet {
    pub fn new(store: Store) -> Self {
        SharedZoneSet {
            zones: Arc::new(RwLock::new(ZoneSet::new(store)))
        }
    }

    pub async fn init(store: Store) -> Result<Self, io::Error> {
        let zones = ZoneSet::new(store);
        zones.update_zone_list().await?;
        Ok(SharedZoneSet {
            zones: Arc::new(RwLock::new(zones))
        })
    }

    pub async fn load(store: Store) -> Result<Self, io::Error> {
        let mut zones = ZoneSet::new(store);
        zones.load().await?;
        Ok(SharedZoneSet {
            zones: Arc::new(RwLock::new(zones))
        })
    }

    pub async fn read(&self) -> impl Deref<Target = ZoneSet> + '_ {
        self.zones.read().await
    }

    pub async fn write(&self) -> impl DerefMut<Target = ZoneSet> + '_ {
        self.zones.write().await
    }
}


//------------ Roots ---------------------------------------------------------

#[derive(Default)]
struct Roots {
    in_: ZoneSetNode,
    others: HashMap<Class, ZoneSetNode>,
}

impl Roots {
    pub fn get(&self, class: Class) -> Option<&ZoneSetNode> {
        if class == Class::In {
            Some(&self.in_)
        }
        else {
            self.others.get(&class)
        }
    }

    pub fn get_or_insert(&mut self, class: Class) -> &mut ZoneSetNode {
        if class == Class::In {
            &mut self.in_
        }
        else {
            self.others.entry(class).or_default()
        }
    }
}


//------------ ZoneSetNode ---------------------------------------------------

#[derive(Default)]
struct ZoneSetNode {
    zone: Option<Zone>,
    children: HashMap<OwnedLabel, ZoneSetNode>,
}

impl ZoneSetNode {
    fn get_zone<'l>(
        &self,
        mut apex_name: impl Iterator<Item = &'l Label>,
    ) -> Option<&Zone> {
        match apex_name.next() {
            Some(label) => {
                self.children.get(label)?.get_zone(apex_name)
            }
            None => {
                self.zone.as_ref()
            }
        }
    }

    pub fn find_zone<'l>(
        &self,
        mut qname: impl Iterator<Item = &'l Label>
    ) -> Option<&Zone> {
        if let Some(label) = qname.next() {
            if let Some(node) = self.children.get(label) {
                if let Some(zone) = node.find_zone(qname) {
                    return Some(zone)
                }
            }
        }
        self.zone.as_ref()
    }


    fn insert_zone<'l>(
        &mut self,
        mut apex_name: impl Iterator<Item = &'l Label>,
        zone: Zone,
    ) -> Result<(), InsertZoneError> {
        if let Some(label) = apex_name.next() {
            self.children.entry(label.into()).or_default()
                .insert_zone(apex_name, zone)
        }
        else if self.zone.is_some() {
            Err(InsertZoneError::ZoneExists)
        }
        else {
            self.zone = Some(zone);
            Ok(())
        }
    }
}


//------------ ZoneSetIter ---------------------------------------------------

pub struct ZoneSetIter<'a> {
    roots: hash_map::Values<'a, Class, ZoneSetNode>,
    nodes: NodesIter<'a>,
}

impl<'a> ZoneSetIter<'a> {
    fn new(set: &'a ZoneSet) -> Self {
        ZoneSetIter {
            roots: set.roots.others.values(),
            nodes: NodesIter::new(&set.roots.in_),
        }
    }
}

impl<'a> Iterator for ZoneSetIter<'a> {
    type Item = &'a Zone;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(node) = self.nodes.next() {
                if let Some(zone) = node.zone.as_ref() {
                    return Some(zone)
                }
                else {
                    continue
                }
            }
            self.nodes = NodesIter::new(self.roots.next()?);
        }
    }
}


//------------ NodesIter -----------------------------------------------------

struct NodesIter<'a> {
    root: Option<&'a ZoneSetNode>,
    stack: Vec<hash_map::Values<'a, OwnedLabel, ZoneSetNode>>,
}

impl<'a> NodesIter<'a> {
    fn new(node: &'a ZoneSetNode) -> Self {
        NodesIter {
            root: Some(node),
            stack: Vec::new(),
        }
    }

    fn next_node(&mut self) -> Option<&'a ZoneSetNode> {
        if let Some(node) = self.root.take() {
            return Some(node)
        }
        loop {
            if let Some(iter) = self.stack.last_mut() {
                if let Some(node) = iter.next() {
                    return Some(node)
                }
            }
            else {
                return None
            }
            let _ = self.stack.pop();
        }
    }
}

impl<'a> Iterator for NodesIter<'a> {
    type Item = &'a ZoneSetNode;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next_node()?;
        self.stack.push(node.children.values());
        Some(node)
    }
}


//============ Error Types ===================================================

#[derive(Debug)]
pub enum InsertZoneError {
    ZoneExists,
    Io(io::Error),
}

impl From<io::Error> for InsertZoneError {
    fn from(src: io::Error) -> Self {
        InsertZoneError::Io(src)
    }
}

impl From<InsertZoneError> for io::Error {
    fn from(src: InsertZoneError) -> Self {
        match src {
            InsertZoneError::Io(err) => err,
            InsertZoneError::ZoneExists => {
                io::Error::new(
                    io::ErrorKind::Other,
                    "zone exists"
                )
            }
        }
    }
}

pub struct ZoneExists; // XXX


//============ Stored Data ===================================================

mod stored {
    use std::io;
    use domain::base::iana::Class;
    use serde::{Deserialize, Serialize};
    use crate::store::{Area, Store};
    use crate::zones::StoredDname;

    pub fn area(store: &Store) -> Result<Area, io::Error> {
        store.area(["zones"])
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct ZoneList {
        pub zones: Vec<(StoredDname, Class)>,
    }
}

