//! Version 1 of the configuration file.

use std::{collections::HashMap, fmt, net::SocketAddr, str::FromStr};

use camino::Utf8Path;
use serde::Deserialize;

use crate::config::{
    Config, CryptoConfig, DaemonConfig, HsmStoreConfig, KeyManagerConfig, LoaderConfig, LogLevel,
    ReviewConfig, ServerConfig, Setting, SettingSource, SignerConfig, SocketConfig,
};

//----------- Spec -------------------------------------------------------------

/// A configuration file.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Spec {
    /// Configuring the Nameshed daemon.
    pub daemon: DaemonSpec,

    /// Configuring the zone loader.
    pub loader: LoaderSpec,

    /// Configuring the zone signer.
    pub signer: SignerSpec,

    /// Configuring the key manager.
    pub key_manager: KeyManagerSpec,

    /// Configuring cryptography.
    pub crypto: CryptoSpec,
}

//--- Conversion

impl Spec {
    /// Build the internal configuration.
    pub fn build(self, config_file: Setting<Box<Utf8Path>>) -> Config {
        Config {
            daemon: self.daemon.build(config_file),
            loader: self.loader.build(),
            signer: self.signer.build(),
            key_manager: self.key_manager.build(),
            crypto: self.crypto.build(),
        }
    }
}

//----------- DaemonSpec -------------------------------------------------------

/// Configuring the Nameshed daemon.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DaemonSpec {
    /// The minimum severity of messages to log.
    pub log_level: Option<LogLevelSpec>,

    /// The location logs are written to.
    pub log_file: Option<Box<Utf8Path>>,
}

//--- Conversion

impl DaemonSpec {
    /// Build the internal configuration.
    pub fn build(self, config_file: Setting<Box<Utf8Path>>) -> DaemonConfig {
        DaemonConfig {
            log_level: self
                .log_level
                .map(|log_level| Setting {
                    source: SettingSource::File,
                    value: log_level.build(),
                })
                .unwrap_or(Setting {
                    source: SettingSource::Default,
                    value: LogLevel::Info,
                }),
            log_file: self
                .log_file
                .map(|log_file| Setting {
                    source: SettingSource::File,
                    value: log_file,
                })
                .unwrap_or(Setting {
                    source: SettingSource::Default,
                    value: "/var/log/nameshed.log".into(),
                }),
            config_file,
        }
    }
}

//----------- LogLevelSpec -----------------------------------------------------

/// A severity level for logging.
#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevelSpec {
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

//--- Conversion

impl LogLevelSpec {
    /// Build the internal configuration.
    pub fn build(self) -> LogLevel {
        match self {
            Self::Trace => LogLevel::Trace,
            Self::Debug => LogLevel::Debug,
            Self::Info => LogLevel::Info,
            Self::Warning => LogLevel::Warning,
            Self::Error => LogLevel::Error,
            Self::Critical => LogLevel::Critical,
        }
    }
}

//----------- LoaderSpec -------------------------------------------------------

/// Configuring the zone loader.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LoaderSpec {
    /// Where to listen for zone update notifications.
    pub notif_listeners: Vec<SocketSpec>,

    /// Configuration for reviewing loaded zones.
    pub review: ReviewSpec,
}

//--- Conversion

impl LoaderSpec {
    /// Build the internal configuration.
    pub fn build(self) -> LoaderConfig {
        LoaderConfig {
            notif_listeners: self
                .notif_listeners
                .into_iter()
                .map(|nl| nl.build())
                .collect(),
            review: self.review.build(),
        }
    }
}

//----------- SignerSpec -------------------------------------------------------

/// Configuring the zone signer.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SignerSpec {
    /// Configuration for reviewing loaded zones.
    pub review: ReviewSpec,
}

//--- Conversion

impl SignerSpec {
    /// Build the internal configuration.
    pub fn build(self) -> SignerConfig {
        SignerConfig {
            review: self.review.build(),
        }
    }
}

//----------- ReviewSpec -------------------------------------------------------

/// Configuring zone review.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReviewSpec {
    /// Where to serve zones for review.
    pub servers: Vec<SocketSpec>,
}

//--- Conversion

impl ReviewSpec {
    /// Build the internal configuration.
    pub fn build(self) -> ReviewConfig {
        ReviewConfig {
            servers: self.servers.into_iter().map(|s| s.build()).collect(),
        }
    }
}

//----------- KeyManagerSpec ---------------------------------------------------

/// Configuring the key manager.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KeyManagerSpec {}

//--- Conversion

impl KeyManagerSpec {
    /// Build the internal configuration.
    pub fn build(self) -> KeyManagerConfig {
        KeyManagerConfig {}
    }
}

//----------- ServerSpec -------------------------------------------------------

/// Configuring the zone server.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServerSpec {
    /// Where to serve zones.
    pub servers: Vec<SocketSpec>,
}

//--- Conversion

impl ServerSpec {
    /// Build the internal configuration.
    pub fn build(self) -> ServerConfig {
        ServerConfig {
            servers: self.servers.into_iter().map(|s| s.build()).collect(),
        }
    }
}

//----------- CryptoSpec -------------------------------------------------------

/// Configuring cryptography.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CryptoSpec {
    /// Configured HSM stores.
    pub hsm_store: HashMap<Box<str>, HsmStoreSpec>,
}

//--- Conversion

impl CryptoSpec {
    /// Build the internal configuration.
    pub fn build(self) -> CryptoConfig {
        CryptoConfig {
            hsm_stores: self
                .hsm_store
                .into_iter()
                .map(|(name, spec)| (name, spec.build()))
                .collect(),
        }
    }
}

//----------- HsmStoreSpec -----------------------------------------------------

/// Configuration for an HSM store.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum HsmStoreSpec {}

//--- Conversion

impl HsmStoreSpec {
    /// Build the internal configuration.
    pub fn build(self) -> HsmStoreConfig {
        match self {}
    }
}

//----------- SocketSpec -------------------------------------------------------

/// Configuration for serving / listening on a network socket.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, expecting = "a URI string or an inline table")]
pub enum SocketSpec {
    /// A simple socket specification.
    Simple(SimpleSocketSpec),

    /// A complex socket specification.
    Complex(ComplexSocketSpec),
}

/// A simple [`SocketSpec`] as a string.
#[derive(Clone, Debug)]
pub enum SimpleSocketSpec {
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

/// A complex [`SocketSpec`] as a table.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum ComplexSocketSpec {
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

//--- Deserialization

impl FromStr for SimpleSocketSpec {
    type Err = ParseSimpleSocketSpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((protocol, address)) = s.split_once("://") else {
            // Default to UDP.
            return Ok(Self::UDP { addr: s.parse()? });
        };

        match protocol {
            "udp" => Ok(Self::UDP {
                addr: address.parse()?,
            }),
            "tcp" => Ok(Self::TCP {
                addr: address.parse()?,
            }),
            _ => Err(ParseSimpleSocketSpecError::UnknownProtocol {
                protocol: protocol.into(),
            }),
        }
    }
}

impl<'de> Deserialize<'de> for SimpleSocketSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

//--- Conversion

impl SocketSpec {
    /// Build the internal configuration.
    pub fn build(self) -> SocketConfig {
        match self {
            SocketSpec::Simple(spec) => spec.build(),
            SocketSpec::Complex(spec) => spec.build(),
        }
    }
}

impl SimpleSocketSpec {
    /// Build the internal configuration.
    pub fn build(self) -> SocketConfig {
        match self {
            Self::UDP { addr } => SocketConfig::UDP { addr },
            Self::TCP { addr } => SocketConfig::TCP { addr },
        }
    }
}

impl ComplexSocketSpec {
    /// Build the internal configuration.
    pub fn build(self) -> SocketConfig {
        match self {
            Self::UDP { addr } => SocketConfig::UDP { addr },
            Self::TCP { addr } => SocketConfig::TCP { addr },
        }
    }
}

//----------- ParseSimpleSocketSpecError ---------------------------------------

/// An error in parsing a [`SocketSpec`] URI string.
#[derive(Clone, Debug)]
pub enum ParseSimpleSocketSpecError {
    /// An unrecognized protocol was specified.
    UnknownProtocol {
        /// The specified protocol value.
        protocol: Box<str>,
    },

    /// The address could not be parsed.
    Address(std::net::AddrParseError),
}

impl fmt::Display for ParseSimpleSocketSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProtocol { protocol } => {
                write!(f, "unrecognized protocol {protocol:?}")
            }
            Self::Address(error) => error.fmt(f),
        }
    }
}

impl From<std::net::AddrParseError> for ParseSimpleSocketSpecError {
    fn from(value: std::net::AddrParseError) -> Self {
        Self::Address(value)
    }
}
