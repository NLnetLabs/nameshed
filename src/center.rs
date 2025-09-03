//! Nameshed's central command.

use std::{
    io,
    sync::{Arc, Mutex},
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
    zone::ZoneByName,
};

//----------- Center -----------------------------------------------------------

/// Nameshed's central command.
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

//--- Saving/Loading

impl Center {
    /// Save Cascade's state to disk.
    pub fn save(&self) {
        let state_path;
        let state_spec;
        let zone_state_dir;
        let zone_states: foldhash::HashMap<_, _>;

        // Read everything from the global state.
        {
            let state = self.state.lock().unwrap();

            state_spec = crate::state::Spec::build(&state);
            state_path = state.config.daemon.state_file.value().clone();

            zone_state_dir = state.config.zone_state_dir.clone();
            zone_states = state
                .zones
                .iter()
                .map(|zone| {
                    let name = zone.0.name.clone();
                    let state = zone.0.state.lock().unwrap();
                    let spec = crate::zone::state::Spec::build(&state);
                    (name, spec)
                })
                .collect();
        }

        // Save the global state file.
        match state_spec.save(&state_path) {
            Ok(()) => log::debug!("Saved global state"),
            Err(err) => {
                log::error!("Could not save global state to '{state_path}': {err}");
            }
        }

        // Save the per-zone state files.
        for (name, spec) in zone_states {
            let path = zone_state_dir.join(name.to_string());
            match spec.save(&path) {
                Ok(()) => log::debug!("Saved state of zone '{name}'"),
                Err(err) => {
                    log::error!("Could not save state of zone '{name}' to '{path}': '{err}");
                }
            }
        }
    }
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
    /// TSIG keys are used for authenticating Nameshed to zone sources, and for
    /// authenticating incoming requests for zones.
    pub tsig_store: TsigStore,
}

//--- Initialization

impl State {
    /// Build a new Nameshed state.
    ///
    /// A new instance of Nameshed is initialized with a blank state.  If a
    /// previous state file exists, it can be imported afterwards.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            zones: Default::default(),
            policies: Default::default(),
            tsig_store: Default::default(),
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
