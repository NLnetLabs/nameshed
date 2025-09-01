//! Version 1 of the configuration file.

use std::{fmt, net::SocketAddr, num::IntErrorKind, str::FromStr};

use camino::Utf8Path;
use serde::Deserialize;

use crate::config::{
    Config, DaemonConfig, GroupId, KeyManagerConfig, LoaderConfig, LogLevel, LogTarget,
    LoggingConfig, ReviewConfig, ServerConfig, Setting, SignerConfig, SocketConfig, UserId,
};

//----------- Spec -------------------------------------------------------------

/// A configuration file.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Spec {
    /// The directory storing policy files.
    #[serde(default = "Spec::policy_dir_default")]
    pub policy_dir: Box<Utf8Path>,

    /// The directory storing per-zone state files.
    #[serde(default = "Spec::zone_state_dir_default")]
    pub zone_state_dir: Box<Utf8Path>,

    /// The file storing TSIG keys.
    #[serde(default = "Spec::tsig_store_path_default")]
    pub tsig_store_path: Box<Utf8Path>,

    /// Configuring the Cascade daemon.
    pub daemon: DaemonSpec,

    /// Configuring how zones are loaded.
    pub loader: LoaderSpec,

    /// Configuring how zones are signed.
    pub signer: SignerSpec,

    /// Configuring key management.
    pub key_manager: KeyManagerSpec,

    /// Configuring zone serving.
    pub server: ServerSpec,
}

//--- Conversion

impl Spec {
    /// Build the internal configuration.
    pub fn build(self, config_file: Setting<Box<Utf8Path>>) -> Config {
        Config {
            policy_dir: self.policy_dir,
            zone_state_dir: self.zone_state_dir,
            tsig_store_path: self.tsig_store_path,
            daemon: self.daemon.build(config_file),
            loader: self.loader.build(),
            signer: self.signer.build(),
            key_manager: self.key_manager.build(),
            server: self.server.build(),
        }
    }
}

//--- Defaults

impl Default for Spec {
    fn default() -> Self {
        Self {
            policy_dir: Self::policy_dir_default(),
            zone_state_dir: Self::zone_state_dir_default(),
            tsig_store_path: Self::tsig_store_path_default(),
            daemon: Default::default(),
            loader: Default::default(),
            signer: Default::default(),
            key_manager: Default::default(),
            server: Default::default(),
        }
    }
}

impl Spec {
    /// The default value for `policy_dir`.
    fn policy_dir_default() -> Box<Utf8Path> {
        "/etc/cascade/policies".into()
    }

    /// The default value for `zone_state_dir`.
    fn zone_state_dir_default() -> Box<Utf8Path> {
        "/var/db/cascade/zone-state".into()
    }

    /// The default value for `tsig_store_path`.
    fn tsig_store_path_default() -> Box<Utf8Path> {
        "/var/db/cascade/tsig-keys".into()
    }
}

//----------- DaemonSpec -------------------------------------------------------

/// Configuring the Cascade daemon.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct DaemonSpec {
    /// The minimum severity of messages to log.
    pub log_level: Option<LogLevelSpec>,

    /// The target to log messages to.
    pub log_target: Option<LogTargetSpec>,

    /// Whether Cascade should fork on startup.
    pub daemonize: Option<bool>,

    /// The path to a PID file to maintain.
    pub pid_file: Option<Box<Utf8Path>>,

    /// The directory to chroot into after startup.
    pub chroot: Option<Box<Utf8Path>>,

    /// The identity to assume after startup.
    pub identity: Option<IdentitySpec>,
}

//--- Conversion

impl DaemonSpec {
    /// Build the internal configuration.
    pub fn build(self, config_file: Setting<Box<Utf8Path>>) -> DaemonConfig {
        let logging = LoggingConfig {
            level: Setting {
                default: LogLevel::Info,
                file: self.log_level.map(|v| v.build()),
                env: None,
                args: None,
            },
            target: Setting {
                default: LogTarget::File("/var/log/cascade.log".into()),
                file: self.log_target.map(|v| v.build()),
                env: None,
                args: None,
            },
            trace_targets: Default::default(),
        };

        DaemonConfig {
            logging,
            config_file,
            daemonize: Setting {
                default: false,
                file: self.daemonize,
                env: None,
                args: None,
            },
            pid_file: self.pid_file,
            chroot: self.chroot,
            identity: self.identity.map(|i| i.build()),
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

    /// Something went wrong (but Cascade can recover).
    Error,

    /// Something went wrong and Cascade can't function at all.
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

//----------- LogTargetSpec ----------------------------------------------------

/// A logging target.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum LogTargetSpec {
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

impl LogTargetSpec {
    /// Build the internal configuration.
    pub fn build(self) -> LogTarget {
        match self {
            Self::File { path } => LogTarget::File(path),
            Self::Syslog => LogTarget::Syslog,
        }
    }
}

//----------- IdentitySpec -----------------------------------------------------

/// A user-group specification.
#[derive(Clone, Debug)]
pub struct IdentitySpec {
    /// The user ID.
    pub user: UserIdSpec,

    /// The group Id.
    pub group: GroupIdSpec,
}

//--- Deserialization

impl FromStr for IdentitySpec {
    type Err = ParseIdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Allow '<user>:<group>', or interpret the single value as both.
        let (user, group) = s.split_once(':').unwrap_or((s, s));

        Ok(Self {
            user: user.parse()?,
            group: group.parse()?,
        })
    }
}

impl<'de> Deserialize<'de> for IdentitySpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

//--- Conversion

impl IdentitySpec {
    /// Build the internal configuration.
    pub fn build(self) -> (UserId, GroupId) {
        (self.user.build(), self.group.build())
    }
}

//----------- UserId -----------------------------------------------------------

/// A numeric or named user ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserIdSpec {
    /// A numeric ID.
    Numeric(u32),

    /// A user name.
    Named(Box<str>),
}

//--- Deserialization

impl FromStr for UserIdSpec {
    type Err = ParseIdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<u32>() {
            Ok(id) => Ok(Self::Numeric(id)),

            Err(error) if *error.kind() == IntErrorKind::PosOverflow => {
                Err(ParseIdentityError::NumericOverflow { value: s.into() })
            }

            _ => Ok(Self::Named(s.into())),
        }
    }
}

//--- Conversion

impl UserIdSpec {
    /// Build the internal configuration.
    pub fn build(self) -> UserId {
        match self {
            Self::Numeric(id) => UserId::Numeric(id),
            Self::Named(id) => UserId::Named(id),
        }
    }
}

//----------- GroupId ----------------------------------------------------------

/// A numeric or named group ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupIdSpec {
    /// A numeric ID.
    Numeric(u32),

    /// A group name.
    Named(Box<str>),
}

//--- Deserialization

impl FromStr for GroupIdSpec {
    type Err = ParseIdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<u32>() {
            Ok(id) => Ok(Self::Numeric(id)),

            Err(error) if *error.kind() == IntErrorKind::PosOverflow => {
                Err(ParseIdentityError::NumericOverflow { value: s.into() })
            }

            _ => Ok(Self::Named(s.into())),
        }
    }
}

//--- Conversion

impl GroupIdSpec {
    /// Build the internal configuration.
    pub fn build(self) -> GroupId {
        match self {
            Self::Numeric(id) => GroupId::Numeric(id),
            Self::Named(id) => GroupId::Named(id),
        }
    }
}

//----------- LoaderSpec -------------------------------------------------------

/// Configuring how zones are loaded.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LoaderSpec {
    /// Where to listen for zone update notifications.
    pub notif_listeners: Vec<SocketSpec>,

    /// Configuring whether and how loaded zones are reviewed.
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
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct SignerSpec {
    /// Configuring whether and how signed zones are reviewed.
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

/// Configuring whether and how zones are reviewed.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
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

/// Configuring DNSSEC key management.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct KeyManagerSpec {}

//--- Conversion

impl KeyManagerSpec {
    /// Build the internal configuration.
    pub fn build(self) -> KeyManagerConfig {
        KeyManagerConfig {}
    }
}

//----------- ServerSpec -------------------------------------------------------

/// Configuring how zones are published.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
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

/// A complex [`SocketSpec`] as a table.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum ComplexSocketSpec {
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

//--- Deserialization

impl FromStr for SimpleSocketSpec {
    type Err = ParseSimpleSocketError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((protocol, address)) = s.split_once("://") else {
            // Default to TCP+UDP.
            return Ok(Self::TCPUDP { addr: s.parse()? });
        };

        match protocol {
            "udp" => Ok(Self::UDP {
                addr: address.parse()?,
            }),
            "tcp" => Ok(Self::TCP {
                addr: address.parse()?,
            }),
            _ => Err(ParseSimpleSocketError::UnknownProtocol {
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
            Self::TCPUDP { addr } => SocketConfig::TCPUDP { addr },
        }
    }
}

impl ComplexSocketSpec {
    /// Build the internal configuration.
    pub fn build(self) -> SocketConfig {
        match self {
            Self::UDP { addr } => SocketConfig::UDP { addr },
            Self::TCP { addr } => SocketConfig::TCP { addr },
            Self::TCPUDP { addr } => SocketConfig::TCPUDP { addr },
        }
    }
}

//----------- ParseIdentityError -----------------------------------------------

/// An error in parsing an [`IdentitySpec`].
#[derive(Clone, Debug)]
pub enum ParseIdentityError {
    /// A numeric ID was out of bounds.
    NumericOverflow {
        /// The specified ID number.
        value: Box<str>,
    },
}

impl fmt::Display for ParseIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NumericOverflow { value } => write!(f, "numeric ID '{value}' too large"),
        }
    }
}

//----------- ParseSimpleSocketError -------------------------------------------

/// An error in parsing a [`SocketSpec`] URI string.
#[derive(Clone, Debug)]
pub enum ParseSimpleSocketError {
    /// An unrecognized protocol was specified.
    UnknownProtocol {
        /// The specified protocol value.
        protocol: Box<str>,
    },

    /// The address could not be parsed.
    Address(std::net::AddrParseError),
}

impl fmt::Display for ParseSimpleSocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProtocol { protocol } => {
                write!(f, "unrecognized protocol {protocol:?}")
            }
            Self::Address(error) => error.fmt(f),
        }
    }
}

impl From<std::net::AddrParseError> for ParseSimpleSocketError {
    fn from(value: std::net::AddrParseError) -> Self {
        Self::Address(value)
    }
}
