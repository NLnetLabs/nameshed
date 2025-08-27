//! Nameshed's central command.

use std::sync::{Arc, Mutex};

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
