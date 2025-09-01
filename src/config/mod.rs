//! Configuring Cascade.
//!
//! As per convention, Cascade is configured from three sources (from least to
//! most specific): configuration files, environment variables, and command-line
//! arguments.  This module defines and collects together these sources.

use std::{
    fmt,
    hash::{Hash, Hasher},
    net::SocketAddr,
};

use camino::Utf8Path;

pub mod args;
pub mod env;
pub mod file;

//----------- Config -----------------------------------------------------------

/// Configuration for Cascade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The directory storing policy files.
    pub policy_dir: Box<Utf8Path>,

    /// The directory storing zone state files.
    pub zone_state_dir: Box<Utf8Path>,

    /// The file storing TSIG keys.
    pub tsig_store_path: Box<Utf8Path>,

    /// Daemon-related configuration.
    pub daemon: DaemonConfig,

    /// The configuration of the zone loader.
    pub loader: LoaderConfig,

    /// The configuration of the zone signer.
    pub signer: SignerConfig,

    /// The configuration of the key manager.
    pub key_manager: KeyManagerConfig,

    /// The configuration of the zone server.
    pub server: ServerConfig,
}

//--- Defaults

impl Default for Config {
    fn default() -> Self {
        Self {
            policy_dir: "/etc/cascade/policies".into(),
            zone_state_dir: "/var/db/cascade/zone-state.db".into(),
            tsig_store_path: "/var/db/cascade/tsig-keys.db".into(),
            daemon: Default::default(),
            loader: Default::default(),
            signer: Default::default(),
            key_manager: Default::default(),
            server: Default::default(),
        }
    }
}

//--- Initialization

impl Config {
    /// Set up a [`clap::Command`] with config-related arguments.
    pub fn setup_cli(cmd: clap::Command) -> clap::Command {
        args::ArgsSpec::setup(cmd)
    }

    /// Initialize Cascade's configuration.
    ///
    /// The configuration file is not read here; it should only be read on
    /// explicit user request or if a global state file is not available.
    pub fn init(cli_matches: &clap::ArgMatches) -> Result<Self, ConfigError> {
        // Process environment variables and command-line arguments.
        let env = env::EnvSpec::process()?;
        let args = args::ArgsSpec::process(cli_matches);

        // Combine their data with the default state.
        let mut this = Self::default();
        args.merge(&mut this);
        env.merge(&mut this);

        // Return the prepared configuration.
        Ok(this)
    }
}

//----------- DaemonConfig -----------------------------------------------------

/// Daemon-related configuration for Cascade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    /// Logging configuration.
    pub logging: LoggingConfig,

    /// The location of the configuration file.
    pub config_file: Setting<Box<Utf8Path>>,

    /// Whether Cascade should fork on startup.
    pub daemonize: Setting<bool>,

    /// The path to a PID file to maintain.
    pub pid_file: Option<Box<Utf8Path>>,

    /// The directory to chroot into after startup.
    pub chroot: Option<Box<Utf8Path>>,

    /// The identity to assume after startup.
    pub identity: Option<(UserId, GroupId)>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            logging: LoggingConfig::default(),
            config_file: Setting::new("/etc/cascade/config.toml".into()),
            daemonize: Setting::new(false),
            pid_file: None,
            chroot: None,
            identity: None,
        }
    }
}

//----------- LoggingConfig ----------------------------------------------------

/// Logging configuration for Cascade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggingConfig {
    /// The minimum severity of messages to log.
    pub level: Setting<LogLevel>,

    /// Where to log messages to.
    pub target: Setting<LogTarget>,

    /// Targets to log trace messages for.
    pub trace_targets: Setting<foldhash::HashSet<Box<str>>>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: Setting::new(LogLevel::Info),
            target: Setting::new(LogTarget::File("/dev/stdout".into())),
            trace_targets: Setting::new(Default::default()),
        }
    }
}

//----------- UserId -----------------------------------------------------------

/// A numeric or named user ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserId {
    /// A numeric ID.
    Numeric(u32),

    /// A user name.
    Named(Box<str>),
}

//----------- GroupId ----------------------------------------------------------

/// A numeric or named group ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupId {
    /// A numeric ID.
    Numeric(u32),

    /// A group name.
    Named(Box<str>),
}

//----------- LoaderConfig -----------------------------------------------------

/// Configuration for the zone loader.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoaderConfig {
    /// Where to listen for zone update notifications.
    pub notif_listeners: Vec<SocketConfig>,

    /// Configuration for reviewing loaded zones.
    pub review: ReviewConfig,
}

//----------- SignerConfig -----------------------------------------------------

/// Configuration for the zone signer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignerConfig {
    /// Configuration for reviewing signed zones.
    pub review: ReviewConfig,
}

//----------- ReviewConfig -----------------------------------------------------

/// Configuration for reviewing zones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewConfig {
    /// Where to serve zones for review.
    pub servers: Vec<SocketConfig>,
}

//----------- KeyManagerConfig -------------------------------------------------

/// Configuration for the key manager.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyManagerConfig {}

//----------- ServerConfig -----------------------------------------------------

/// Configuration for the zone server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerConfig {
    /// Where to serve zones.
    pub servers: Vec<SocketConfig>,
}

//----------- SocketConfig -----------------------------------------------------

/// Configuration for serving / listening on a network socket.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SocketConfig {
    /// Listen exclusively over UDP.
    UDP {
        /// The socket address to listen on.
        addr: SocketAddr,
    },

    /// Listen exclusively over TCP.
    TCP {
        /// The socket address to listen on.
        addr: SocketAddr,
    },

    /// Listen over both TCP and UDP.
    TCPUDP {
        /// The socket address to listen on.
        addr: SocketAddr,
    },
    //
    // TODO: TLS
}

//----------- LogLevel ---------------------------------------------------------

/// A severity level for logging.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl LogLevel {
    /// Represent a [`LogLevel`] as a string.
    pub const fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warning => "warning",
            LogLevel::Error => "error",
            LogLevel::Critical => "critical",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

//----------- LogTarget --------------------------------------------------------

/// A logging target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTarget {
    /// Append logs to a file.
    ///
    /// If the file is a terminal, ANSI color codes may be used.
    File(Box<Utf8Path>),

    /// Write logs to the UNIX syslog.
    Syslog,
}

//----------- Setting ----------------------------------------------------------

/// A configured setting.
#[derive(Debug, Default, Clone, Copy)]
pub struct Setting<T> {
    /// The default for the value.
    pub default: T,

    /// The setting in the configuration file, if any.
    pub file: Option<T>,

    /// The setting from environment variables, if any.
    pub env: Option<T>,

    /// The setting from command-line arguments, if any.
    pub args: Option<T>,
}

impl<T> Setting<T> {
    /// Construct a new [`Setting`].
    pub const fn new(default: T) -> Self {
        Self {
            default,
            file: None,
            env: None,
            args: None,
        }
    }

    /// The current value.
    pub const fn value(&self) -> &T {
        // This is a 'const' implementation of:
        //
        // self.args.as_ref()
        //     .or(self.env.as_ref())
        //     .or(self.file.as_ref())
        //     .unwrap_or(&self.default)

        match self {
            Self {
                args: Some(value), ..
            }
            | Self {
                env: Some(value), ..
            }
            | Self {
                file: Some(value), ..
            }
            | Self { default: value, .. } => value,
        }
    }

    /// The source of the current value.
    pub const fn setting(&self) -> SettingSource {
        match self {
            Self { args: Some(_), .. } => SettingSource::Args,
            Self { env: Some(_), .. } => SettingSource::Env,
            Self { file: Some(_), .. } => SettingSource::File,
            Self { default: _, .. } => SettingSource::Default,
        }
    }
}

impl<T: PartialEq> PartialEq for Setting<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}

impl<T: Eq> Eq for Setting<T> {}

impl<T: Hash> Hash for Setting<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value().hash(state)
    }
}

//----------- SettingSource ----------------------------------------------------

/// The source of a configured setting.
///
/// There are four possible sources for a setting.  Each source has a designated
/// priority, with which it can override settings from other sources.  They are
/// enumerated here from lowest to highest priority.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingSource {
    /// A default.
    Default,

    /// The configuration file.
    File,

    /// Environment variables.
    Env,

    /// Command-line arguments.
    Args,
}

//----------- ConfigError ------------------------------------------------------

/// An error in configuring Cascade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// An error occurred regarding environment variables.
    Env(env::EnvError),

    /// An error occurred regarding the configuration file.
    File {
        /// The location of the config file.
        path: Box<Utf8Path>,

        /// The error that occurred.
        error: file::FileError,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Env(error) => error.fmt(f),
            ConfigError::File {
                error: file::FileError::Load(error),
                path,
            } => {
                write!(f, "could not load the config file '{path}': {error}")
            }
            ConfigError::File {
                error: file::FileError::Parse(error),
                path,
            } => {
                write!(f, "could not parse the config file '{path}': {error}")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Env(error) => Some(error),
            ConfigError::File { error, .. } => Some(error),
        }
    }
}

impl From<env::EnvError> for ConfigError {
    fn from(value: env::EnvError) -> Self {
        Self::Env(value)
    }
}
