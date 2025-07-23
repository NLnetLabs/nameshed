//! Zone-specific state and management.

#![deny(dead_code)]
#![deny(unused_variables)]

use std::{
    borrow::Borrow,
    cmp::Ordering,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, RwLock},
};

use domain::new::base::name::RevName;

pub mod loader;
pub use loader::LoaderState;

pub mod contents;

//----------- Zone -------------------------------------------------------------

/// A zone.
#[derive(Debug)]
pub struct Zone {
    /// The name of this zone.
    pub name: Box<RevName>,

    /// The state of this zone.
    ///
    /// This uses a mutex to ensure that all parts of the zone state are
    /// consistent with each other, and that changes to the zone happen in a
    /// single (sequentially consistent) order.
    pub data: Mutex<ZoneState>,
}

/// The state of a zone.
#[derive(Debug, Default)]
pub struct ZoneState {
    /// Loading new versions of the zone.
    pub loader: LoaderState,
    //
    // TODO:
    // - Policy
    // - A log?
    // - Initialization?
    // - Contents
    // - Loader state
    // - Key manager state
    // - Signer state
    // - Server state
}

impl Zone {
    /// Construct a new [`Zone`].
    ///
    /// The zone is initialized to an empty state, where nothing is known about
    /// it and Nameshed won't act on it.
    pub fn new(name: Box<RevName>) -> Self {
        Self {
            name,
            data: Default::default(),
        }
    }
}

//----------- Zones ------------------------------------------------------------

/// The zones known to Nameshed.
#[derive(Default)]
pub struct Zones {
    /// The internal map of zone names to [`Zone`]s.
    ///
    /// This is write-locked when a zone needs to be added or removed.  Zones
    /// can be looked up using a read-lock.
    map: RwLock<foldhash::HashSet<ZoneByName>>,
}

impl Zones {
    /// Add a new zone.
    ///
    /// The zone will be initialized to an empty state, and a handle to it will
    /// be returned in an [`Ok`].  If the zone already exists, a handle to it
    /// will be returned (like [`Self::get()`]) in an [`Err`].
    pub fn add(&self, name: Box<RevName>) -> Result<Arc<Zone>, Arc<Zone>> {
        let mut map = self
            .map
            .write()
            .expect("the internal RwLock cannot be corrupted");

        if let Some(zone) = map.get(&*name) {
            Err(zone.0.clone())
        } else {
            let zone = Arc::new(Zone::new(name));
            map.insert(ZoneByName(zone.clone()));
            Ok(zone)
        }
    }

    /// Look up a zone.
    pub fn get(&self, name: &RevName) -> Option<Arc<Zone>> {
        self.map
            .read()
            .expect("the internal RwLock cannot be corrupted")
            .get(name)
            .map(|zone| zone.0.clone())
    }
}

//----------- ZoneByName -------------------------------------------------------

/// A [`Zone`] keyed by its name.
#[derive(Clone)]
pub struct ZoneByName(pub Arc<Zone>);

impl Borrow<RevName> for ZoneByName {
    fn borrow(&self) -> &RevName {
        &self.0.name
    }
}

impl PartialEq for ZoneByName {
    fn eq(&self, other: &Self) -> bool {
        self.0.name == other.0.name
    }
}

impl Eq for ZoneByName {}

impl PartialOrd for ZoneByName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ZoneByName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.name.cmp(&other.0.name)
    }
}

impl Hash for ZoneByName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.name.hash(state)
    }
}

//----------- ZoneByPtr --------------------------------------------------------

/// A [`Zone`] keyed by its address in memory.
#[derive(Clone)]
pub struct ZoneByPtr(pub Arc<Zone>);

impl PartialEq for ZoneByPtr {
    fn eq(&self, other: &Self) -> bool {
        Arc::as_ptr(&self.0).cast::<()>() == Arc::as_ptr(&other.0).cast::<()>()
    }
}

impl Eq for ZoneByPtr {}

impl PartialOrd for ZoneByPtr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ZoneByPtr {
    fn cmp(&self, other: &Self) -> Ordering {
        Arc::as_ptr(&self.0)
            .cast::<()>()
            .cmp(&Arc::as_ptr(&other.0).cast::<()>())
    }
}

impl Hash for ZoneByPtr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).cast::<()>().hash(state)
    }
}
