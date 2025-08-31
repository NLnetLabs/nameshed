//! Version 1 of the configuration file.

use std::{fmt, net::SocketAddr, num::IntErrorKind, str::FromStr};

use camino::Utf8Path;
use serde::Deserialize;

use crate::config::{
    Config, DaemonConfig, GroupId, KeyManagerConfig, LoaderConfig, LogLevel, LogTarget,
    ReviewConfig, ServerConfig, SignerConfig, SocketConfig, UserId,
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
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut Config) {
        config.policy_dir = self.policy_dir;
        config.zone_state_dir = self.zone_state_dir;
        config.tsig_store_path = self.tsig_store_path;
        self.daemon.parse_into(&mut config.daemon);
        self.loader.parse_into(&mut config.loader);
        self.signer.parse_into(&mut config.signer);
        self.key_manager.parse_into(&mut config.key_manager);
        self.server.parse_into(&mut config.server);
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
        "/var/db/cascade/zone-state.db".into()
    }

    /// The default value for `tsig_store_path`.
    fn tsig_store_path_default() -> Box<Utf8Path> {
        "/var/db/cascade/tsig-keys.db".into()
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
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut DaemonConfig) {
        config.logging.level.file = self.log_level.map(|v| v.parse());
        config.logging.target.file = self.log_target.map(|v| v.parse());
        config.daemonize.file = self.daemonize;
        config.pid_file = self.pid_file;
        config.chroot = self.chroot;
        config.identity = self.identity.map(|v| v.parse());
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
    /// Parse from this specification.
    pub fn parse(self) -> LogLevel {
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
    /// Parse from this specification.
    pub fn parse(self) -> LogTarget {
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
    /// Parse from this specification.
    pub fn parse(self) -> (UserId, GroupId) {
        (self.user.parse(), self.group.parse())
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
    /// Parse from this specification.
    pub fn parse(self) -> UserId {
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
    /// Parse from this specification.
    pub fn parse(self) -> GroupId {
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
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut LoaderConfig) {
        config.notif_listeners.clear();
        config
            .notif_listeners
            .extend(self.notif_listeners.into_iter().map(|v| v.parse()));
        self.review.parse_into(&mut config.review);
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
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut SignerConfig) {
        self.review.parse_into(&mut config.review);
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
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut ReviewConfig) {
        config.servers.clear();
        config
            .servers
            .extend(self.servers.into_iter().map(|v| v.parse()));
    }
}

//----------- KeyManagerSpec ---------------------------------------------------

/// Configuring DNSSEC key management.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct KeyManagerSpec {}

//--- Conversion

impl KeyManagerSpec {
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut KeyManagerConfig) {
        let KeyManagerConfig {} = *config;
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
    /// Parse from this specification.
    pub fn parse_into(self, config: &mut ServerConfig) {
        config.servers.clear();
        config
            .servers
            .extend(self.servers.into_iter().map(|v| v.parse()));
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
    /// Parse from this specification.
    pub fn parse(self) -> SocketConfig {
        match self {
            SocketSpec::Simple(spec) => spec.parse(),
            SocketSpec::Complex(spec) => spec.parse(),
        }
    }
}

impl SimpleSocketSpec {
    /// Parse from this specification.
    pub fn parse(self) -> SocketConfig {
        match self {
            Self::UDP { addr } => SocketConfig::UDP { addr },
            Self::TCP { addr } => SocketConfig::TCP { addr },
            Self::TCPUDP { addr } => SocketConfig::TCPUDP { addr },
        }
    }
}

impl ComplexSocketSpec {
    /// Parse from this specification.
    pub fn parse(self) -> SocketConfig {
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
