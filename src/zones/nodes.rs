//! The nodes in a zone tree.

use std::io;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use domain::base::iana::{Class, Rtype};
use domain::base::name::{Label, OwnedLabel, ToDname, ToLabelIter};
use parking_lot::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
use serde::{Deserialize, Serialize};
use crate::store::{ReadData, ReplaceData};
use super::flavor::Flavor;
use super::rrset::{SharedRr, SharedRrset, StoredDname, StoredRecord};
use super::versioned::{FlavorVersioned, Version};


//------------ ZoneApex ------------------------------------------------------

pub struct ZoneApex {
    apex_name: StoredDname,
    apex_name_display: String,
    class: Class,
    rrsets: NodeRrsets,
    children: NodeChildren,
}

impl ZoneApex {
    /// Creates a new apex.
    pub fn new(apex_name: StoredDname, class: Class) -> Self {
        ZoneApex {
            apex_name_display: format!("{}", apex_name),
            apex_name,
            class,
            rrsets: Default::default(),
            children: Default::default(),
        }
    }

    /// Loads the tree from a snapshot
    pub(super) fn load_snapshot(
        &mut self, store: &mut ReadData,
    ) -> Result<(), io::Error> {
        let zone_data: stored::ZoneSnapshot = store.deserialize()?;
        if zone_data.apex_name != self.apex_name
            || zone_data.class != self.class
        {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "corrupt database: zone apex for {} {} does not match",
                    self.apex_name, self.class
                )
            ))
        }

        self.rrsets = NodeRrsets::from_snapshot(store)?;
        self.children = NodeChildren::from_snapshot(store)?;
        Ok(())
    }

    /// Returns the apex name of the zone.
    pub fn apex_name(&self) -> &StoredDname {
        &self.apex_name
    }

    /// Returns the string version of the apex name.
    pub fn apex_name_display(&self) -> &str {
        &self.apex_name_display
    }

    /// Returns the class of the zone.
    pub fn class(&self) -> Class {
        self.class
    }

    /// Returns the class name.
    pub fn display_class(&self) -> Cow<str> {
        match self.class() {
            Class::In => Cow::Borrowed("IN"),
            class => Cow::Owned(class.to_string())
        }
    }

    pub fn prepare_name<'l>(
        &self, qname: &'l impl ToDname
    ) -> Result<impl Iterator<Item=&'l Label>, OutOfZone> {
        let mut qname = qname.iter_labels().rev();
        for apex_label in self.apex_name().iter_labels().rev() {
            let qname_label = qname.next();
            if Some(apex_label) != qname_label {
                return Err(OutOfZone)
            }
        }
        Ok(qname)
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

    pub fn snapshot(
        &self, store: &mut ReplaceData, version: Version
    ) -> Result<(), io::Error> {
        store.serialize(
            stored::ZoneSnapshot {
                apex_name: self.apex_name.clone(),
                class: self.class
            }
        )?;
        self.rrsets.snapshot(store, version)?;
        self.children.snapshot(store, version)?;
        Ok(())
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
                .and_then(Option::as_ref)
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

    fn snapshot(
        &self, store: &mut ReplaceData, version: Version
    ) -> Result<(), io::Error> {
        store.serialize(stored::NodeSnapshot)?;
        self.rrsets.snapshot(store, version)?;
        self.snapshot_special(store, version)?;
        self.children.snapshot(store, version)
    }

    fn from_snapshot(
        store: &mut ReadData
    ) -> Result<Self, io::Error> {
        let _ = store.deserialize::<stored::NodeSnapshot>()?;
        Ok(ZoneNode {
            rrsets: NodeRrsets::from_snapshot(store)?,
            special: Self::special_from_snapshot(store)?,
            children: NodeChildren::from_snapshot(store)?,
        })
    }

    fn snapshot_special(
        &self, store: &mut ReplaceData, version: Version
    ) -> Result<(), io::Error> {
        use stored::SpecialSnapshot::*;

        for (flavor, special) in self.special.read().iter_version(version) {
            store.serialize(match *special {
                None => Regular { flavor },
                Some(Special::NxDomain) => NxDomain { flavor },
                Some(Special::Cut(ref cut)) => {
                    ZoneCut { flavor, cut: cut.clone() }
                }
                Some(Special::Cname(ref cname)) => {
                    Cname { flavor, cname: cname.clone() }
                }
            })?;
        }
        Ok(())
    }

    fn special_from_snapshot(
        store: &mut ReadData
    ) -> Result<RwLock<FlavorVersioned<Option<Special>>>, io::Error> {
        let mut special = FlavorVersioned::<Option<Special>>::new();
        loop {
            match store.deserialize()? {
                stored::SpecialSnapshot::Regular { flavor } => {
                    special.update(flavor, Version::default(), None);
                }
                stored::SpecialSnapshot::NxDomain { flavor } => {
                    special.update(
                        flavor, Version::default(), Some(Special::NxDomain)
                    );
                }
                stored::SpecialSnapshot::ZoneCut { flavor, cut } => {
                    special.update(
                        flavor, Version::default(), Some(Special::Cut(cut))
                    );
                }
                stored::SpecialSnapshot::Cname { flavor, cname } => {
                    special.update(
                        flavor, Version::default(), Some(Special::Cname(cname))
                    );
                }
                stored::SpecialSnapshot::Done => {
                    break;
                }
            }
        }
        Ok(RwLock::new(special))
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

    fn snapshot(
        &self, store: &mut ReplaceData, version: Version
    ) -> Result<(), io::Error> {
        for (&rtype, rrset) in self.rrsets.read().iter() {
            rrset.snapshot(rtype, store, version)?;
        }
        Ok(())
    }

    fn from_snapshot(
        store: &mut ReadData
    ) -> Result<Self, io::Error> {
        let mut rrsets = HashMap::<Rtype, NodeRrset>::new();
        while let
            stored::RrsetSnapshot::Rrset { rtype, flavor, rrset }
                = store.deserialize()?
        {
            rrsets.entry(rtype).or_default().rrsets.update(
                flavor, Version::default(), rrset
            );
        }
        Ok(NodeRrsets { rrsets: RwLock::new(rrsets) })
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
        self.rrsets.get(flavor, version).and_then(Option::as_ref)
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

    fn snapshot(
        &self, rtype: Rtype, store: &mut ReplaceData, version: Version
    ) -> Result<(), io::Error> {
        for (flavor, rrset) in self.rrsets.iter_version(version) {
            store.serialize(
                stored::RrsetSnapshot::Rrset {
                    rtype,
                    flavor,
                    rrset: rrset.clone()
                }
            )?;
        }
        store.serialize(stored::RrsetSnapshot::Done)
    }
}


//------------ Special -------------------------------------------------------

#[derive(Clone)]
pub enum Special {
    Cut(ZoneCut),
    Cname(SharedRr),
    NxDomain,
}


//------------ ZoneCut -------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ZoneCut {
    pub name: StoredDname,
    pub ns: SharedRrset,
    pub ds: Option<SharedRrset>,
    pub glue: Vec<StoredRecord>,
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

    fn snapshot(
        &self, store: &mut ReplaceData, version: Version
    ) -> Result<(), io::Error> {
        for (label, node) in self.children.read().iter() {
            store.serialize(
                stored::ChildSnapshot::Node {
                    label: *label
                }
            )?;
            node.snapshot(store, version)?;
        }
        store.serialize(stored::ChildSnapshot::Done)
    }

    fn from_snapshot(
        store: &mut ReadData
    ) -> Result<Self, io::Error> {
        let mut children = HashMap::<OwnedLabel, Arc<ZoneNode>>::new();
        while let stored::ChildSnapshot::Node { label }
            = store.deserialize()?
        {
            children.insert(
                label,
                Arc::new(ZoneNode::from_snapshot(store)?)
            );
        }
        Ok(NodeChildren { children: RwLock::new(children) })
    }
}


//============ Error Types ==================================================

/// A domain name is not under the zone’s apex.
#[derive(Clone, Copy, Debug)]
pub struct OutOfZone;


//============ Stored Data ===================================================

mod stored {
    use serde::{Deserialize, Serialize};
    use super::*;

    /// The start of a snapshot of a zone.
    ///
    /// This struct is followed by a series of `RrsetSnapshot` followed by a
    /// series of `NodeSnapshot`.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct ZoneSnapshot {
        pub apex_name: StoredDname,
        pub class: Class,
    }

    /// Snapshot of an Rrset or the end of the list of rrsets.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub enum RrsetSnapshot {
        Rrset {
            rtype: Rtype,
            flavor: Option<Flavor>,
            rrset: Option<SharedRrset>,
        },
        Done
    }

    /// Snapshof of an child node or end of the list of children.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub enum ChildSnapshot {
        /// The snapshot of child node with the given label follows.
        ///
        /// This value is followed by a sequence starting with  `NodeSnapshot`.
        Node {
            label: OwnedLabel,
        },
        Done
    }

    /// Snapshot of a node.
    ///
    /// This is followed by a series of `RrsetSnapshot` followed by a series of
    /// `SpecialSnapshot` followed by a series of `ChildSnapshot`.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct NodeSnapshot;

    /// Snapshot of the specials of a node.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub enum SpecialSnapshot {
        Regular { flavor: Option<Flavor> },
        NxDomain { flavor: Option<Flavor> },
        ZoneCut { flavor: Option<Flavor>, cut: ZoneCut, },
        Cname { flavor: Option<Flavor>, cname: SharedRr, },
        Done,
    }
}

