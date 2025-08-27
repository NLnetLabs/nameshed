//! Nameshed's central command.

use std::sync::Mutex;

use crate::{config::Config, policy::Policy, tsig::TsigStore, zone::ZoneByName};

//----------- Center -----------------------------------------------------------

/// Nameshed's central command.
pub struct Center {
    /// Global state.
    pub state: Mutex<State>,
}

//--- Initialization

impl Center {
    /// Initialize Nameshed.
    pub fn launch(config: Config) -> Self {
        Self {
            state: Mutex::new(State::new(config)),
        }
    }
}

//----------- State ------------------------------------------------------------

/// Global state for Nameshed.
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
