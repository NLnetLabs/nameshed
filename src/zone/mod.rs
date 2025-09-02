//! Zone-specific state and management.

use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    io,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use domain::{
    base::{iana::Class, Name},
    zonetree::{self, ZoneBuilder},
};

use crate::{
    config::Config,
    policy::{Policy, PolicyVersion},
};

pub mod state;

//----------- Zone -------------------------------------------------------------

/// A zone.
#[derive(Debug)]
pub struct Zone {
    /// The name of this zone.
    pub name: Name<Bytes>,

    /// The state of this zone.
    ///
    /// This uses a mutex to ensure that all parts of the zone state are
    /// consistent with each other, and that changes to the zone happen in a
    /// single (sequentially consistent) order.
    pub state: Mutex<ZoneState>,

    /// The loaded contents of the zone.
    pub loaded: zonetree::Zone,

    /// The signed contents of the zone.
    pub signed: zonetree::Zone,

    /// The published contents of the zone.
    pub published: zonetree::Zone,
}

/// The state of a zone.
#[derive(Debug)]
pub struct ZoneState {
    /// The policy (version) used by the zone.
    pub policy: Arc<PolicyVersion>,
    //
    // TODO:
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
    /// it and Cascade won't act on it.
    pub fn new(name: Name<Bytes>, policy: Arc<PolicyVersion>) -> Self {
        Self {
            name: name.clone(),
            state: Mutex::new(ZoneState { policy }),
            loaded: ZoneBuilder::new(name.clone(), Class::IN).build(),
            signed: ZoneBuilder::new(name.clone(), Class::IN).build(),
            published: ZoneBuilder::new(name.clone(), Class::IN).build(),
        }
    }
}

//--- Loading / Saving

impl Zone {
    /// Reload the state of this zone.
    pub fn reload_state(
        self: &Arc<Self>,
        policies: &mut foldhash::HashMap<Box<str>, Policy>,
        config: &Config,
    ) -> io::Result<()> {
        // Load and parse the state file.
        let path = config.zone_state_dir.join(format!("{}.state", self.name));
        let spec = state::Spec::load(&path)?;

        // Merge the parsed data.
        let mut state = self.state.lock().unwrap();
        spec.parse_into(self, &mut state, policies);

        Ok(())
    }

    /// Save the state of this zone.
    pub fn save_state(self: &Arc<Self>, config: &Config) -> io::Result<()> {
        // Read the state out.
        let spec = {
            let state = self.state.lock().unwrap();
            state::Spec::build(&state)
        };

        // Build and write the state file.
        let path = config.zone_state_dir.join(format!("{}.state", self.name));
        spec.save(&path)
    }
}

//----------- ZoneByName -------------------------------------------------------

/// A [`Zone`] keyed by its name.
#[derive(Clone)]
pub struct ZoneByName(pub Arc<Zone>);

impl Borrow<Name<Bytes>> for ZoneByName {
    fn borrow(&self) -> &Name<Bytes> {
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

impl fmt::Debug for ZoneByName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
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

impl fmt::Debug for ZoneByPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
