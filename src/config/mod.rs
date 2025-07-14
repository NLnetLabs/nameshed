//! Configuring Nameshed.
//!
//! As per convention, Nameshed is configured from three sources (from least to
//! most specific): configuration files, environment variables, and command-line
//! arguments.  This module defines and collects together these sources.

use std::{fmt, net::SocketAddr};

use camino::Utf8Path;

pub mod args;
pub mod env;
pub mod file;

//----------- Config -----------------------------------------------------------

/// Configuration for Nameshed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
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

//--- Processing

impl Config {
    /// Set up a [`clap::Command`] with config-related arguments.
    pub fn setup_cli(cmd: clap::Command) -> clap::Command {
        args::ArgsSpec::setup(cmd)
    }

    /// Process all configuration sources.
    pub fn process(cli_matches: &clap::ArgMatches) -> Result<Self, ConfigError> {
        // Process environment variables and command-line arguments.
        let mut env = env::EnvSpec::process()?;
        let mut args = args::ArgsSpec::process(cli_matches);

        // Determine the location of the configuration file.
        let config_file = args
            .config
            .take()
            .map(|path| Setting {
                source: SettingSource::Args,
                value: path,
            })
            .or(env.config.take().map(|path| Setting {
                source: SettingSource::Env,
                value: path,
            }))
            .unwrap_or(Setting {
                source: SettingSource::Default,
                value: "/etc/nameshed/config.toml".into(),
            });

        // Load and parse the configuration file.
        let file = match file::FileSpec::load(&config_file.value) {
            Ok(file) => file,
            Err(error) => {
                return Err(ConfigError::File {
                    path: config_file.value,
                    error,
                })
            }
        };

        // Build the configuration.
        let mut this = file.build(config_file);

        // Include data from environment variables and command-line arguments.
        args.merge(&mut this);
        env.merge(&mut this);

        // Return the prepared configuration.
        Ok(this)
    }
}

//----------- DaemonConfig -----------------------------------------------------

/// Daemon-related configuration for Nameshed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    /// The minimum severity of messages to log.
    pub log_level: Setting<LogLevel>,

    /// The location logs are written to.
    pub log_file: Setting<Box<Utf8Path>>,

    /// The location of the configuration file.
    pub config_file: Setting<Box<Utf8Path>>,
}

//----------- LoaderConfig -----------------------------------------------------

/// Configuration for the zone loader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoaderConfig {
    /// Where to listen for zone update notifications.
    pub notif_listeners: Vec<SocketConfig>,

    /// Configuration for reviewing loaded zones.
    pub review: ReviewConfig,
}

//----------- SignerConfig -----------------------------------------------------

/// Configuration for the zone signer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignerConfig {
    /// Configuration for reviewing signed zones.
    pub review: ReviewConfig,
}

//----------- ReviewConfig -----------------------------------------------------

/// Configuration for reviewing zones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewConfig {
    /// Where to serve zones for review.
    pub servers: Vec<SocketConfig>,
}

//----------- KeyManagerConfig -------------------------------------------------

/// Configuration for the key manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyManagerConfig {}

//----------- ServerConfig -----------------------------------------------------

/// Configuration for the zone server.
#[derive(Clone, Debug, PartialEq, Eq)]
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

    /// Something is wrong (but Nameshed can recover).
    Error,

    /// Something is wrong and Nameshed can't function at all.
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

//----------- Setting ----------------------------------------------------------

/// A configured setting.
#[derive(Debug, Clone, Copy)]
pub struct Setting<T> {
    /// The source of the value.
    pub source: SettingSource,

    /// The underlying value.
    pub value: T,
}

impl<T> Setting<T> {
    /// Merge two [`Setting`]s, keeping the highest-priority value.
    pub fn merge(&mut self, other: Self) {
        if self.source < other.source {
            self.value = other.value;
        }
    }

    /// Merge a [`Setting`] with a value from a fixed source.
    pub fn merge_value(&mut self, value: Option<T>, source: SettingSource) {
        if let Some(value) = value {
            self.merge(Setting { source, value });
        }
    }
}

impl<T: PartialEq> PartialEq for Setting<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for Setting<T> {}

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

/// An error in configuring Nameshed.
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
