//! The nodes in a zone tree.

use std::collections::HashMap;
use std::sync::Arc;
use domain::base::iana::Rtype;
use domain::base::name::{Label, OwnedLabel, ToDname, ToLabelIter};
use parking_lot::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
use super::flavor::Flavor;
use super::rrset::{SharedRr, SharedRrset, StoredDname};
use super::versioned::{FlavorVersioned, Version};


//------------ ZoneApex ------------------------------------------------------

pub struct ZoneApex {
    apex_name: StoredDname,
    rrsets: NodeRrsets,
    children: NodeChildren,
}

impl ZoneApex {
    /// Creates a new apex.
    pub fn new(apex_name: StoredDname) -> Self {
        ZoneApex {
            apex_name,
            rrsets: Default::default(),
            children: Default::default(),
        }
    }

    /// Returns the apex name of the zone.
    pub fn apex_name(&self) -> &StoredDname {
        &self.apex_name
    }

    pub fn prepare_name<'l>(
        &self, qname: &'l impl ToDname
    ) -> impl Iterator<Item=&'l Label> {
        let mut qname = qname.iter_labels().rev();
        for apex_label in self.apex_name().iter_labels().rev() {
            let qname_label = qname.next();
            assert_eq!(
                Some(apex_label), qname_label,
                "dispatched query to the wrong zone"
            );
        }
        qname
    }

    /// Returns the RRsets of this node.
    pub fn rrsets(&self) -> &NodeRrsets {
        &self.rrsets
    }

    /// Returns the SOA record for the given flavor and version if available.
    pub fn get_soa(
        &self, flavor: Option<Flavor>, version: Version
    ) -> Option<SharedRr> {
        self.rrsets().get(Rtype::Soa, flavor, version)
            .and_then(|rrset| rrset.first())
    }

    /// Returns the children.
    pub fn children(&self) -> &NodeChildren {
        &self.children
    }

    pub fn rollback(&self, version: Version) {
        self.rrsets.rollback(version);
        self.children.rollback(version);
    }

    pub fn clean(&self, version: Version) {
        self.rrsets.clean(version);
        self.children.clean(version);
    }
}


//------------ ZoneNode ------------------------------------------------------

#[derive(Default)]
pub struct ZoneNode {
    /// The RRsets of the node.
    rrsets: NodeRrsets,

    /// The special functions of the node for the various flavors.
    special: RwLock<FlavorVersioned<Option<Special>>>,

    /// The child nodes of the node.
    children: NodeChildren,
}

impl ZoneNode {
    /// Returns the RRsets of this node.
    pub fn rrsets(&self) -> &NodeRrsets {
        &self.rrsets
    }

    /// Returns whether the node is NXDomain for a flavor.
    pub fn is_nx_domain(
        &self, flavor: Option<Flavor>, version: Version
    ) -> bool {
        self.with_special(flavor, version, |special| {
            matches!(special, Some(Special::NxDomain))
        })
    }

    pub fn with_special<R>(
        &self,
        flavor: Option<Flavor>, version: Version,
        op: impl FnOnce(Option<&Special>) -> R
    ) -> R {
        op(
            self.special.read().get(flavor, version)
                .map(Option::as_ref).flatten()
        )
    }

    /// Updates the special.
    pub fn update_special(
        &self,
        flavor: Option<Flavor>, version: Version,
        special: Option<Special>,
    ) {
        self.special.write().update(flavor, version, special)
    }

    /// Returns the children.
    pub fn children(&self) -> &NodeChildren {
        &self.children
    }

    pub fn rollback(&self, version: Version) {
        self.rrsets.rollback(version);
        self.special.write().rollback(version);
        self.children.rollback(version);
    }

    pub fn clean(&self, version: Version) {
        self.rrsets.clean(version);
        self.special.write().clean(version);
        self.children.clean(version);
    }
}


//------------ NodeRrsets ----------------------------------------------------

#[derive(Default)]
pub struct NodeRrsets {
    rrsets: RwLock<HashMap<Rtype, NodeRrset>>,
}

impl NodeRrsets {
    /// Returns whether there are no RRsets for the given flavor.
    pub fn is_empty(&self, flavor: Option<Flavor>, version: Version) -> bool {
        let rrsets = self.rrsets.read();
        if rrsets.is_empty() {
            return true
        }
        for value in self.rrsets.read().values() {
            if value.get(flavor, version).is_some() {
                return false
            }
        }
        true
    }

    /// Returns the RRset for a given record type.
    pub fn get(
        &self, rtype: Rtype, flavor: Option<Flavor>, version: Version
    ) -> Option<SharedRrset> {
        self.rrsets.read().get(&rtype)
            .and_then(|rrsets| rrsets.get(flavor, version))
            .cloned()
    }

    /// Updates an RRset.
    pub fn update(
        &self, rrset: SharedRrset, flavor: Option<Flavor>, version: Version
    ) {
        self.rrsets.write()
            .entry(rrset.rtype()).or_default()
            .update(rrset, flavor, version)
    }

    /// Removes the RRset for the given type.
    pub fn remove(
        &self, rtype: Rtype, flavor: Option<Flavor>, version: Version
    ) {
        self.rrsets.write()
            .entry(rtype).or_default()
            .remove(flavor, version)
    }

    pub fn rollback(&self, version: Version) {
        self.rrsets.write().values_mut().for_each(|rrset| {
            rrset.rollback(version)
        });
    }

    pub fn clean(&self, version: Version) {
        self.rrsets.write().values_mut().for_each(|rrset| {
            rrset.clean(version)
        });
    }
}


//------------ NodeRrset -----------------------------------------------------

#[derive(Default)]
struct NodeRrset {
    /// The RRsets for the various flavors.
    ///
    /// A stored `None` value means there is explicitely no RRset here. This
    /// is used to signal that a flavor doesn’t have an RRset if a default is
    /// present.
    rrsets: FlavorVersioned<Option<SharedRrset>>,
}

impl NodeRrset {
    fn get(
        &self, flavor: Option<Flavor>, version: Version
    ) -> Option<&SharedRrset> {
        self.rrsets.get(flavor, version).map(Option::as_ref).flatten()
    }

    fn update(
        &mut self, rrset: SharedRrset, flavor: Option<Flavor>, version: Version
    ) {
        self.rrsets.update(flavor, version, Some(rrset))
    }

    fn remove(&mut self, flavor: Option<Flavor>, version: Version) {
        self.rrsets.update(flavor, version, None)
    }

    pub fn rollback(&mut self, version: Version) {
        self.rrsets.rollback(version);
    }

    pub fn clean(&mut self, version: Version) {
        self.rrsets.rollback(version);
    }
}


//------------ Special -------------------------------------------------------

#[derive(Clone)]
pub enum Special {
    Cut(Arc<ZoneCut>),
    Cname(SharedRr),
    NxDomain,
}


//------------ ZoneCut -------------------------------------------------------

pub struct ZoneCut {
    pub name: StoredDname,
    pub ns: SharedRrset,
    pub ds: Option<SharedRrset>,
}


//------------ NodeChildren --------------------------------------------------

#[derive(Default)]
pub struct NodeChildren {
    children: RwLock<HashMap<OwnedLabel, Arc<ZoneNode>>>
}

impl NodeChildren {
    pub fn with<R>(
        &self,
        label: &Label,
        op: impl FnOnce(Option<&Arc<ZoneNode>>) -> R
    ) -> R {
        op(self.children.read().get(label))
    }

    /// Executes a closure for a child, creating a new one of necessary.
    ///
    /// The closure receives a references to the node and a boolean
    /// expressing whether the child was created.
    pub fn with_or_default<R>(
        &self,
        label: &Label,
        op: impl FnOnce(&Arc<ZoneNode>, bool) -> R
    ) -> R {
        let lock = self.children.upgradable_read();
        if let Some(node) = lock.get(label) {
            return op(node, false)
        }
        let mut lock = RwLockUpgradableReadGuard::upgrade(lock);
        lock.insert(label.into(), Default::default());
        let lock = RwLockWriteGuard::downgrade(lock);
        op(lock.get(label).unwrap(), true)
    }

    fn rollback(&self, version: Version) {
        self.children.read().values().for_each(|item| item.rollback(version))
    }

    fn clean(&self, version: Version) {
        self.children.read().values().for_each(|item| item.clean(version))
    }
}

