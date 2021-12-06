
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use bytes::Bytes;
use domain::base::iana::{Rcode, Rtype};
use domain::base::name::{Dname, Label, OwnedLabel, ToDname, ToLabelIter};
use domain::base::record::Record;
use domain::rdata::ZoneRecordData;
use parking_lot::RwLock;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use super::answer::{Answer, AnswerAuthority};
use super::flavor::{Flavor, Flavored};
use super::rrset::{SharedRr, SharedRrset};
use super::versioned::{Version, Versioned};


//------------ Type Aliases --------------------------------------------------

pub type StoredDname = Dname<Bytes>;
pub type StoredRecordData = ZoneRecordData<Bytes, StoredDname>;
pub type StoredRecord = Record<StoredDname, StoredRecordData>;


//------------ Zone ----------------------------------------------------------

pub struct Zone {
    apex: Arc<ZoneApex>,
    versions: Arc<RwLock<ZoneVersions>>,
    update_lock: Arc<AsyncMutex<()>>,
}

impl Zone {
    pub fn new(apex_name: StoredDname) -> Self {
        Zone {
            apex: Arc::new(ZoneApex::new(apex_name)),
            versions: Default::default(),
            update_lock: Default::default(),
        }
    }

    pub fn apex_name(&self) -> &StoredDname {
        &self.apex.apex_name
    }

    pub fn read(&self) -> ZoneView {
        let (version, marker) = self.versions.read().current.clone();
        ZoneView::new(self.apex.clone(), version, marker)
    }

    pub async fn write(&self) -> ZoneUpdate {
        ZoneUpdate::new(
            self.update_lock.clone().lock_owned().await,
            self.apex.clone(),
            self.versions.clone()
        )
    }
}


//------------ ZoneVersions --------------------------------------------------

struct ZoneVersions {
    current: (Version, Arc<VersionMarker>),
    all: Vec<(Version, Weak<VersionMarker>)>,
}

impl Default for ZoneVersions {
    fn default() -> Self {
        let marker = Arc::new(VersionMarker);
        let weak_marker = Arc::downgrade(&marker);
        ZoneVersions {
            current: (Version::default(), marker),
            all: vec![(Version::default(), weak_marker)]
        }
    }
}


//------------ VersionMarker -------------------------------------------------

pub struct VersionMarker;


//------------ ZoneView ------------------------------------------------------

pub struct ZoneView {
    apex: Arc<ZoneApex>,
    version: Version,
    _version_marker: Arc<VersionMarker>,
}

impl ZoneView {
    fn new(
        apex: Arc<ZoneApex>,
        version: Version,
        _version_marker: Arc<VersionMarker>
    ) -> Self {
        ZoneView { apex, version, _version_marker }
    }

    pub fn query(
        &self, qname: &impl ToDname, qtype: Rtype, flavor: Option<Flavor>,
    ) -> Answer {
        let qname = self.apex.prepare_name(qname);
        self.apex.query(qname, qtype, flavor, self.version)
    }
}


//------------ ZoneUpdate ----------------------------------------------------

pub struct ZoneUpdate {
    apex: Arc<ZoneApex>,
    _lock: OwnedMutexGuard<()>,
    version: Version,
    zone_versions: Arc<RwLock<ZoneVersions>>,
}

impl ZoneUpdate {
    fn new(
        _lock: OwnedMutexGuard<()>,
        apex: Arc<ZoneApex>,
        zone_versions: Arc<RwLock<ZoneVersions>>
    ) -> Self {
        let version = zone_versions.read().current.0.next();
        ZoneUpdate { apex, _lock, version, zone_versions }
    }

    pub fn commit(&mut self) {
        // The order here is important so we don’t accidentally remove the
        // newly created version right away.
        let marker = Arc::new(VersionMarker);
        self.zone_versions.write().current = (self.version, marker.clone());
        self.clean_versions();
        self.zone_versions.write().all.push(
            (self.version, Arc::downgrade(&marker))
        );
        
        // Start the next version.
        self.version = self.version.next();
    }

    fn clean_versions(&mut self) {
        // Remove all dropped versions from the list and keep the largest
        // number.

        let mut max_version = None;
        self.zone_versions.write().all.retain(|item| {
            if item.1.strong_count() > 0 {
                true
            }
            else {
                match max_version {
                    Some(old) => {
                        if item.0 > old {
                            max_version = Some(item.0)
                        }
                    }
                    None => max_version = Some(item.0)
                }
                false
            }
        });
        if let Some(version) = max_version {
            self.apex.clean(version)
        }
    }

    /// Sets the RRset for the given name and flavor.
    ///
    /// The method will fail if the there are zone cuts or aliases along the
    /// way or if the node itself is not a (possibly empty or even not yet
    /// existing) populated node.
    pub fn set_rrset(
        &mut self,
        name: &impl ToDname,
        rrset: SharedRrset,
        flavor: Option<Flavor>,
    ) -> Result<(), UpdateError> {
        let mut name = self.apex.prepare_name(name);
        match name.next() {
            Some(label) => {
                self.apex.get_accessible_node(
                    label, name, flavor, self.version
                )?.set_rrset(rrset, flavor, self.version)
            }
            None => self.apex.set_rrset(rrset, flavor, self.version)
        }
    }
}

impl Drop for ZoneUpdate {
    fn drop(&mut self) {
        self.apex.rollback(self.version)
    }
}


//------------ ZoneApex ------------------------------------------------------

struct ZoneApex {
    apex_name: StoredDname,
    soa: RwLock<Versioned<SharedRrset>>,
    default_rrsets: RwLock<NodeRrsets>,
    flavor_rrsets: RwLock<Flavored<NodeRrsets>>,
    children: NodeChildren,
}

impl ZoneApex {
    fn new(apex_name: StoredDname) -> Self {
        ZoneApex {
            apex_name,
            soa: Default::default(),
            default_rrsets: Default::default(),
            flavor_rrsets: Default::default(),
            children: Default::default(),
        }
    }
}

/// # Read Access
///
impl ZoneApex {
    fn prepare_name<'l>(
        &self, qname: &'l impl ToDname
    ) -> impl Iterator<Item=&'l Label> {
        let mut qname = qname.iter_labels().rev();
        for apex_label in self.apex_name.iter_labels().rev() {
            let qname_label = qname.next();
            assert_eq!(
                Some(apex_label), qname_label,
                "dispatched query to the wrong zone"
            );
        }
        qname
    }

    fn query<'l>(
        &self,
        mut qname: impl Iterator<Item=&'l Label>,
        qtype: Rtype,
        flavor: Option<Flavor>,
        version: Version
    ) -> Answer {
        let mut answer = if let Some(label) = qname.next() {
            if let Some(child) = self.children.get_child(label) {
                child.query(qname, qtype, flavor, version)
            }
            else {
                NodeAnswer::nx_domain()
            }
        }
        else {
            self.query_here(qtype, flavor, version)
        };
        if answer.add_soa {
            self.add_soa(version, &mut answer.answer);
        }
        answer.answer
    }

    fn add_soa(&self, version: Version, answer: &mut Answer) {
        if let Some(soa) = self.soa.read().get(version).cloned() {
            answer.add_authority(
                AnswerAuthority::new(self.apex_name.clone(), soa, None)
            )
        }
    }

    fn query_here(
        &self, qtype: Rtype, flavor: Option<Flavor>, version: Version
    ) -> NodeAnswer {
        if let Some(flavor) = flavor {
            if let Some(answer) =
                self.flavor_rrsets.read().get(flavor)
                    .and_then(|rrset| rrset.query(qtype, version))
            {
                return answer
            }
        }
        if let Some(answer) =
            self.default_rrsets.read().query(qtype, version)
        {
            return answer
        }
        NodeAnswer::no_data()
    }
}


/// # Write Access
///
impl ZoneApex {
    /*
    fn get_node<'l>(
        &self,
        label: &'l Label,
        name: impl Iterator<Item = &'l Label>
    ) -> Option<Arc<ZoneNode>> {
        self.children.get_child(label)?.get_node(name)
    }
    */

    /// Returns the node for the given name and makes sure it is accessible.
    ///
    /// Walks down the tree of nodes starting with the child identified by
    /// `label`. Further children are taken from the iterator `name`. Each
    /// node is added if it doesn’t yet exist.
    ///
    /// The method assumes that the final node is going to receive data of
    /// some kind for the given flavor, i.e., it will either be a populated
    /// node with at least one RRset or a zone cut. It will thus error out if
    /// there are zone cuts along the path. It will flip any NXDomain nodes
    /// to empty populated nodes.
    pub fn get_accessible_node<'l>(
        &self,
        label: &'l Label,
        name: impl Iterator<Item = &'l Label>,
        flavor: Option<Flavor>,
        version: Version,
    ) -> Result<Arc<ZoneNode>, UpdateError> {
        self.children.get_or_insert(label).get_accessible_node(
            name, flavor, version
        )
    }

    pub fn set_rrset(
        &self,
        rrset: SharedRrset,
        flavor: Option<Flavor>,
        version: Version
    ) -> Result<(), UpdateError> {
        match flavor {
            Some(flavor) => {
                self.flavor_rrsets.write()
                    .get_or_default(flavor).update(rrset, version)
            }
            None => {
                self.default_rrsets.write().update(rrset, version)
            }
        }
    }

    pub fn rollback(&self, version: Version) {
        self.default_rrsets.write().rollback(version);
        self.flavor_rrsets.write().iter_mut().for_each(|item| {
            item.rollback(version)
        });
        self.children.rollback(version);
    }

    pub fn clean(&self, version: Version) {
        self.default_rrsets.write().clean(version);
        self.flavor_rrsets.write().iter_mut().for_each(|item| {
            item.clean(version)
        });
        self.children.clean(version);
    }
}


//------------ ZoneNode ------------------------------------------------------

/// A node in the zone tree.
///
/// The node stores the data for a single domain name in the zone. It carries
/// the (possibly empty) data of the name itself as well as links to all the
/// children with “more specific” names.
struct ZoneNode {
    /// The default content for the node if no flavors are used.
    default: RwLock<Versioned<NodeContent>>,

    /// The content of the node for flavors.
    ///
    /// If present for a specific flavor, the content here replaces the
    /// default content except if both the default and the flavored content
    /// are of the “populated” variant, in which case the flavored RRsets are
    /// extending and overiding the default RRsets.
    flavors: RwLock<Flavored<Versioned<NodeContent>>>,

    /// The child nodes.
    children: NodeChildren,
}

/// # Read Access
///
impl ZoneNode {
    pub fn query<'l>(
        &self,
        mut qname: impl Iterator<Item = &'l Label>,
        qtype: Rtype,
        flavor: Option<Flavor>,
        version: Version
    ) -> NodeAnswer {
        if let Some(label) = qname.next() {
            self.query_below(label, qname, qtype, flavor, version)
        }
        else {
            self.query_here(qtype, flavor, version)
        }
    }

    fn query_below<'l>(
        &self,
        label: &Label,
        qname: impl Iterator<Item = &'l Label>,
        qtype: Rtype,
        flavor: Option<Flavor>,
        version: Version
    ) -> NodeAnswer {
        // Whether we need to actually go further depends on the node content.
        // If we are a zone cut or an NX domain, we are done.
        self.with_content(flavor, version, |content| {
            match content {
                Some(NodeContent::Cut(ref cut)) => {
                    NodeAnswer::authority(
                        AnswerAuthority::new(
                            cut.name.clone(),
                            cut.ns.clone(),
                            cut.ds.as_ref().cloned()
                        )
                    )
                }
                Some(NodeContent::NxDomain) => NodeAnswer::nx_domain(),
                _ => {
                    match self.children.get_child(label) {
                        Some(child) => {
                            child.query(qname, qtype, flavor, version)
                        }
                        None => NodeAnswer::nx_domain()
                    }
                }
            }
        })
    }

    fn with_content<R>(
        &self, 
        flavor: Option<Flavor>,
        version: Version,
        op: impl FnOnce(Option<&NodeContent>) -> R
    ) -> R {
        if let Some(flavor) = flavor {
            if let Some(content) =
                self.flavors.read().get(flavor)
                    .and_then(|flavor| flavor.get(version))
            {
                op(Some(content))
            }
            else {
                op(self.default.read().get(version))
            }
        }
        else {
            op(self.default.read().get(version))
        }
    }

    fn query_here(
        &self,
        qtype: Rtype,
        flavor: Option<Flavor>,
        version: Version
    ) -> NodeAnswer {
        // Special case: if there is populated content for the flavor, but it
        // doesn’t contain an RRset for qtype, we need to also check if the
        // default has populated content that contains an RRset for qtype.
        //
        // That’s why we can’t simply use `with_content` again.

        if let Some(flavor) = flavor {
            if let Some(content) =
                self.flavors.read().get(flavor)
                    .and_then(|flavor| flavor.get(version))
            {
                match content {
                    NodeContent::Populated(ref rrsets) => {
                        if let Some(answer) = rrsets.query(qtype, version) {
                            return answer
                        }
                        else if let Some(NodeContent::Populated(ref rrsets)) =
                            self.default.read().get(version)
                        {
                            return rrsets.query(qtype, version)
                                .unwrap_or_else(NodeAnswer::no_data)
                        }
                        else {
                            return NodeAnswer::no_data()
                        }
                    }
                    NodeContent::Cname(ref rr) => {
                        return NodeAnswer::cname(rr.clone())
                    }
                    NodeContent::Cut(ref cut) => {
                        return cut.query_here(qtype)
                    }
                    NodeContent::NxDomain => return NodeAnswer::nx_domain()
                }
            }
        }

        match self.default.read().get(version) {
            Some(NodeContent::Populated(ref rrsets)) => {
                rrsets.query(qtype, version)
                    .unwrap_or_else(NodeAnswer::no_data)
            }
            Some(NodeContent::Cname(ref rr)) => NodeAnswer::cname(rr.clone()),
            Some(NodeContent::Cut(ref cut)) => cut.query_here(qtype),
            Some(NodeContent::NxDomain) => NodeAnswer::nx_domain(),
            None => NodeAnswer::no_data(),
        }
    }
}

/// # Write Access
impl ZoneNode {
    /// Returns the node for the given name and makes sure it is accessible.
    ///
    /// See [`ZoneApex::get_accessible_node`] for an explanation of what this
    /// means.
    pub fn get_accessible_node<'l>(
        self: Arc<ZoneNode>,
        mut name: impl Iterator<Item = &'l Label>,
        flavor: Option<Flavor>,
        version: Version,
    ) -> Result<Arc<ZoneNode>, UpdateError> {
        let label = match name.next() {
            Some(label) => label,
            None => return Ok(self)
        };
        self.make_accessible(flavor, version)?;
        self.children.get_or_insert(label).get_accessible_node(
            name, flavor, version
        )
    }

    fn make_accessible(
        &self, flavor: Option<Flavor>, version: Version
    ) -> Result<(), UpdateError> {
        // Deal with a special flavor first.
        if let Some(flavor) = flavor {
            let mut flavor_content = self.flavors.write();
            if let Some(flavor_content) = flavor_content.get_mut(flavor) {
                match flavor_content.last() {
                    Some(NodeContent::Cut(_)) => {
                        return Err(UpdateError::ZoneCutOnPath)
                    }
                    Some(NodeContent::NxDomain) => {
                        flavor_content.update(version, NodeContent::no_data());
                        return Ok(())
                    }
                    Some(_) => return Ok(()),
                    _ => {}
                }
            }
        }

        let mut default_content = self.default.write();
        match default_content.last() {
            Some(NodeContent::Cut(_)) => {
                Err(UpdateError::ZoneCutOnPath)
            }
            Some(NodeContent::NxDomain) => {
                if let Some(flavor) = flavor {
                    self.flavors.write().get_or_default(flavor).update(
                        version, NodeContent::no_data()
                    );
                }
                else {
                    default_content.update(version, NodeContent::no_data())
                }
                Ok(())
            }
            _ => Ok(())
        }
    }

    pub fn set_rrset(
        &self,
        rrset: SharedRrset,
        flavor: Option<Flavor>,
        version: Version
    ) -> Result<(), UpdateError> {
        fn update_content(
            content: &mut Versioned<NodeContent>,
            rrset: SharedRrset,
            version: Version
        ) -> Result<(), UpdateError> {
            match content.last() {
                Some(NodeContent::Populated(_)) => {}
                Some(NodeContent::Cname(_)) => {
                    return Err(UpdateError::NameIsCname)
                }
                Some(NodeContent::Cut(_)) => {
                    return Err(UpdateError::NameIsZoneCut)
                }
                Some(NodeContent::NxDomain) | None => {
                    content.update(version, NodeContent::no_data())
                }
            }
            match content.last_mut() {
                Some(NodeContent::Populated(ref mut rrsets)) => {
                    rrsets.update(rrset, version)
                }
                _ => unreachable!()
            }
        }

        match flavor {
            Some(flavor) => {
                update_content(
                    self.flavors.write().get_or_default(flavor),
                    rrset, version
                )
            }
            None => {
                update_content(&mut self.default.write(), rrset, version)
            }
        }
    }

    /*
    /// Convert the content for this node and flavour into a zone cut.
    ///
    /// If the content already is a zone cut, merely updates the data.
    /// 
    /// Note: This method assumes that the zone cut’s `name` field is
    /// correct.
    pub fn set_zone_cut(
        &self,
        cut: ZoneCut,
        flavor: Option<Flavor>,
        version: Version
    ) -> Result<(), UpdateError> {
        match flavor {
            Some(flavor) => self.set_flavor_zone_cut(cut, flavor, version),
            None => self.set_default_zone_cut(cut, version),
        }
    }

    fn set_default_zone_cut(
        &self,
        _cut: ZoneCut,
        _version: Version,
    ) -> Result<(), UpdateError> {
        unimplemented!()
    }

    fn set_flavor_zone_cut(
        &self,
        _cut: ZoneCut,
        _flavor: Flavor,
        _version: Version,
    ) -> Result<(), UpdateError> {
        unimplemented!()
    }
    */

    pub fn rollback(&self, version: Version) {
        self.default.write().rollback(version);
        self.flavors.write().iter_mut().for_each(|item|
            item.rollback(version)
        );
        self.children.rollback(version);
    }

    pub fn clean(&self, version: Version) {
        self.default.write().clean(version);
        self.flavors.write().iter_mut().for_each(|item| item.clean(version));
        self.children.clean(version);
    }
}

impl Default for ZoneNode {
    fn default() -> Self {
        ZoneNode {
            default: RwLock::new(Versioned::default()),
            flavors: RwLock::new(Flavored::default()),
            children: NodeChildren::default()
        }
    }
}


//------------ NodeContent ---------------------------------------------------

/// The content of a node.
#[allow(dead_code)] // XXX
enum NodeContent {
    /// A “normal” node with a (possibly empty) set of RRsets.
    Populated(NodeRrsets),

    /// An alias node.
    ///
    /// The data for the node can be found at the given name.
    Cname(SharedRr),

    /// A zone cut.
    ///
    /// Authoritative data for the domain name lives elsewhere.
    Cut(ZoneCut),

    /// The node doesn’t actually exist.
    ///
    /// This variant is used in conjunction with flavors to signal that, even
    /// though there may be child nodes, none of them have any data for this
    /// flavor and an answer should be NXDomain.
    NxDomain,
}

impl NodeContent {
    fn no_data() -> Self {
        NodeContent::Populated(NodeRrsets::default())
    }
}


//------------ ZoneCut -------------------------------------------------------

struct ZoneCut {
    name: StoredDname,
    ns: SharedRrset,
    ds: Option<SharedRrset>,
}

impl ZoneCut {
    fn query_here(&self, qtype: Rtype) -> NodeAnswer {
        match qtype {
            Rtype::Ds => {
                if let Some(rrset) = self.ds.as_ref() {
                    NodeAnswer::data(rrset.clone())
                }
                else {
                    NodeAnswer::no_data()
                }
            }
            _ => {
                NodeAnswer::authority(
                    AnswerAuthority::new(
                        self.name.clone(),
                        self.ns.clone(),
                        self.ds.as_ref().cloned()
                    )
                )
            }
        }
    }
}


//------------ NodeRrsets ----------------------------------------------------

#[derive(Default)]
struct NodeRrsets {
    rrsets: HashMap<Rtype, Versioned<SharedRrset>>,
}

impl NodeRrsets {
    fn get(
        &self, rtype: Rtype, version: Version
    ) -> Option<SharedRrset> {
        self.rrsets.get(&rtype)
            .and_then(|rrset| rrset.get(version).cloned())
    }

    fn query(
        &self, qtype: Rtype, version: Version
    ) -> Option<NodeAnswer> {
        self.get(qtype, version).map(NodeAnswer::data)
    }

    /// Updates the RRset for the given type and flavor.
    fn update(
        &mut self,
        rrset: SharedRrset,
        version: Version
    ) -> Result<(), UpdateError> {
        self.rrsets.entry(rrset.rtype()).or_default().update(version, rrset);
        Ok(())
    }

    fn rollback(&mut self, version: Version) {
        self.rrsets.values_mut().for_each(|value| value.rollback(version));
    }

    fn clean(&mut self, version: Version) {
        self.rrsets.values_mut().for_each(|value| value.clean(version));
    }
}


//------------ NodeChildren --------------------------------------------------

#[derive(Default)]
struct NodeChildren {
    children: RwLock<HashMap<OwnedLabel, Arc<ZoneNode>>>
}

impl NodeChildren {
    fn get_child(&self, label: &Label) -> Option<Arc<ZoneNode>> {
        self.children.read().get(label).cloned()
    }

    fn get_or_insert(&self, label: &Label) -> Arc<ZoneNode> {
        self.children.write().entry(label.into()).or_default().clone()
    }

    fn rollback(&self, version: Version) {
        self.children.read().values().for_each(|item| item.rollback(version))
    }

    fn clean(&self, version: Version) {
        self.children.read().values().for_each(|item| item.clean(version))
    }
}


//------------ NodeAnswer ----------------------------------------------------

/// An answer that includes instructions to the apex on what it needs to do.
#[derive(Clone)]
struct NodeAnswer {
    /// The actual answer.
    answer: Answer,

    /// Does the apex need to add the SOA RRset to the answer?
    add_soa: bool,
}

impl NodeAnswer {
    fn data(rrset: SharedRrset) -> Self {
        // Empty RRsets are used to remove a default RRset for a flavor. So
        // they really are NODATA responses.
        if rrset.is_empty() {
            Self::no_data()
        }
        else {
            let mut answer = Answer::new(Rcode::NoError);
            answer.add_answer(rrset);
            NodeAnswer {
                answer,
                add_soa: false,
            }
        }
    }

    fn no_data() -> Self {
        NodeAnswer{
            answer: Answer::new(Rcode::NoError),
            add_soa: true,
        }
    }

    fn cname(rr: SharedRr) -> Self {
        let mut answer = Answer::new(Rcode::NoError);
        answer.add_cname(rr);
        NodeAnswer {
            answer,
            add_soa: false
        }
    }

    fn nx_domain() -> Self {
        NodeAnswer {
            answer: Answer::new(Rcode::NXDomain),
            add_soa: true,
        }
    }

    fn authority(authority: AnswerAuthority) -> Self {
        NodeAnswer {
            answer: Answer::with_authority(Rcode::NoError, authority),
            add_soa: false,
        }
    }
}


//============ Error Types ===================================================

#[derive(Clone, Copy, Debug)]
pub enum UpdateError {
    /// A zone cut appeared along the way to the name.
    ZoneCutOnPath,

    /// The name is a zone cut.
    NameIsZoneCut,

    /// The name is a CNAME.
    NameIsCname,

    /// The name has some RRsets.
    NameHasContent,
}

