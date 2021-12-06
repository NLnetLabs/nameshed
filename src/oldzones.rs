
use std::ops;
use std::sync::Arc;
use arc_swap::ArcSwapOption;
use bytes::Bytes;
use concread::hashmap::HashMap;
use domain::base::iana::{Class, Rcode, Rtype};
use domain::base::message::Message;
use domain::base::message_builder::MessageBuilder;
use domain::base::name::{Dname, Label, OwnedLabel, ToDname, ToLabelIter};
use domain::base::octets::OctetsBuilder;
use domain::base::record::Record;
//use domain::base::rdata::RecordData;
use domain::rdata::ZoneRecordData;
use serde::{Deserialize, Serialize};


//============ Zone Data Storage =============================================

//------------ ZoneSet -------------------------------------------------------

/// The set of zones we are authoritative for.
#[derive(Default)]
pub struct ZoneSet {
    in_root: Arc<ZoneSetNode>,
    other_roots: HashMap<Class, Arc<ZoneSetNode>>,
}

impl ZoneSet {
    pub fn query(
        &self,
        qname: &impl ToDname, qclass: Class, qtype: Rtype,
        flavor: Option<&str>,
    ) -> Answer {
        let root = if qclass == Class::In {
            self.in_root.clone()
        }
        else {
            match self.other_roots.read().get(&qclass) {
                Some(root) => root.clone(),
                None => return Answer::refused()
            }
        };
        match root.find_apex(qname.iter_labels().rev()) {
            Some(apex) => apex.query(qname, qtype, flavor),
            None => Answer::refused()
        }
    }

    pub fn get_apex(
        &self,
        apex_name: &impl ToDname,
        class: Class,
    ) -> Option<Arc<ZoneApex>> {
        if class == Class::In {
            self.in_root.get_apex(apex_name.iter_labels().rev())
        }
        else if let Some(root) = self.other_roots.read().get(&class) {
            root.get_apex(apex_name.iter_labels().rev())
        }
        else {
            None
        }
    }

    pub fn insert(
        &self,
        class: Class,
        apex: ZoneApex,
    ) -> Result<(), ZoneExists> {
        let apex_name = apex.name.clone();
        let apex_name = apex_name.iter_labels().rev();
        if class == Class::In {
            self.in_root.insert(apex_name, apex)
        }
        else if let Some(root) = self.other_roots.read().get(&class) {
            root.insert(apex_name, apex)
        }
        else {
            {
                let mut roots = self.other_roots.write();
                if !roots.contains_key(&class) {
                    roots.insert(class, Default::default());
                    roots.commit();
                }
            }
            self.other_roots.read().get(&class).unwrap()
                .insert(apex_name, apex)
        }

    }

    pub fn update_zone(
        &mut self,
        _apex_name: &impl ToDname,
        _class: Class,
        _flavor: Option<&str>,
        _records: impl Iterator<Item = Record<impl ToDname, StoredRecordData>>
    ) {
        unimplemented!();
        /*
        let root = if class == Class::In {
            &mut self.in_root
        }
        else {
            self.other_roots.entry(class).or_default()
        };
        let apex = root.get_apex_mut(
            apex_name.iter_labels().rev(), apex_name
        );
        apex.update_zone(flavor, records)
        */
    }
}


//------------ SharedZoneSet -------------------------------------------------

/// A shareable, mutable zone set.
///
/// Access to the set has read-write lock semantics, i.e., any number of
/// tasks can concurrently access the set for reading but only one task can
/// exclusively access the set for writing.
#[derive(Clone, Default)]
pub struct SharedZoneSet {
    set: Arc<ZoneSet>,
}

impl SharedZoneSet {
    pub fn new(set: ZoneSet) -> Self {
        SharedZoneSet { set: Arc::new(set) }
    }
}

impl ops::Deref for SharedZoneSet {
    type Target = ZoneSet;

    fn deref(&self) -> &Self::Target {
        &self.set
    }
}



//------------ ZoneSetNode ---------------------------------------------------

#[derive(Default)]
struct ZoneSetNode {
    zone: ArcSwapOption<ZoneApex>,
    children: HashMap<OwnedLabel, Arc<ZoneSetNode>>,
}

impl ZoneSetNode {
    pub fn find_apex<'l>(
        &self,
        mut qname: impl Iterator<Item = &'l Label>
    ) -> Option<Arc<ZoneApex>> {
        match qname.next() {
            Some(label) => {
                match self.children.read().get(label) {
                    Some(node) => {
                        match node.find_apex(qname) {
                            Some(apex) => Some(apex),
                            None => self.zone.load().as_ref().cloned(),
                        }
                    }
                    None => self.zone.load().as_ref().cloned(),
                }
            }
            None => self.zone.load().as_ref().cloned()
        }
    }

    fn get_apex<'l>(
        &self,
        mut apex_name: impl Iterator<Item = &'l Label>,
    ) -> Option<Arc<ZoneApex>> {
        match apex_name.next() {
            Some(label) => {
                self.children.read().get(label)?.get_apex(apex_name)
            }
            None => {
                self.zone.load().as_ref().cloned()
            }
        }
    }


    fn insert<'l>(
        &self,
        mut apex_name: impl Iterator<Item = &'l Label>,
        apex: ZoneApex,
    ) -> Result<(), ZoneExists> {
        match apex_name.next() {
            Some(label) => {
                match self.children.read().get(label) {
                    Some(node) => node.insert(apex_name, apex),
                    None => {
                        {
                            let mut children = self.children.write();
                            if !children.contains_key(label) {
                                children.insert(
                                    label.into(), Default::default()
                                );
                                children.commit()
                            }
                        }
                        self.children.read().get(label).unwrap()
                            .insert(apex_name, apex)
                    }
                }
            }
            None => {
                if self.zone.load().is_some() {
                    Err(ZoneExists)
                }
                else {
                    self.zone.store(Some(Arc::new(apex)));
                    Ok(())
                }
            }
        }
    }
}


//------------ ZoneApex ------------------------------------------------------

pub struct ZoneApex {
    name: StoredDname,
    flavors: HashMap<String, usize>,
    apex_node: ZoneNode,
}

impl ZoneApex {
    pub fn new(qname: &impl ToDname) -> Self {
        ZoneApex {
            name: qname.to_dname().unwrap(), // Vec<u8> is always big enough.
            flavors: Default::default(),
            apex_node: Default::default(),
        }
    }

    fn get_flavor(&self, name: Option<&str>) -> Flavor {
        Flavor::new(name.and_then(|name| {
            self.flavors.read().get(name).copied()
        }))
    }

    /*
    fn get_or_add_flavor(&mut self, name: Option<&str>) -> Flavor {
        if let Some(name) = name {
            if let Some(&idx) = self.flavors.get(name) {
                Flavor::new(Some(idx))
            }
            else {
                let idx = self.flavors.len();
                self.flavors.insert(name.into(), idx);
                Flavor::new(Some(idx))
            }
        }
        else {
            Flavor::new(None)
        }
    }
    */
}


//--- Querying
//
impl ZoneApex {
    pub fn query(
        &self, qname: &impl ToDname, qtype: Rtype, flavor: Option<&str>,
    ) -> Answer {
        let flavor = self.get_flavor(flavor);
        let qname = self.prepare_qname(qname);

        let mut answer = self.apex_node.query(qname, qtype, flavor);
        if answer.add_soa {
            self.add_soa(flavor, &mut answer.answer)
        }
        answer.answer
    }

    fn prepare_qname<'l>(
        &self, qname: &'l impl ToDname
    ) -> impl Iterator<Item=&'l Label> {
        let mut qname = qname.iter_labels().rev();
        let mut apex_name = self.name.iter_labels().rev();
        while let Some(apex_label) = apex_name.next() {
            let qname_label = qname.next();
            assert_eq!(
                Some(apex_label), qname_label,
                "dispatched query to the wrong zone"
            );
        }
        qname
    }

    fn add_soa(&self, flavor: Flavor, answer: &mut Answer) {
        if let Some(soa) = self.apex_node.get_rrset(Rtype::Soa, flavor) {
            answer.add_authority(
                AnswerAuthority::new(self.name.clone(), soa.clone(), None)
            )
        }
    }
}


/*
//--- Updating
//
impl ZoneApex {
    fn update_zone(
        &mut self, 
        flavor: Option<&str>,
        records: impl Iterator<Item = Record<impl ToDname, StoredRecordData>>
    ) {
        let flavor = self.get_or_add_flavor(flavor);
        for record in records {
            let ttl = record.ttl();
            let (owner, data) = record.into_owner_and_data();
            self.apex_node.insert_record(
                self.prepare_qname(&owner), &owner, ttl, data, flavor
            )
        }
    }
}
*/


//------------ SharedZoneApex ------------------------------------------------

/// A shareable, mutable zone apex.
///
/// Access has read-write lock semantics, i.e., any number of tasks can
/// concurrently access the apex for reading but only one task can
/// exclusively access the apex for writing.


//------------ ZoneNode ------------------------------------------------------

#[derive(Default)]
pub struct ZoneNode {
    content: Flavored<NodeContent>,
    children: HashMap<OwnedLabel, Arc<ZoneNode>>,
}

impl ZoneNode {
    fn query<'l>(
        &self,
        mut qname: impl Iterator<Item = &'l Label>,
        qtype: Rtype,
        flavor: Flavor
    ) -> NodeAnswer {
        match qname.next() {
            Some(label) => self.query_children(label, qname, qtype, flavor),
            None => self.query_here(qtype, flavor)
        }
    }

    fn query_children<'l>(
        &self,
        label: &'l Label,
        qname: impl Iterator<Item = &'l Label>,
        qtype: Rtype,
        flavor: Flavor
    ) -> NodeAnswer {
        // If the content for the given flavor is a zone cut, we don’t need
        // to continue further down and have to return a delegation instead.
        if let NodeContent::Cut(ref node) = *self.content.get(flavor) {
            return node.query_delegation()
        }

        // Otherwise, just dive down.
        match self.children.read().get(label) {
            Some(child) => child.query(qname, qtype, flavor),
            None => NodeAnswer::nx_domain()
        }
    }

    fn query_here(
        &self,
        qtype: Rtype,
        flavor: Flavor
    ) -> NodeAnswer {
        match *self.content.get(flavor) {
            NodeContent::Populated(ref node) => {
                node.query_here(qtype, flavor)
            }
            NodeContent::Cut(ref node) => {
                node.query_here(qtype, flavor)
            }
            NodeContent::Empty => {
                NodeAnswer::no_data()
            }
            NodeContent::NxDomain => {
                NodeAnswer::nx_domain()
            }
        }
    }

    fn get_rrset(&self, qtype: Rtype, flavor: Flavor) -> Option<SharedRrset> {
        match *self.content.get(flavor) {
            NodeContent::Populated(ref node) => node.get_rrset(qtype, flavor),
            NodeContent::Cut(ref node) => node.get_rrset(qtype, flavor),
            _ => None
        }
    }

    /*
    fn insert_record<'l>(
        &mut self, 
        mut labels: impl Iterator<Item = &'l Label>,
        name: &impl ToDname,
        ttl: u32,
        data: StoredRecordData,
        flavor: Flavor,
    ) {
        match labels.next() {
            Some(label) => {
                self.insert_child(
                    label.into(), labels, name, ttl, data, flavor
                )
            }
            None => self.insert_here(name, ttl, data, flavor),
        }
    }

    fn insert_child<'l>(
        &mut self, 
        label: OwnedLabel,
        labels: impl Iterator<Item = &'l Label>,
        name: &impl ToDname,
        ttl: u32,
        data: StoredRecordData,
        flavor: Flavor,
    ) {
        self.children.entry(label.into()).or_default().insert_record(
            labels, name, ttl, data, flavor
        );
        if matches!(self.content.get(flavor), NodeContent::NxDomain) {
            *self.content.get_mut(flavor) = NodeContent::Empty;
        }
    }

    fn insert_here<'l>(
        &mut self, 
        name: &impl ToDname,
        ttl: u32,
        data: StoredRecordData,
        flavor: Flavor,
    ) {
        let content = self.content.get_mut(flavor);
        if Rrset::is_delegation(data.rtype()) {
            if matches!(
                content, NodeContent::Empty | NodeContent::NxDomain
            ) {
                *content = NodeContent::Cut(
                    ZoneCut::empty(name.to_dname().unwrap())
                );
            }
            match *content {
                NodeContent::Cut(ref mut cut) => {
                    cut.insert_here(ttl, data);
                }
                _ => panic!("expected zone cut")
            }
        }
        else {
            if matches!(
                content, NodeContent::Empty | NodeContent::NxDomain
            ) {
                *content = NodeContent::Populated(Default::default());
            }
            match *content {
                NodeContent::Populated(ref mut populated) => {
                    populated.insert_here(ttl, data, flavor)
                }
                _ => panic!("expected regular node")
            }
        }
    }
*/
}


//------------ NodeContent ---------------------------------------------------

pub enum NodeContent {
    Populated(PopulatedNode),
    Cut(ZoneCut),
    Empty,
    NxDomain,
}

impl Default for NodeContent {
    fn default() -> Self {
        NodeContent::NxDomain
    }
}


//------------ PopulatedNode -------------------------------------------------

#[derive(Default)]
pub struct PopulatedNode {
    rrsets: HashMap<Rtype, Flavored<Option<SharedRrset>>>,
}

impl PopulatedNode {
    fn query_here(
        &self,
        qtype: Rtype,
        flavor: Flavor
    ) -> NodeAnswer {
        match self.get_rrset(qtype, flavor) {
            Some(rrset) => NodeAnswer::data(rrset.clone()),
            None => NodeAnswer::no_data(),
        }
    }

    fn get_rrset(&self, qtype: Rtype, flavor: Flavor) -> Option<SharedRrset> {
        self.rrsets.read().get(&qtype)
            .and_then(|rrset| rrset.get(flavor).as_ref()).cloned()
    }

    /*
    fn insert_here<'l>(
        &mut self, 
        ttl: u32,
        data: StoredRecordData,
        flavor: Flavor
    ) {
        let rrset = self.rrsets.entry(
            data.rtype()
        ).or_default().get_mut(flavor);
        if let Some(rrset) = rrset.as_mut() {
            rrset.push_data(ttl, data);
        }
        else {
            *rrset = Some(SharedRrset::new(Rrset::new(ttl, data)))
        }
    }
    */
}


//------------ ZoneCut -------------------------------------------------------

pub struct ZoneCut {
    name: StoredDname,
    ns: SharedRrset,
    ds: Option<SharedRrset>,
}

impl ZoneCut {
    /*
    fn empty(name: StoredDname) -> Self {
        ZoneCut {
            name,
            ns: SharedRrset::default(),
            ds: None,
        }
    }
    */

    fn query_here(
        &self,
        qtype: Rtype,
        _flavor: Flavor
    ) -> NodeAnswer {
        match qtype {
            Rtype::Ds => {
                match self.ds.as_ref() {
                    Some(rrset) => NodeAnswer::data(rrset.clone()),
                    None => NodeAnswer::no_data(),
                }
            }
            _ => self.query_delegation(),
        }
    }

    fn query_delegation(&self) -> NodeAnswer {
        NodeAnswer::authority(
            AnswerAuthority::new(
                self.name.clone(), self.ns.clone(), self.ds.as_ref().cloned()
            )
        )
    }

    fn get_rrset(&self, qtype: Rtype, _flavor: Flavor) -> Option<SharedRrset> {
        match qtype {
            Rtype::Ns => Some(self.ns.clone()),
            Rtype::Ds => self.ds.as_ref().cloned(),
            _ => None
        }
    }

    /*
    fn insert_here<'l>(
        &mut self, 
        ttl: u32,
        data: StoredRecordData,
    ) {
        match data.rtype() {
            Rtype::Ns => self.ns.push_data(ttl, data),
            Rtype::Ds => {
                if let Some(rrset) = self.ds.as_mut() {
                    rrset.push_data(ttl, data)
                }
                else {
                    self.ds = Some(SharedRrset::new(Rrset::new(ttl, data)))
                }
            }
            _ => panic!("unexpected rtype")
        }
    }
    */
}


//------------ Rrset ---------------------------------------------------------

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Rrset {
    ttl: u32,
    data: Vec<StoredRecordData>,
    additional: Vec<StoredRecord>,
}

impl Rrset {
    /*
    fn is_delegation(rtype: Rtype) -> bool {
        matches!(rtype, Rtype::Ns | Rtype::Ds)
    }

    fn new(ttl: u32, record: StoredRecordData) -> Self {
        Rrset {
            ttl,
            data: vec![record],
            additional: Vec::new()
        }
    }
    */

    pub fn ttl(&self) -> u32 {
        self.ttl
    }

    pub fn data(&self) -> &[StoredRecordData] {
        &self.data
    }

    pub fn additional(&self) -> &[StoredRecord] {
        &self.additional
    }

    pub fn set_ttl(&mut self, ttl: u32) {
        self.ttl = ttl;
    }

    pub fn push_data(&mut self, data: StoredRecordData) {
        self.data.push(data);
    }

    pub fn push_additional(&mut self, record: StoredRecord) {
        self.additional.push(record);
    }
}


//------------ SharedRrset ---------------------------------------------------

/// An RRset behind an arc.
#[derive(Clone, Default)]
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


//------------ Type Aliases --------------------------------------------------

type StoredDname = Dname<Bytes>;
type StoredRecordData = ZoneRecordData<Bytes, StoredDname>;
type StoredRecord = Record<StoredDname, StoredRecordData>;



//------------ Flavor --------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Flavor {
    index: Option<usize>,
}

impl Flavor {
    fn new(index: Option<usize>) -> Self {
        Flavor { index }
    }
}


//------------ Flavored ------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Flavored<T> {
    default: T,
    flavors: Vec<Option<T>>,
}

impl<T> Flavored<T> {
    pub fn get(&self, flavor: Flavor) -> &T {
        match flavor.index {
            Some(index) => {
                self.flavors.get(index)
                    .map(|opt| opt.as_ref())
                    .flatten()
                    .unwrap_or_else(|| &self.default)
            }
            None => &self.default
        }
    }

    pub fn get_mut(&mut self, flavor: Flavor) -> &mut T
    where T: Default {
        match flavor.index {
            Some(index) => {
                if index >= self.flavors.len() {
                    self.flavors.resize_with(index + 1, || None);
                }
                let res = &mut self.flavors[index];
                if res.is_none() {
                    *res = Some(T::default());
                }
                res.as_mut().unwrap()
            }
            None => &mut self.default
        }
    }
}

impl<T: Default> Default for Flavored<T> {
    fn default() -> Self {
        Flavored {
            default: T::default(),
            flavors: Vec::new()
        }
    }
}


//============ Querying ======================================================

//------------ Answer --------------------------------------------------------

#[derive(Clone)]
pub struct Answer {
    /// The response code of the answer.
    rcode: Rcode,

    /// The actual answer, if available.
    answer: Option<SharedRrset>,

    // Cname chain

    /// The optional authority section to be included in the answer.
    authority: Option<AnswerAuthority>
}

impl Answer {
    fn new(rcode: Rcode) -> Self {
        Answer {
            rcode,
            answer: None,
            authority: Default::default(),
        }
    }

    fn with_authority(rcode: Rcode, authority: AnswerAuthority) -> Self {
        Answer {
            rcode,
            answer: None,
            authority: Some(authority),
        }
    }

    fn refused() -> Self {
        Answer::new(Rcode::Refused)
    }

    fn add_answer(&mut self, answer: SharedRrset) {
        self.answer = Some(answer.clone())
    }

    fn add_authority(&mut self, authority: AnswerAuthority) {
        self.authority = Some(authority)
    }

    pub fn to_message<Target: OctetsBuilder>(
        &self,
        message: Message<&[u8]>,
        builder: MessageBuilder<Target>
    ) -> Target {
        let question = message.sole_question().unwrap();
        let qname = question.qname();
        let qclass = question.qclass();
        let mut builder = builder.start_answer(&message, self.rcode).unwrap();

        if let Some(ref answer) = self.answer {
            for item in answer.data() {
                builder.push((qname, qclass, answer.ttl(), item)).unwrap();
            }
        }

        let mut builder = builder.authority();
        if let Some(authority) = self.authority.as_ref() {
            for item in &authority.ns_or_soa.data {
                builder.push(
                    (
                        authority.owner.clone(), qclass,
                        authority.ns_or_soa.ttl,
                        item
                    )
                ).unwrap()
            }
            if let Some(ref ds) = authority.ds {
                for item in ds.data() {
                    builder.push(
                        (authority.owner.clone(), qclass, ds.ttl, item)
                    ).unwrap()
                }
            }
        }

        builder.finish()
    }
}


//------------ AnswerAuthority -----------------------------------------------

/// The authority section of a query answer.
#[derive(Clone)]
pub struct AnswerAuthority {
    /// The owner name of the record sets in the authority section.
    owner: StoredDname,

    /// The NS or SOA record set.
    ///
    /// If the answer is a no-data answer, the SOA record is used. If the
    /// answer is a delegation answer, the NS record is used.
    ns_or_soa: SharedRrset,

    /// The optional DS record set.
    ///
    /// This is only used in a delegation answer.
    ds: Option<SharedRrset>,
}

impl AnswerAuthority {
    fn new(
        owner: StoredDname,
        ns_or_soa: SharedRrset,
        ds: Option<SharedRrset>,
    ) -> Self {
        AnswerAuthority { owner, ns_or_soa, ds }
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
        let mut answer = Answer::new(Rcode::NoError);
        answer.add_answer(rrset);
        NodeAnswer {
            answer,
            add_soa: false,
        }
    }

    fn no_data() -> Self {
        NodeAnswer{
            answer: Answer::new(Rcode::NoError),
            add_soa: true,
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

pub struct ZoneExists;

