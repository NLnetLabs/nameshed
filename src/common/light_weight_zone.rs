/// A quick PoC to see if using a BTree compared to default in-memory zone
/// store uses less memory, and it does, even with its dumb way of storing
/// values in the tree. It's not a fair comparison either as the default
/// in-memory store also supports proper answers to queries, versioning and
/// IXFR diff generation.
use std::{
    any::Any,
    collections::{btree_map::Entry, BTreeMap, HashSet},
    future::{ready, Future},
    ops::Deref,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use bytes::{Bytes, BytesMut};
use domain::{
    base::{
        iana::{Class, Rcode},
        name::Label,
        Name, NameBuilder, Rtype,
    },
    zonetree::{
        error::OutOfZone, Answer, InMemoryZoneDiff, ReadableZone, Rrset, SharedRrset, StoredName,
        WalkOp, WritableZone, WritableZoneNode, ZoneStore,
    },
};
use log::trace;

#[derive(Debug, Eq)]
struct HashedSharedRrset(SharedRrset);

impl std::hash::Hash for HashedSharedRrset {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.rtype().hash(state);
    }
}

impl std::ops::Deref for HashedSharedRrset {
    type Target = SharedRrset;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for HashedSharedRrset {
    fn eq(&self, other: &Self) -> bool {
        self.0.rtype() == other.0.rtype()
    }
}

#[derive(Clone, Debug)]
struct SimpleZoneInner {
    root: StoredName,
    tree: Arc<std::sync::RwLock<BTreeMap<StoredName, HashSet<HashedSharedRrset>>>>,
    skipped: Arc<AtomicUsize>,
    skip_signed: bool,
}

#[derive(Clone, Debug)]
pub struct LightWeightZone {
    inner: SimpleZoneInner,
}

impl LightWeightZone {
    pub fn new(root: StoredName, skip_signed: bool) -> Self {
        Self {
            inner: SimpleZoneInner {
                root,
                tree: Default::default(),
                skipped: Default::default(),
                skip_signed,
            },
        }
    }
}

impl ZoneStore for LightWeightZone {
    fn class(&self) -> Class {
        Class::IN
    }

    fn apex_name(&self) -> &StoredName {
        &self.inner.root
    }

    fn read(self: Arc<Self>) -> Box<dyn ReadableZone> {
        trace!("READ");
        Box::new(self.inner.clone()) as Box<dyn ReadableZone>
    }

    fn write(
        self: Arc<Self>,
    ) -> Pin<Box<(dyn Future<Output = Box<(dyn WritableZone + 'static)>> + Send + Sync + 'static)>>
    {
        trace!("WRITE");
        Box::pin(ready(Box::new(self.inner.clone()) as Box<dyn WritableZone>))
    }

    fn as_any(&self) -> &dyn Any {
        trace!("ANY");
        self as &dyn Any
    }
}

impl ReadableZone for SimpleZoneInner {
    fn query(&self, qname: Name<Bytes>, qtype: Rtype) -> Result<Answer, OutOfZone> {
        trace!("QUERY");
        Ok(self
            .tree
            .read()
            .unwrap()
            .get(&qname)
            .map(|rrsets| {
                trace!("QUERY RRSETS");
                let mut answer = Answer::new(Rcode::NOERROR);
                if let Some(rrset) = rrsets.iter().find(|rrset| rrset.rtype() == qtype) {
                    trace!("QUERY RRSETS: FOUND {qtype}");
                    answer.add_answer(rrset.deref().clone());
                }
                answer
            })
            .unwrap_or(Answer::new(Rcode::NOERROR)))
    }

    fn walk(&self, op: WalkOp) {
        trace!("WALK");
        for (name, rrsets) in self.tree.read().unwrap().iter() {
            for rrset in rrsets {
                // TODO: Set false to proper value for "at zone cut or not"
                (op)(name.clone(), rrset, false)
            }
        }
        trace!("WALK FINISHED");
    }

    fn is_async(&self) -> bool {
        false
    }
}

impl WritableZone for SimpleZoneInner {
    fn open(
        &self,
        create_diff: bool,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn WritableZoneNode>, std::io::Error>> + Send + Sync>,
    > {
        trace!("OPEN FOR WRITING");
        self.skipped.store(0, Ordering::SeqCst);
        Box::pin(ready(Ok(Box::new(SimpleZoneNode::new(
            self.tree.clone(),
            self.root.clone(),
            self.skipped.clone(),
            self.skip_signed,
        )) as Box<dyn WritableZoneNode>)))
    }

    fn commit(
        &mut self,
        bump_soa_serial: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<InMemoryZoneDiff>, std::io::Error>> + Send + Sync>>
    {
        trace!(
            "COMMITTING: Skipped {} records",
            self.skipped.load(Ordering::SeqCst)
        );
        Box::pin(ready(Ok(None)))
    }
}

struct SimpleZoneNode {
    pub tree: Arc<std::sync::RwLock<BTreeMap<StoredName, HashSet<HashedSharedRrset>>>>,
    pub name: StoredName,
    pub skipped: Arc<AtomicUsize>,
    pub skip_signed: bool,
}

impl SimpleZoneNode {
    fn new(
        tree: Arc<std::sync::RwLock<BTreeMap<StoredName, HashSet<HashedSharedRrset>>>>,
        name: StoredName,
        skipped: Arc<AtomicUsize>,
        skip_signed: bool,
    ) -> Self {
        Self {
            tree,
            name,
            skipped,
            skip_signed,
        }
    }
}

impl WritableZoneNode for SimpleZoneNode {
    fn update_child(
        &self,
        label: &Label,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn WritableZoneNode>, std::io::Error>> + Send + Sync>,
    > {
        let mut builder = NameBuilder::<BytesMut>::new();
        builder.append_label(label.as_slice()).unwrap();
        let child_name = builder.append_origin(&self.name).unwrap();
        let node = Self::new(
            self.tree.clone(),
            child_name,
            self.skipped.clone(),
            self.skip_signed,
        );
        Box::pin(ready(Ok(Box::new(node) as Box<dyn WritableZoneNode>)))
    }

    fn get_rrset(
        &self,
        rtype: Rtype,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SharedRrset>, std::io::Error>> + Send + Sync>>
    {
        let rrset = self
            .tree
            .read()
            .unwrap()
            .get(&self.name)
            .and_then(|v| v.iter().find(|rrset| rrset.rtype() == rtype))
            .map(|hashed_shared_rrset| hashed_shared_rrset.deref().clone());

        Box::pin(ready(Ok(rrset)))
    }

    fn update_rrset(
        &self,
        rrset: SharedRrset,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        // Filter out attempts to add or change DNSSEC records to/in this zone.
        match rrset.rtype() {
            Rtype::DNSKEY
            // | Rtype::DS
            | Rtype::RRSIG
            | Rtype::NSEC
            | Rtype::NSEC3
            | Rtype::NSEC3PARAM
                if self.skip_signed =>
            {
                self.skipped.fetch_add(1, Ordering::SeqCst);
                Box::pin(ready(Ok(())))
            }

            _ => {
                match self.tree.write().unwrap().entry(self.name.clone()) {
                    Entry::Vacant(e) => {
                        // trace!("Inserting first RRSET {rrset:?} at {}", self.name);
                        let _ = e.insert(HashSet::from([HashedSharedRrset(rrset)]));
                    }
                    Entry::Occupied(mut e) => {
                        let new_rrset = HashedSharedRrset(rrset);
                        let rrsets = e.get_mut();
                        match rrsets.take(&new_rrset) {
                            Some(existing_rrset) => {
                                // trace!("Merging {new_rrset:?} into existing RRSET of type {} at {}", new_rrset.rtype(), self.name);
                                // Yeuch, inefficient.
                                let mut new_data = HashSet::new();

                                for data in existing_rrset.data() {
                                    new_data.insert(data.clone());
                                }
                                for data in new_rrset.data() {
                                    new_data.insert(data.clone());
                                }
                                let mut new_new_rrset =
                                    Rrset::new(new_rrset.rtype(), new_rrset.ttl());
                                for data in new_data.drain() {
                                    new_new_rrset.push_data(data);
                                }
                                let _ =
                                    rrsets.insert(HashedSharedRrset(new_new_rrset.into_shared()));
                            }
                            None => {
                                // trace!("Inserting additional RRSET {new_rrset:?} of type {} at {}", new_rrset.rtype(), self.name);
                                let _ = rrsets.insert(new_rrset);
                            }
                        }
                    }
                }

                Box::pin(ready(Ok(())))
            }
        }
    }

    fn remove_rrset(
        &self,
        rtype: Rtype,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        if let Some(rrsets) = self.tree.write().unwrap().get_mut(&self.name) {
            rrsets.retain(|rrset| rrset.rtype() != rtype);
        }

        Box::pin(ready(Ok(())))
    }

    fn make_regular(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        Box::pin(ready(Ok(())))
    }

    fn make_zone_cut(
        &self,
        cut: domain::zonetree::types::ZoneCut,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        Box::pin(ready(Ok(())))
    }

    fn make_cname(
        &self,
        cname: domain::zonetree::SharedRr,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        Box::pin(ready(Ok(())))
    }

    fn remove_all(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        self.tree
            .write()
            .unwrap()
            .clear();
        Box::pin(ready(Ok(())))
    }
}
