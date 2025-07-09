//! Configuring Nameshed.
//!
//! As per convention, Nameshed is configured from three sources (from least to
//! most specific): configuration files, environment variables, and command-line
//! arguments.  This module defines and collects together these sources.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::SocketAddr,
};

use camino::Utf8Path;

pub mod args;

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

    /// Cryptography-related configuration.
    pub crypto: CryptoConfig,
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
    pub notif_listeners: HashSet<SocketConfig>,

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
    pub servers: HashSet<SocketConfig>,
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
    pub servers: HashSet<SocketConfig>,
}

//----------- CryptoConfig -----------------------------------------------------

/// Cryptography-related configuration for Nameshed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CryptoConfig {
    /// Configured HSM stores.
    pub hsm_stores: HashMap<Box<str>, HsmStoreConfig>,
}

//----------- HsmStoreConfig ---------------------------------------------------

/// Configuration for an HSM store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HsmStoreConfig {
    /// A PKCS#11 store.
    PKCS11 {
        /// The location of the dynamic library to load.
        library: Box<Utf8Path>,
    },
    //
    // TODO: KMIP?
}

//----------- SocketConfig -----------------------------------------------------

/// Configuration for serving / listening on a network socket.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SocketConfig {
    /// Listen over UDP.
    UDP {
        /// The socket address to listen on.
        addr: SocketAddr,
    },

    /// Listen over TCP.
    TCP {
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
}

impl<T: PartialEq> PartialEq for Setting<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for Setting<T> {}

//----------- SettingSource ----------------------------------------------------

/// The source of a configured setting.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingSource {
    /// The configuration file.
    File,

    /// Environment variables.
    Env,

    /// Command-line arguments.
    Args,
}
