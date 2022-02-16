//! Write access to zones.

use std::sync::Arc;
use domain::base::iana::Rtype;
use domain::base::name::{Label, ToDname};
use parking_lot::RwLock;
use tokio::sync::OwnedMutexGuard;
use super::flavor::Flavor;
use super::nodes::{OutOfZone, Special, ZoneApex, ZoneCut, ZoneNode};
use super::rrset::{SharedRr, SharedRrset};
use super::versioned::Version;
use super::zone::ZoneVersions;


//------------ WriteZone -----------------------------------------------------

pub struct WriteZone {
    apex: Arc<ZoneApex>,
    _lock: OwnedMutexGuard<()>,
    version: Version,
    zone_versions: Arc<RwLock<ZoneVersions>>,
}

impl WriteZone {
    pub(super) fn new(
        apex: Arc<ZoneApex>,
        _lock: OwnedMutexGuard<()>,
        version: Version,
        zone_versions: Arc<RwLock<ZoneVersions>>
    ) -> Self {
        WriteZone { apex, _lock, version, zone_versions }
    }

    pub fn apex(&mut self) -> WriteNode {
        WriteNode::new(self, None)
    }

    pub fn commit(&mut self) {
        // The order here is important so we don’t accidentally remove the
        // newly created version right away.
        let marker = self.zone_versions.write().update_current(self.version);
        if let Some(version) = self.zone_versions.write().clean_versions() {
            self.apex.clean(version)
        }
        self.zone_versions.write().push_version(self.version, marker);
        
        // Start the next version.
        self.version = self.version.next();
    }
}

/// # Convenience Methods
///
impl WriteZone {
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
    ) -> Result<(), OutOfZone> {
        let name = self.apex.prepare_name(name)?;
        let mut node = self.apex();
        for label in name {
            node = node.into_child_or_default(label);
        }
        node.update_rrset(rrset, flavor);
        Ok(())
    }
}

impl Drop for WriteZone {
    fn drop(&mut self) {
        self.apex.rollback(self.version)
    }
}


//------------ WriteNode ------------------------------------------------------

pub struct WriteNode<'a> {
    /// The zone we are updating.
    zone: &'a mut WriteZone,

    /// The node we are updating.
    ///
    /// If this is `None` we are updating the apex and can use
    /// `self.zone.apex`.
    node: Option<Arc<ZoneNode>>,
}

impl<'a> WriteNode<'a> {
    fn new(zone: &'a mut WriteZone, node: Option<Arc<ZoneNode>>) -> Self {
        WriteNode { zone, node }
    }

    pub fn into_child_or_default(
        self, label: &Label
    ) -> Self {
        let children = match self.node.as_ref() {
            Some(node) => node.children(),
            None => self.zone.apex.children(),
        };
        let (node, created) = children.with_or_default(label, |node, created| {
            (node.clone(), created)
        });
        let mut res = Self::new(self.zone, Some(node));
        if created {
            res.make_regular(None)
        }
        res
    }

    pub fn update_rrset(
        &mut self, rrset: SharedRrset, flavor: Option<Flavor>, 
    ) {
        let rrsets = match self.node.as_ref() {
            Some(node) => node.rrsets(),
            None => self.zone.apex.rrsets(),
        };
        rrsets.update(rrset, flavor, self.zone.version);
        self.check_nx_domain(flavor);
    }

    pub fn remove_rrset(
        &mut self, rtype: Rtype, flavor: Option<Flavor>,
    ) {
        let rrsets = match self.node.as_ref() {
            Some(node) => node.rrsets(),
            None => self.zone.apex.rrsets(),
        };
        rrsets.remove(rtype, flavor, self.zone.version);
        self.check_nx_domain(flavor);
    }

    pub fn make_regular(&mut self, flavor: Option<Flavor>) {
        if let Some(node) = self.node.as_ref() {
            node.update_special(flavor, self.zone.version, None);
            self.check_nx_domain(flavor);
        }
    }

    pub fn make_zone_cut(
        &mut self, cut: ZoneCut, flavor: Option<Flavor>
    ) -> Result<(), WriteApexError> {
        self.node.as_ref().ok_or(WriteApexError)?.update_special(
            flavor, self.zone.version, Some(Special::Cut(cut.into()))
        );
        Ok(())
    }

    pub fn make_cname(
        &mut self, cname: SharedRr, flavor: Option<Flavor>
    ) -> Result<(), WriteApexError> {
        self.node.as_ref().ok_or(WriteApexError)?.update_special(
            flavor, self.zone.version, Some(Special::Cname(cname))
        );
        Ok(())
    }

    /// Makes sure a NXDomain special is set or removed as necesssary.
    fn check_nx_domain(&self, flavor: Option<Flavor>) {
        let node = match self.node.as_ref() {
            Some(node) => node,
            None => return
        };
        let opt_new_special = node.with_special(
            flavor, self.zone.version,
            |special| {
                match special {
                    Some(Special::NxDomain) => {
                        if !node.rrsets().is_empty(flavor, self.zone.version) {
                            Some(None)
                        }
                        else {
                            None
                        }
                    }
                    None => {
                        if node.rrsets().is_empty(flavor, self.zone.version) {
                            Some(Some(Special::NxDomain))
                        }
                        else {
                            None
                        }
                    }
                    _ => None
                }
            }
        );
        if let Some(new_special) = opt_new_special {
            node.update_special(flavor, self.zone.version, new_special)
        }
    }
}


//------------ WriteApexError ------------------------------------------------

/// The requested operation is not allowed at the apex of a zone.
#[derive(Clone, Copy, Debug)]
pub struct WriteApexError;

