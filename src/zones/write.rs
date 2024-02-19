//! Write access to zones.

use std::{fmt, io};
use std::sync::Arc;
use domain::base::iana::Rtype;
use domain::base::name::{Label, ToDname};
use futures::future::Either;
use parking_lot::RwLock;
use tokio::sync::OwnedMutexGuard;
use crate::store::{AppendData, ReadData};
use super::flavor::Flavor;
use super::nodes::{Special, ZoneApex, ZoneCut, ZoneNode};
use super::rrset::{SharedRr, SharedRrset};
use super::versioned::Version;
use super::zone::ZoneVersions;


//------------ WriteZone -----------------------------------------------------

pub struct WriteZone {
    apex: Arc<ZoneApex>,
    _lock: OwnedMutexGuard<()>,
    version: Version,
    dirty: bool,
    zone_versions: Arc<RwLock<ZoneVersions>>,
    store: Option<AppendData>
}

impl WriteZone {
    pub(super) fn new(
        apex: Arc<ZoneApex>,
        _lock: OwnedMutexGuard<()>,
        version: Version,
        zone_versions: Arc<RwLock<ZoneVersions>>,
        store: Option<AppendData>,
    ) -> Self {
        WriteZone {
            apex,
            _lock,
            version,
            dirty: false,
            zone_versions,
            store
        }
    }

    pub fn apex(
        &mut self, flavor: Option<Flavor>
    ) -> Result<WriteNode, io::Error> {
        if !self.dirty {
            if let Some(store) = self.store.as_mut() {
                store.serialize(
                    stored::ZoneUpdateStart {
                        version: self.version,
                        snapshot: false,
                    }
                )?
            }
        }
        WriteNode::new_apex(self, flavor)
    }

    pub fn commit(mut self) -> Result<(), io::Error> {
        if let Some(store) = self.store.as_mut() {
            store.serialize(stored::ZoneUpdate::Done)?;
            store.commit()?;
        }

        // The order here is important so we don’t accidentally remove the
        // newly created version right away.
        let marker = self.zone_versions.write().update_current(
            self.version
        );
        if let Some(version)
            = self.zone_versions.write().clean_versions()
        {
            self.apex.clean(version)
        }
        self.zone_versions.write().push_version(self.version, marker);
        
        // Start the next version.
        self.version = self.version.next();
        self.dirty = false;

        Ok(())
    }

    pub fn load(&mut self, store: &mut ReadData) -> Result<(), io::Error> {
        while let stored::ZoneUpdate::UpdateApex { flavor }
            = store.deserialize()?
        {
            self.apex(flavor)?.load(store)?;
        }
        Ok(())
    }
}

/// # Convenience Methods
///
impl WriteZone {
    pub fn update_node<F, R>(
        &mut self, name: &impl ToDname, flavor: Option<Flavor>,
        op: F
    ) -> Result<R, io::Error>
    where F: FnOnce(&mut WriteNode) -> Result<R, io::Error> {
        let name = self.apex.prepare_name(name).unwrap(); // XXX
        self.apex(flavor)?.update_name(name, op)
    }

    /// Update the RRset for the given name and flavour.
    ///
    /// This method adds empty nodes if necessary to get to the name. It does
    /// not check for zone cuts and NXDomain nodes along the way, so the
    /// updated RRset may not actually be visible.
    pub fn update_rrset(
        &mut self,
        name: &impl ToDname,
        rrset: SharedRrset,
        flavor: Option<Flavor>,
    ) -> Result<(), io::Error> {
        let name = self.apex.prepare_name(name).unwrap(); // XXX
        self.apex(flavor)?.update_name(name, |node| {
            node.update_rrset(rrset)
        })
    }
}

impl Drop for WriteZone {
    fn drop(&mut self) {
        if self.dirty {
            self.apex.rollback(self.version);
            self.dirty = false;
        }
    }
}


//------------ WriteNode ------------------------------------------------------

pub struct WriteNode<'a> {
    /// The writer for the zone we are working with.
    zone: &'a mut WriteZone,

    /// The node we are updating.
    node: Either<Arc<ZoneApex>, Arc<ZoneNode>>,

    /// The flavor of the node we are updating.
    flavor: Option<Flavor>,
}

impl<'a> WriteNode<'a> {
    fn new_apex(
        zone: &'a mut WriteZone,
        flavor: Option<Flavor>
    ) -> Result<Self, io::Error> {
        let apex = zone.apex.clone();
        if let Some(store) = zone.store.as_mut() {
            store.serialize(stored::ZoneUpdate::UpdateApex { flavor })?;
        }
        Ok(WriteNode {
            zone,
            node: Either::Left(apex),
            flavor,
        })
    }

    pub fn update_child(
        &mut self, label: &Label
    ) -> Result<WriteNode<'_>, io::Error> {
        let children = match self.node {
            Either::Left(ref apex) => apex.children(),
            Either::Right(ref node) => node.children(),
        };
        let (node, created) = children.with_or_default(label, |node, created| {
            (node.clone(), created)
        });
        if let Some(store) = self.zone.store.as_mut() {
            store.serialize(
                stored::NodeUpdate::UpdateChild { label: label.into() }
            )?;
        }
        let mut res = WriteNode {
            zone: self.zone,
            node: Either::Right(node),
            flavor: self.flavor
        };
        if created {
            res.make_regular()?;
        }
        Ok(res)
    }

    pub fn update_rrset(
        &mut self, rrset: SharedRrset,
    ) -> Result<(), io::Error> {
        if let Some(store) = self.zone.store.as_mut() {
            store.serialize(
                stored::NodeUpdate::UpdateRrset { rrset: rrset.clone() }
            )?;
        }
        let rrsets = match self.node {
            Either::Right(ref apex) => apex.rrsets(),
            Either::Left(ref node) => node.rrsets(),
        };
        rrsets.update(rrset, self.flavor, self.zone.version);
        self.check_nx_domain()?;
        Ok(())
    }

    pub fn remove_rrset(
        &mut self, rtype: Rtype,
    ) -> Result<(), io::Error> {
        if let Some(store) = self.zone.store.as_mut() {
            store.serialize(
                stored::NodeUpdate::RemoveRrset { rtype }
            )?;
        }
        let rrsets = match self.node {
            Either::Left(ref apex) => apex.rrsets(),
            Either::Right(ref node) => node.rrsets(),
        };
        rrsets.remove(rtype, self.flavor, self.zone.version);
        self.check_nx_domain()?;
        Ok(())
    }

    pub fn make_regular(&mut self) -> Result<(), io::Error> {
        if let Either::Right(ref node) = self.node {
            if let Some(store) = self.zone.store.as_mut() {
                store.serialize(stored::NodeUpdate::MakeRegular)?;
            }
            node.update_special(self.flavor, self.zone.version, None);
            self.check_nx_domain()?;
        }
        Ok(())
    }

    pub fn make_zone_cut(
        &mut self, cut: ZoneCut,
    ) -> Result<(), WriteApexError> {
        match self.node {
            Either::Left(_) => Err(WriteApexError::NotAllowed),
            Either::Right(ref node) => {
                if let Some(store) = self.zone.store.as_mut() {
                    store.serialize(
                        stored::NodeUpdate::MakeZoneCut { cut: cut.clone() }
                    )?;
                }
                node.update_special(
                    self.flavor, self.zone.version,
                    Some(Special::Cut(cut))
                );
                Ok(())
            }
        }
    }

    pub fn make_cname(
        &mut self, cname: SharedRr,
    ) -> Result<(), WriteApexError> {
        match self.node {
            Either::Left(_) => Err(WriteApexError::NotAllowed),
            Either::Right(ref node) => {
                if let Some(store) = self.zone.store.as_mut() {
                    store.serialize(
                        stored::NodeUpdate::MakeCname { cname: cname.clone() }
                    )?;
                }
                node.update_special(
                    self.flavor, self.zone.version,
                    Some(Special::Cname(cname))
                );
                Ok(())
            }
        }
    }

    /// Makes sure a NXDomain special is set or removed as necesssary.
    fn check_nx_domain(&mut self) -> Result<(), io::Error> {
        let node = match self.node {
            Either::Left(_) => return Ok(()),
            Either::Right(ref node) => node,
        };
        let opt_new_nxdomain = node.with_special(
            self.flavor, self.zone.version,
            |special| {
                match special {
                    Some(Special::NxDomain) => {
                        if !node.rrsets().is_empty(
                            self.flavor, self.zone.version
                        ) {
                            Some(false)
                        }
                        else {
                            None
                        }
                    }
                    None => {
                        if node.rrsets().is_empty(
                            self.flavor, self.zone.version
                        ) {
                            Some(true)
                        }
                        else {
                            None
                        }
                    }
                    _ => None
                }
            }
        );
        if let Some(new_nxdomain) = opt_new_nxdomain {
            if new_nxdomain {
                node.update_special(
                    self.flavor, self.zone.version, Some(Special::NxDomain)
                );
            }
            else {
                node.update_special(self.flavor, self.zone.version, None);
            }
        }
        Ok(())
    }

    fn update_name<'l, F, R>(
        &mut self,
        mut name: impl Iterator<Item=&'l Label>,
        op: F
    ) -> Result<R, io::Error>
    where F: FnOnce(&mut WriteNode) -> Result<R, io::Error> {
        match name.next() {
            Some(label) => {
                self.update_child(label)?
                    .update_name(name, op)
            }
            None => {
                op(self)
            }
        }
    }

    fn load(&mut self, store: &mut ReadData) -> Result<(), io::Error> {
        loop {
            match store.deserialize()? {
                stored::NodeUpdate::UpdateChild { label } => {
                    self.update_child(&label)?.load(store)?;
                }
                stored::NodeUpdate::UpdateRrset { rrset } => {
                    self.update_rrset(rrset)?;
                }
                stored::NodeUpdate::RemoveRrset { rtype } => {
                    self.remove_rrset(rtype)?;
                }
                stored::NodeUpdate::MakeRegular => {
                    self.make_regular()?;
                }
                stored::NodeUpdate::MakeZoneCut { cut } => {
                    self.make_zone_cut(cut)?;
                }
                stored::NodeUpdate::MakeCname { cname } => {
                    self.make_cname(cname)?;
                }
                stored::NodeUpdate::Done => {
                    break
                }
            }
        }
        Ok(())
    }
}

impl<'a> Drop for WriteNode<'a> {
    fn drop(&mut self) {
        if let Some(store) = self.zone.store.as_mut() {
            store.serialize_delay_err(stored::NodeUpdate::Done)
        }
    }
}


//------------ WriteApexError ------------------------------------------------

/// The requested operation is not allowed at the apex of a zone.
#[derive(Debug)]
pub enum WriteApexError {
    /// This operation is not allowed at the apex.
    NotAllowed,

    /// An IO error happened while processing the operation.
    Io(io::Error)
}

impl From<io::Error> for WriteApexError {
    fn from(src: io::Error) -> WriteApexError {
        WriteApexError::Io(src)
    }
}

impl From<WriteApexError> for io::Error {
    fn from(src: WriteApexError) -> io::Error {
        match src {
            WriteApexError::NotAllowed => {
                io::Error::new(
                    io::ErrorKind::Other,
                    "operation not allowed at apex"
                )
            }
            WriteApexError::Io(err) => err,
        }
    }
}

impl fmt::Display for WriteApexError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            WriteApexError::NotAllowed => f.write_str("operation not allowed"),
            WriteApexError::Io(ref err) => err.fmt(f)
        }
    }
}


//============ Stored Data ===================================================

mod stored {
    use domain::base::name::OwnedLabel;
    use serde::{Deserialize, Serialize};
    use super::*;

    /// Marker for the begin of an update to a zone.
    ///
    /// This event is followed by [`ZoneUpdate`] events until
    /// [`ZoneUpdate::Done`] is encountered marking the end of the update of
    /// the zone.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct ZoneUpdateStart {
        /// The version of this update.
        pub version: Version,

        /// Is this update a snapshot?
        pub snapshot: bool,
    }

    /// A single event for updating a zone.
    ///
    /// The apex and class of the zone in question are to be taken from the
    /// preceding event.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub enum ZoneUpdate {
        /// Start of an update for the zone’s nodes at the apex node.
        ///
        /// This event is followed by one or more [`NodeUpdate`] events until
        /// `[NodeUpdate::Done]` is encountered.
        UpdateApex {
            /// The flavor to be updated.
            flavor: Option<Flavor>,
        },

        /// The zone update is done.
        Done,
    }

    /// A single event for updating a zone node.
    ///
    /// The owner name of the node is to be taken from the preceding
    /// [`ZoneUpdate`] event.
    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub enum NodeUpdate {
        /// Start updating a child node.
        ///
        /// This event is followed by one or more [`NodeUpdate`] events until
        /// `[NodeUpdate::Done]` is encountered.
        UpdateChild {
            label: OwnedLabel,
        },

        /// An RRset in the node is to be added or replaced.
        UpdateRrset {
            /// The RRset that replaces the current one.
            ///
            /// This RRset is either added or replaces an already existing
            /// one with the same record type.
            rrset: SharedRrset,
        },

        /// An RRset in the node is the be removed.
        RemoveRrset {
            /// The record type of the RRset to be removed.
            rtype: Rtype,
        },

        /// The node is to be converted into a regular node.
        MakeRegular,

        /// The node is to be converted into a zone cut.
        MakeZoneCut {
            cut: ZoneCut,
        },

        /// The node is to be converted into a CNAME.
        MakeCname {
            cname: SharedRr,
        },

        /// The node update is done.
        Done,
    }
}
