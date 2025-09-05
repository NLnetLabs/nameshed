//! Version 1 of the state file.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use camino::Utf8Path;
use domain::base::Name;
use serde::{Deserialize, Serialize};

use crate::{
    center::{Change, State},
    config::{self, Config},
    policy::{
        KeyManagerPolicy, LoaderPolicy, Nsec3OptOutPolicy, Policy, PolicyVersion, ReviewPolicy,
        ServerPolicy, SignerDenialPolicy, SignerPolicy, SignerSerialPolicy,
    },
    util::update_value,
    zone::{Zone, ZoneByName},
};

//----------- Spec -------------------------------------------------------------

/// A state file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Spec {
    /// Configuration.
    ///
    /// This does not include configuration from environment variables or the
    /// command line, as they can change between invocations.
    pub config: ConfigSpec,

    /// Known zones.
    ///
    /// Only the names of the zones are stored here.  The state of each zone is
    /// stored in a dedicated state file.
    pub zones: foldhash::HashSet<Name<Bytes>>,

    /// Policies.
    pub policies: foldhash::HashMap<Box<str>, PolicySpec>,
}

//--- Conversion

impl Spec {
    /// Parse from this specification.
    pub fn parse_into(self, state: &mut State, changes: &mut Vec<Change>) {
        // Update the configuration.
        let mut changed = false;
        self.config.parse_into(&mut state.config, &mut changed);
        if changed {
            changes.push(Change::ConfigChanged);
        }

        // Update the policy set.
        let mut new_policies = foldhash::HashMap::default();
        for (name, spec) in self.policies {
            let policy = match state.policies.remove(&name) {
                Some(mut policy) => {
                    log::trace!("Retaining existing policy '{name}'");
                    let mut changed = false;
                    spec.parse_into(&mut policy, &mut changed);
                    if changed {
                        changes.extend(policy.zones.iter().map(|zone| {
                            Change::ZonePolicyChanged(zone.clone(), policy.latest.clone())
                        }));
                    }
                    policy
                }
                None => {
                    log::info!("Adding policy '{name}' from global state");
                    spec.parse(&name)
                }
            };
            new_policies.insert(name, policy);
        }
        for (name, policy) in state.policies.drain() {
            if !policy.zones.is_empty() {
                log::error!("The policy '{name}' has been removed from the global state, but some zones are still using it; Cascade will preserve its internal copy");
                new_policies.insert(name, policy);
            } else {
                log::info!("Removing policy '{name}'");
            }
        }
        state.policies = new_policies;

        // Update the zone set.
        #[allow(clippy::mutable_key_type)]
        let new_zones = self
            .zones
            .into_iter()
            .map(|name| match state.zones.take(&name) {
                Some(zone) => {
                    log::trace!("Retaining existing zone '{name}'");
                    zone
                }
                None => {
                    log::info!("Adding zone '{name}' from global state");
                    ZoneByName(Arc::new(Zone::new(name.clone())))
                }
            })
            .collect();
        for zone in state.zones.drain() {
            log::info!("Removing zone '{}'", zone.0.name);
            changes.push(Change::ZoneRemoved(zone.0.name.clone()));
        }
        state.zones = new_zones;
    }

    /// Build this state specification.
    pub fn build(state: &State) -> Self {
        Self {
            config: ConfigSpec::build(&state.config),
            zones: state.zones.iter().map(|zone| zone.0.name.clone()).collect(),
            policies: state
                .policies
                .iter()
                .map(|(name, policy)| (name.clone(), PolicySpec::build(policy)))
                .collect(),
        }
    }
}

//----------- ConfigSpec -------------------------------------------------------

/// Configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConfigSpec {
    /// The directory storing policy files.
    pub policy_dir: Box<Utf8Path>,

    /// The directory storing zone state files.
    pub zone_state_dir: Box<Utf8Path>,

    /// The file storing TSIG keys.
    pub tsig_store_path: Box<Utf8Path>,

    /// Path to the directory where the keys should be stored.
    pub keys_dir: Box<Utf8Path>,

    /// Path to the dnst binary that Cascade should use.
    pub dnst_binary_path: Box<Utf8Path>,

    /// Daemon-related configuration.
    pub daemon: DaemonConfigSpec,

    /// The configuration of the zone loader.
    pub loader: LoaderConfigSpec,

    /// The configuration of the zone signer.
    pub signer: SignerConfigSpec,

    /// The configuration of the key manager.
    pub key_manager: KeyManagerConfigSpec,

    /// The configuration of the zone server.
    pub server: ServerConfigSpec,
}

//--- Conversion

impl ConfigSpec {
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut Config, changed: &mut bool) {
        update_value(&mut config.policy_dir, self.policy_dir, changed);
        update_value(&mut config.zone_state_dir, self.zone_state_dir, changed);
        update_value(&mut config.tsig_store_path, self.tsig_store_path, changed);
        update_value(&mut config.keys_dir, self.keys_dir, changed);
        update_value(&mut config.dnst_binary_path, self.dnst_binary_path, changed);
        self.daemon.parse_into(&mut config.daemon, changed);
        update_value(&mut config.loader, self.loader.parse(), changed);
        update_value(&mut config.signer, self.signer.parse(), changed);
        update_value(&mut config.key_manager, self.key_manager.parse(), changed);
        update_value(&mut config.server, self.server.parse(), changed);
    }

    /// Build this state specification.
    pub fn build(config: &Config) -> Self {
        Self {
            policy_dir: config.policy_dir.clone(),
            zone_state_dir: config.zone_state_dir.clone(),
            tsig_store_path: config.tsig_store_path.clone(),
            keys_dir: config.keys_dir.clone(),
            dnst_binary_path: config.dnst_binary_path.clone(),
            daemon: DaemonConfigSpec::build(&config.daemon),
            loader: LoaderConfigSpec::build(&config.loader),
            signer: SignerConfigSpec::build(&config.signer),
            key_manager: KeyManagerConfigSpec::build(&config.key_manager),
            server: ServerConfigSpec::build(&config.server),
        }
    }
}

//----------- DaemonConfigSpec -------------------------------------------------

/// Daemon-related configuration for Cascade.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DaemonConfigSpec {
    /// Logging configuration.
    pub logging: LoggingConfigSpec,

    /// The location of the configuration file.
    pub config_file: Option<Box<Utf8Path>>,

    /// Whether Cascade should fork on startup.
    pub daemonize: Option<bool>,

    /// The path to a PID file to maintain.
    pub pid_file: Option<Box<Utf8Path>>,

    /// The directory to chroot into after startup.
    pub chroot: Option<Box<Utf8Path>>,

    /// The identity to assume after startup.
    pub identity: Option<(UserId, GroupId)>,
}

//--- Conversion

impl DaemonConfigSpec {
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut config::DaemonConfig, changed: &mut bool) {
        self.logging.parse_into(&mut config.logging, changed);
        update_value(&mut config.config_file.file, self.config_file, changed);
        update_value(&mut config.daemonize.file, self.daemonize, changed);
        update_value(&mut config.pid_file, self.pid_file, changed);
        update_value(&mut config.chroot, self.chroot, changed);
        update_value(
            &mut config.identity,
            self.identity.map(|(u, g)| (u.into(), g.into())),
            changed,
        );
    }

    /// Build this state specification.
    pub fn build(config: &config::DaemonConfig) -> Self {
        Self {
            logging: LoggingConfigSpec::build(&config.logging),
            config_file: config.config_file.file.clone(),
            daemonize: config.daemonize.file,
            pid_file: config.pid_file.clone(),
            chroot: config.chroot.clone(),
            identity: config.identity.clone().map(|(u, g)| (u.into(), g.into())),
        }
    }
}

//----------- LoggingConfigSpec ------------------------------------------------

/// Logging configuration for Cascade.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LoggingConfigSpec {
    /// The minimum severity of messages to log.
    pub level: Option<LogLevel>,

    /// Where to log messages to.
    pub target: Option<LogTarget>,

    /// Targets to log trace messages for.
    pub trace_targets: Option<foldhash::HashSet<Box<str>>>,
}

//--- Conversion

impl LoggingConfigSpec {
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut config::LoggingConfig, changed: &mut bool) {
        update_value(
            &mut config.level.file,
            self.level.map(|v| v.into()),
            changed,
        );
        update_value(
            &mut config.target.file,
            self.target.map(|v| v.into()),
            changed,
        );
        update_value(&mut config.trace_targets.file, self.trace_targets, changed);
    }

    /// Build this state specification.
    pub fn build(config: &config::LoggingConfig) -> Self {
        Self {
            level: config.level.file.map(|v| v.into()),
            target: config.target.file.clone().map(|v| v.into()),
            trace_targets: config.trace_targets.file.clone(),
        }
    }
}

//----------- UserId -----------------------------------------------------------

/// A numeric or named user ID.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum UserId {
    /// A numeric ID.
    Numeric(u32),

    /// A user name.
    Named(Box<str>),
}

//--- Conversion

impl From<config::UserId> for UserId {
    fn from(value: config::UserId) -> Self {
        match value {
            config::UserId::Numeric(v) => Self::Numeric(v),
            config::UserId::Named(v) => Self::Named(v),
        }
    }
}

impl From<UserId> for config::UserId {
    fn from(value: UserId) -> Self {
        match value {
            UserId::Numeric(v) => Self::Numeric(v),
            UserId::Named(v) => Self::Named(v),
        }
    }
}

//----------- GroupId ----------------------------------------------------------

/// A numeric or named group ID.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum GroupId {
    /// A numeric ID.
    Numeric(u32),

    /// A group name.
    Named(Box<str>),
}

//--- Conversion

impl From<config::GroupId> for GroupId {
    fn from(value: config::GroupId) -> Self {
        match value {
            config::GroupId::Numeric(v) => Self::Numeric(v),
            config::GroupId::Named(v) => Self::Named(v),
        }
    }
}

impl From<GroupId> for config::GroupId {
    fn from(value: GroupId) -> Self {
        match value {
            GroupId::Numeric(v) => Self::Numeric(v),
            GroupId::Named(v) => Self::Named(v),
        }
    }
}

//----------- LoaderConfigSpec -------------------------------------------------

/// Configuration for the zone loader.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LoaderConfigSpec {
    /// Where to listen for zone update notifications.
    pub notif_listeners: Vec<SocketConfigSpec>,

    /// Configuration for reviewing loaded zones.
    pub review: ReviewConfigSpec,
}

//--- Conversion

impl LoaderConfigSpec {
    /// Parse from this specification.
    pub fn parse(self) -> config::LoaderConfig {
        config::LoaderConfig {
            notif_listeners: self.notif_listeners.into_iter().map(|v| v.into()).collect(),
            review: self.review.parse(),
        }
    }

    /// Build this state specification.
    pub fn build(config: &config::LoaderConfig) -> Self {
        Self {
            notif_listeners: config
                .notif_listeners
                .iter()
                .map(|v| v.clone().into())
                .collect(),
            review: ReviewConfigSpec::build(&config.review),
        }
    }
}

//----------- SignerConfigSpec -------------------------------------------------

/// Configuration for the zone signer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SignerConfigSpec {
    /// Configuration for reviewing signed zones.
    pub review: ReviewConfigSpec,
}

//--- Conversion

impl SignerConfigSpec {
    /// Parse from this specification.
    pub fn parse(self) -> config::SignerConfig {
        config::SignerConfig {
            review: self.review.parse(),
        }
    }

    /// Build this state specification.
    pub fn build(config: &config::SignerConfig) -> Self {
        Self {
            review: ReviewConfigSpec::build(&config.review),
        }
    }
}

//----------- ReviewConfigSpec -------------------------------------------------

/// Configuration for reviewing zones.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReviewConfigSpec {
    /// Where to serve zones for review.
    pub servers: Vec<SocketConfigSpec>,
}

//--- Conversion

impl ReviewConfigSpec {
    /// Parse from this specification.
    pub fn parse(self) -> config::ReviewConfig {
        config::ReviewConfig {
            servers: self.servers.into_iter().map(|v| v.into()).collect(),
        }
    }

    /// Build this state specification.
    pub fn build(config: &config::ReviewConfig) -> Self {
        Self {
            servers: config.servers.iter().map(|v| v.clone().into()).collect(),
        }
    }
}

//----------- KeyManagerConfigSpec ---------------------------------------------

/// Configuration for the key manager.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KeyManagerConfigSpec {}

//--- Conversion

impl KeyManagerConfigSpec {
    /// Parse from this specification.
    pub fn parse(self) -> config::KeyManagerConfig {
        config::KeyManagerConfig {}
    }

    /// Build this state specification.
    pub fn build(config: &config::KeyManagerConfig) -> Self {
        let config::KeyManagerConfig {} = config;
        Self {}
    }
}

//----------- ServerConfigSpec -------------------------------------------------

/// Configuration for the zone server.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServerConfigSpec {
    /// Where to serve zones.
    pub servers: Vec<SocketConfigSpec>,
}

//--- Conversion

impl ServerConfigSpec {
    /// Parse from this specification.
    pub fn parse(self) -> config::ServerConfig {
        config::ServerConfig {
            servers: self.servers.into_iter().map(|v| v.into()).collect(),
        }
    }

    /// Build this state specification.
    pub fn build(config: &config::ServerConfig) -> Self {
        Self {
            servers: config.servers.iter().map(|v| v.clone().into()).collect(),
        }
    }
}

//----------- SocketConfigSpec -------------------------------------------------

/// Configuration for serving / listening on a network socket.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum SocketConfigSpec {
    /// Listen exclusively over UDP.
    Udp {
        /// The socket address to listen on.
        addr: SocketAddr,
    },

    /// Listen exclusively over TCP.
    Tcp {
        /// The socket address to listen on.
        addr: SocketAddr,
    },

    /// Listen over both TCP and UDP.
    TcpUdp {
        /// The socket address to listen on.
        addr: SocketAddr,
    },
}

//--- Conversion

impl From<config::SocketConfig> for SocketConfigSpec {
    fn from(value: config::SocketConfig) -> Self {
        match value {
            config::SocketConfig::UDP { addr } => Self::Udp { addr },
            config::SocketConfig::TCP { addr } => Self::Tcp { addr },
            config::SocketConfig::TCPUDP { addr } => Self::TcpUdp { addr },
        }
    }
}

impl From<SocketConfigSpec> for config::SocketConfig {
    fn from(value: SocketConfigSpec) -> Self {
        match value {
            SocketConfigSpec::Udp { addr } => Self::UDP { addr },
            SocketConfigSpec::Tcp { addr } => Self::TCP { addr },
            SocketConfigSpec::TcpUdp { addr } => Self::TCPUDP { addr },
        }
    }
}

//----------- LogLevel ---------------------------------------------------------

/// A severity level for logging.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum LogLevel {
    /// A function or variable was interacted with, for debugging.
    Trace,

    /// Something occurred that may be relevant to debugging.
    Debug,

    /// Things are proceeding as expected.
    Info,

    /// Something does not appear to be correct.
    Warning,

    /// Something is wrong (but Cascade can recover).
    Error,

    /// Something is wrong and Cascade can't function at all.
    Critical,
}

//--- Conversion

impl From<config::LogLevel> for LogLevel {
    fn from(value: config::LogLevel) -> Self {
        match value {
            config::LogLevel::Trace => Self::Trace,
            config::LogLevel::Debug => Self::Debug,
            config::LogLevel::Info => Self::Info,
            config::LogLevel::Warning => Self::Warning,
            config::LogLevel::Error => Self::Error,
            config::LogLevel::Critical => Self::Critical,
        }
    }
}

impl From<LogLevel> for config::LogLevel {
    fn from(value: LogLevel) -> Self {
        match value {
            LogLevel::Trace => Self::Trace,
            LogLevel::Debug => Self::Debug,
            LogLevel::Info => Self::Info,
            LogLevel::Warning => Self::Warning,
            LogLevel::Error => Self::Error,
            LogLevel::Critical => Self::Critical,
        }
    }
}

//----------- LogTarget --------------------------------------------------------

/// A logging target.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum LogTarget {
    /// Append logs to a file.
    ///
    /// If the file is a terminal, ANSI color codes may be used.
    File {
        /// The path to the file.
        path: Box<Utf8Path>,
    },

    /// Write logs to the UNIX syslog.
    Syslog,
}

//--- Conversion

impl From<config::LogTarget> for LogTarget {
    fn from(value: config::LogTarget) -> Self {
        match value {
            config::LogTarget::File(path) => Self::File { path },
            config::LogTarget::Syslog => Self::Syslog,
        }
    }
}

impl From<LogTarget> for config::LogTarget {
    fn from(value: LogTarget) -> Self {
        match value {
            LogTarget::File { path } => Self::File(path),
            LogTarget::Syslog => Self::Syslog,
        }
    }
}

//----------- PolicySpec -------------------------------------------------------

/// A policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PolicySpec {
    /// The latest version of the policy.
    pub latest: PolicyVersionSpec,

    /// Whether the policy is being deleted.
    pub mid_deletion: bool,
}

//--- Conversion

impl PolicySpec {
    /// Parse from this specification.
    pub fn parse(self, name: &str) -> Policy {
        Policy {
            latest: Arc::new(self.latest.parse(name)),
            mid_deletion: self.mid_deletion,
            zones: Default::default(),
        }
    }

    /// Merge from this specification.
    pub fn parse_into(self, policy: &mut Policy, changed: &mut bool) {
        let name = &policy.latest.name;
        let latest = Arc::new(self.latest.parse(name));
        update_value(&mut policy.latest, latest, changed);
        // TODO: How does this affect zones using the policy?
        policy.mid_deletion |= self.mid_deletion;
    }

    /// Build into this specification.
    pub fn build(policy: &Policy) -> Self {
        Self {
            latest: PolicyVersionSpec::build(&policy.latest),
            mid_deletion: policy.mid_deletion,
        }
    }
}

//----------- PolicyVersionSpec ------------------------------------------------

/// A particular version of a policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PolicyVersionSpec {
    /// How zones are loaded.
    pub loader: LoaderPolicySpec,

    /// Zone key management.
    pub key_manager: KeyManagerPolicySpec,

    /// How zones are signed.
    pub signer: SignerPolicySpec,

    /// How zones are served.
    pub server: ServerPolicySpec,
}

//--- Conversion

impl PolicyVersionSpec {
    /// Parse from this specification.
    pub fn parse(self, name: &str) -> PolicyVersion {
        PolicyVersion {
            name: name.into(),
            loader: self.loader.parse(),
            key_manager: self.key_manager.parse(),
            signer: self.signer.parse(),
            server: self.server.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &PolicyVersion) -> Self {
        Self {
            loader: LoaderPolicySpec::build(&policy.loader),
            key_manager: KeyManagerPolicySpec::build(&policy.key_manager),
            signer: SignerPolicySpec::build(&policy.signer),
            server: ServerPolicySpec::build(&policy.server),
        }
    }
}

//----------- LoaderSpec -------------------------------------------------------

/// Policy for loading zones.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LoaderPolicySpec {
    /// Reviewing loaded zones.
    pub review: ReviewPolicySpec,
}

//--- Conversion

impl LoaderPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> LoaderPolicy {
        LoaderPolicy {
            review: self.review.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &LoaderPolicy) -> Self {
        Self {
            review: ReviewPolicySpec::build(&policy.review),
        }
    }
}

//----------- KeyManagerSpec ---------------------------------------------------

/// Policy for zone key management.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KeyManagerPolicySpec {}

//--- Conversion

impl KeyManagerPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> KeyManagerPolicy {
        KeyManagerPolicy {}
    }

    /// Build into this specification.
    pub fn build(policy: &KeyManagerPolicy) -> Self {
        let KeyManagerPolicy {} = policy;
        Self {}
    }
}

//----------- SignerPolicySpec -------------------------------------------------

/// Policy for signing zones.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SignerPolicySpec {
    /// The serial number generation policy.
    pub serial_policy: SignerSerialPolicySpec,

    /// The offset for record signature inceptions, in seconds.
    pub sig_inception_offset: u64,

    /// How long record signatures will be valid for, in seconds.
    pub sig_validity_time: u64,

    /// How denial-of-existence records are generated.
    pub denial: SignerDenialPolicySpec,

    /// Reviewing signed zones.
    pub review: ReviewPolicySpec,
}

//--- Conversion

impl SignerPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerPolicy {
        SignerPolicy {
            serial_policy: self.serial_policy.parse(),
            sig_inception_offset: Duration::from_secs(self.sig_inception_offset),
            sig_validity_time: Duration::from_secs(self.sig_validity_time),
            denial: self.denial.parse(),
            review: self.review.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &SignerPolicy) -> Self {
        Self {
            serial_policy: SignerSerialPolicySpec::build(policy.serial_policy),
            sig_inception_offset: policy.sig_inception_offset.as_secs(),
            sig_validity_time: policy.sig_validity_time.as_secs(),
            denial: SignerDenialPolicySpec::build(&policy.denial),
            review: ReviewPolicySpec::build(&policy.review),
        }
    }
}

//----------- SignerSerialPolicySpec -------------------------------------------

/// Policy for generating serial numbers.
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum SignerSerialPolicySpec {
    /// Use the same serial number as the unsigned zone.
    Keep,

    /// Increment the serial number on every change.
    Counter,

    /// Use the current Unix time, in seconds.
    ///
    /// New versions of the zone cannot be generated in the same second.
    UnixTime,

    /// Set the serial number to `<YYYY><MM><DD><xx>`.
    DateCounter,
}

//--- Conversion

impl SignerSerialPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerSerialPolicy {
        match self {
            Self::Keep => SignerSerialPolicy::Keep,
            Self::Counter => SignerSerialPolicy::Counter,
            Self::UnixTime => SignerSerialPolicy::UnixTime,
            Self::DateCounter => SignerSerialPolicy::DateCounter,
        }
    }

    /// Build into this specification.
    pub fn build(policy: SignerSerialPolicy) -> Self {
        match policy {
            SignerSerialPolicy::Keep => Self::Keep,
            SignerSerialPolicy::Counter => Self::Counter,
            SignerSerialPolicy::UnixTime => Self::UnixTime,
            SignerSerialPolicy::DateCounter => Self::DateCounter,
        }
    }
}

//----------- SignerDenialPolicySpec -------------------------------------------

/// Spec for generating denial-of-existence records.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum SignerDenialPolicySpec {
    /// Generate NSEC records.
    NSec,

    /// Generate NSEC3 records.
    NSec3 {
        /// Whether and how to enable NSEC3 Opt-Out.
        opt_out: Nsec3OptOutPolicySpec,
    },
}

//--- Conversion

impl SignerDenialPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerDenialPolicy {
        match self {
            SignerDenialPolicySpec::NSec => SignerDenialPolicy::NSec,
            SignerDenialPolicySpec::NSec3 { opt_out } => SignerDenialPolicy::NSec3 {
                opt_out: opt_out.parse(),
            },
        }
    }

    /// Build into this specification.
    pub fn build(policy: &SignerDenialPolicy) -> Self {
        match *policy {
            SignerDenialPolicy::NSec => SignerDenialPolicySpec::NSec,
            SignerDenialPolicy::NSec3 { opt_out } => SignerDenialPolicySpec::NSec3 {
                opt_out: Nsec3OptOutPolicySpec::build(opt_out),
            },
        }
    }
}

//----------- Nsec3OptOutPolicySpec --------------------------------------------

/// Spec for the NSEC3 Opt-Out mechanism.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum Nsec3OptOutPolicySpec {
    /// Do not enable Opt-Out.
    Disabled,

    /// Only set the Opt-Out flag.
    FlagOnly,

    /// Enable Opt-Out and omit the corresponding NSEC3 records.
    Enabled,
}

//--- Conversion

impl Nsec3OptOutPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> Nsec3OptOutPolicy {
        match self {
            Nsec3OptOutPolicySpec::Disabled => Nsec3OptOutPolicy::Disabled,
            Nsec3OptOutPolicySpec::FlagOnly => Nsec3OptOutPolicy::FlagOnly,
            Nsec3OptOutPolicySpec::Enabled => Nsec3OptOutPolicy::Enabled,
        }
    }

    /// Build into this specification.
    pub fn build(policy: Nsec3OptOutPolicy) -> Self {
        match policy {
            Nsec3OptOutPolicy::Disabled => Nsec3OptOutPolicySpec::Disabled,
            Nsec3OptOutPolicy::FlagOnly => Nsec3OptOutPolicySpec::FlagOnly,
            Nsec3OptOutPolicy::Enabled => Nsec3OptOutPolicySpec::Enabled,
        }
    }
}

//----------- ReviewSpec -------------------------------------------------------

/// Policy for reviewing loaded/signed zones.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReviewPolicySpec {
    /// Whether review is required.
    pub required: bool,

    /// A command hook for reviewing a new version of the zone.
    pub cmd_hook: Option<String>,
}

//--- Conversion

impl ReviewPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> ReviewPolicy {
        ReviewPolicy {
            required: self.required,
            cmd_hook: self.cmd_hook,
        }
    }

    /// Build into this specification.
    pub fn build(policy: &ReviewPolicy) -> Self {
        Self {
            required: policy.required,
            cmd_hook: policy.cmd_hook.clone(),
        }
    }
}

//----------- ServerSpec -------------------------------------------------------

/// Policy for serving zones.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServerPolicySpec {}

//--- Conversion

impl ServerPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> ServerPolicy {
        ServerPolicy {}
    }

    /// Build into this specification.
    pub fn build(policy: &ServerPolicy) -> Self {
        let ServerPolicy {} = policy;
        Self {}
    }
}
