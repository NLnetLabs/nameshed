//! Cascade's central command.

use std::{
    fmt, io,
    sync::{Arc, Mutex},
    time::Duration,
};

use arc_swap::ArcSwap;
use bytes::Bytes;
use domain::{base::Name, zonetree::ZoneTree};
use tokio::sync::mpsc;

use crate::{
    comms::ApplicationCommand,
    config::Config,
    log::Logger,
    payload::Update,
    policy::{Policy, PolicyVersion},
    tsig::TsigStore,
    zone::{Zone, ZoneByName},
};

//----------- Center -----------------------------------------------------------

/// Cascade's central command.
#[derive(Debug)]
pub struct Center {
    /// Global state.
    pub state: Mutex<State>,

    /// The logger.
    pub logger: &'static Logger,

    /// The latest unsigned contents of all zones.
    pub unsigned_zones: Arc<ArcSwap<ZoneTree>>,

    /// The latest signed contents of all zones.
    pub signed_zones: Arc<ArcSwap<ZoneTree>>,

    /// The latest published contents of all zones.
    pub published_zones: Arc<ArcSwap<ZoneTree>>,

    /// The old TSIG key store.
    pub old_tsig_key_store: crate::common::tsig::TsigKeyStore,

    /// A channel to send units commands.
    pub app_cmd_tx: mpsc::UnboundedSender<(String, ApplicationCommand)>,

    /// A channel to send the central command updates.
    pub update_tx: mpsc::UnboundedSender<Update>,
}

//--- Actions

/// Add a zone.
pub fn add_zone(center: &Arc<Center>, name: Name<Bytes>) -> Result<(), ZoneAddError> {
    let zone = Arc::new(Zone::new(name.clone()));
    {
        let mut state = center.state.lock().unwrap();
        if !state.zones.insert(ZoneByName(zone.clone())) {
            return Err(ZoneAddError::AlreadyExists);
        }

        center
            .update_tx
            .send(Update::Changed(Change::ZoneAdded(name.clone())))
            .unwrap();

        state.mark_dirty(center);
    }

    {
        let mut state = zone.state.lock().unwrap();
        zone.mark_dirty(&mut state, center);
    }

    log::info!("Added zone '{name}'");
    Ok(())
}

/// Remove a zone.
pub fn remove_zone(center: &Arc<Center>, name: Name<Bytes>) -> Result<(), ZoneRemoveError> {
    center
        .update_tx
        .send(Update::Changed(Change::ZoneRemoved(name.clone())))
        .unwrap();

    let mut state = center.state.lock().unwrap();
    let zone = state.zones.take(&name).ok_or(ZoneRemoveError::NotFound)?;
    let mut zone_state = zone.0.state.lock().unwrap();

    // Update the policy's referenced zones.
    if let Some(policy) = zone_state.policy.take() {
        let policy = state
            .policies
            .get_mut(&policy.name)
            .expect("every zone policy exists");
        assert!(policy.zones.remove(&name), "zone policies are consistent");

        state.mark_dirty(center);
    }

    log::info!("Removed zone '{name}'");
    Ok(())
}

//----------- State ------------------------------------------------------------

/// Global state for Cascade.
#[derive(Debug)]
pub struct State {
    /// Configuration.
    ///
    /// This need not correspond to the live contents of the configuration file;
    /// this field is only refreshed when the user requests it.
    pub config: Config,

    /// Known zones.
    ///
    /// This field stores the live state of every zone.  Crucially, zones are
    /// concurrently accessible, as each one is locked behind a unique mutex.
    pub zones: foldhash::HashSet<ZoneByName>,

    /// Zone policies.
    ///
    /// A policy provides is a template for zone configuration, that can be used
    /// by many zones simultaneously.  It is the primary way to configure zones.
    ///
    /// This map points to the latest known version of each policy.  Changes to
    /// the policy result in new commits, which the associated zones are
    /// gradually transitioned to.
    ///
    /// Like global configuration, these are only reloaded on user request.
    pub policies: foldhash::HashMap<Box<str>, Policy>,

    /// The TSIG key store.
    ///
    /// TSIG keys are used for authenticating Cascade to zone sources, and for
    /// authenticating incoming requests for zones.
    pub tsig_store: TsigStore,

    /// An enqueued save of this state.
    ///
    /// The enqueued save operation will persist the current state in a short
    /// duration of time.  If the field is `None`, and the state is changed, a
    /// new save operation should be enqueued.
    pub enqueued_save: Option<tokio::task::JoinHandle<()>>,
}

//--- Initialization

impl State {
    /// Build a new Cascade state.
    ///
    /// A new instance of Cascade is initialized with a blank state.  If a
    /// previous state file exists, it can be imported afterwards.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            zones: Default::default(),
            policies: Default::default(),
            tsig_store: Default::default(),
            enqueued_save: None,
        }
    }

    /// Attempt to load the global state file.
    pub fn init_from_file(&mut self) -> io::Result<()> {
        let path = self.config.daemon.state_file.value();
        let spec = crate::state::Spec::load(path)?;
        let mut _changes = Vec::new();
        spec.parse_into(self, &mut _changes);
        Ok(())
    }

    /// Mark the global state as dirty.
    ///
    /// A persistence operation for the global state will be enqueued (unless
    /// one already exists), so that it will be saved in the near future.
    pub fn mark_dirty(&mut self, center: &Arc<Center>) {
        if self.enqueued_save.is_some() {
            // A save is already enqueued; nothing to do.
            return;
        }

        // Enqueue a new save.
        let center = center.clone();
        let task = tokio::spawn(async move {
            // TODO: Make this time configurable.
            tokio::time::sleep(Duration::from_secs(5)).await;

            let (path, spec);
            {
                // Load the global state.
                let mut state = center.state.lock().unwrap();
                let Some(_) = state.enqueued_save.take_if(|s| s.id() == tokio::task::id()) else {
                    // 'enqueued_save' does not match what we set, so somebody
                    // else set it to 'None' first.  Don't do anything.
                    log::trace!("Ignoring enqueued save due to race");
                    return;
                };

                path = state.config.daemon.state_file.value().clone();
                spec = crate::state::Spec::build(&state);
            }

            // Save the global state.
            match spec.save(&path) {
                Ok(()) => log::debug!("Saved global state (to '{path}')"),
                Err(err) => {
                    log::error!("Could not save global state to '{path}': {err}");
                }
            }
        });
        self.enqueued_save = Some(task);
    }
}

//----------- Change -----------------------------------------------------------

/// A change to global state.
#[derive(Clone, Debug)]
pub enum Change {
    /// The configuration has been changed.
    ConfigChanged,

    /// A zone has been added.
    ZoneAdded(Name<Bytes>),

    /// The policy of a zone has changed.
    ZonePolicyChanged(Name<Bytes>, Arc<PolicyVersion>),

    /// A zone has been removed.
    ZoneRemoved(Name<Bytes>),
}

//----------- ZoneAddError -----------------------------------------------------

/// An error adding a zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZoneAddError {
    /// A zone of the same name already exists.
    AlreadyExists,
}

impl std::error::Error for ZoneAddError {}

impl fmt::Display for ZoneAddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyExists => "a zone of this name already exists",
        })
    }
}

//----------- ZoneRemoveError --------------------------------------------------

/// An error removing a zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZoneRemoveError {
    /// No such name could be found.
    NotFound,
}

impl std::error::Error for ZoneRemoveError {}

impl fmt::Display for ZoneRemoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "no such zone was found",
        })
    }
}
