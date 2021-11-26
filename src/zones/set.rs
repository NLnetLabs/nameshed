//! The known set of zones.

use std::ops::{Deref, DerefMut};
use std::collections::HashMap;
use std::sync::Arc;
use domain::base::iana::Class;
use domain::base::name::{Label, OwnedLabel, ToDname, ToLabelIter};
use tokio::sync::RwLock;
use super::zone::Zone;


//------------ ZoneSet -------------------------------------------------------

/// The set of zones we are authoritative for.
#[derive(Default)]
pub struct ZoneSet {
    roots: Roots,
}

impl ZoneSet {
    pub fn get_zone(
        &self,
        apex_name: &impl ToDname,
        class: Class,
    ) -> Option<&Zone> {
        self.roots.get(class)?.get_zone(apex_name.iter_labels().rev())
    }

    pub fn insert_zone(
        &mut self,
        class: Class,
        zone: Zone
    ) -> Result<(), ZoneExists> {
        self.roots.get_or_insert(class).insert_zone(
            &mut zone.apex_name().clone().iter_labels().rev(), zone
        )
    }

    pub fn find_zone<'l>(
        &self,
        qname: &impl ToDname,
        class: Class
    ) -> Option<&Zone> {
        self.roots.get(class)?.find_zone(qname.iter_labels().rev())
    }
}


//------------ SharedZoneSet -------------------------------------------------

/// The set of zones we are authoritative for.
#[derive(Clone, Default)]
pub struct SharedZoneSet {
    zones: Arc<RwLock<ZoneSet>>,
}

impl SharedZoneSet {
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
    ) -> Result<(), ZoneExists> {
        if let Some(label) = apex_name.next() {
            self.children.entry(label.into()).or_default()
                .insert_zone(apex_name, zone)
        }
        else if self.zone.is_some() {
            Err(ZoneExists)
        }
        else {
            self.zone = Some(zone);
            Ok(())
        }
    }
}


//============ Error Types ===================================================

#[derive(Clone, Copy, Debug)]
pub struct ZoneExists;


