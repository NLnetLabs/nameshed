//! Configuration from environment variables.

use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};

use super::{Config, LogLevel, LogTarget, SettingSource};

//----------- EnvSpec ----------------------------------------------------------

/// Configuration-related environment variables.
#[derive(Clone, Debug)]
pub struct EnvSpec {
    /// The configuration file to load.
    pub config: Option<Box<Utf8Path>>,

    /// The minimum severity of messages to log.
    pub log_level: Option<LogLevel>,

    /// The target of log messages.
    pub log_target: Option<LogTargetSpec>,
}

impl EnvSpec {
    /// Process environment variables.
    pub fn process() -> Result<Self, EnvError> {
        fn var(var: &'static str) -> Result<Option<String>, EnvError> {
            std::env::var_os(var)
                .map(|value| value.into_string().map_err(|_| EnvError::NonUtf8 { var }))
                .transpose()
        }

        let config_path =
            var("NAMESHED_CONFIG_PATH")?.map(|path| Utf8PathBuf::from(path).into_boxed_path());

        let log_level = var("NAMESHED_LOG_LEVEL")?
            .map(|value| match &*value {
                "trace" => Ok(LogLevel::Trace),
                "debug" => Ok(LogLevel::Debug),
                "info" => Ok(LogLevel::Info),
                "warning" => Ok(LogLevel::Warning),
                "error" => Ok(LogLevel::Error),
                "critical" => Ok(LogLevel::Critical),
                _ => Err(EnvError::InvalidLogLevel {
                    value: value.into_boxed_str(),
                }),
            })
            .transpose()?;

        let log = var("NAMESHED_LOG")?.map(LogTargetSpec::parse).transpose()?;

        Ok(Self {
            config: config_path,
            log_level,
            log_target: log,
        })
    }

    /// Merge this into a [`Config`].
    pub fn merge(self, config: &mut Config) {
        let daemon = &mut config.daemon;
        let source = SettingSource::Env;
        daemon.log_level.merge_value(self.log_level, source);
        daemon
            .log_target
            .merge_value(self.log_target.map(|t| t.build()), source);
        daemon.config_file.merge_value(self.config, source);
    }
}

//----------- LogTarget --------------------------------------------------------

/// A logging target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTargetSpec {
    /// Append logs to a file.
    ///
    /// If the file is a terminal, ANSI color codes may be used.
    File(Box<Utf8Path>),

    /// Write logs to the UNIX syslog.
    Syslog,
}

//--- Parsing

impl LogTargetSpec {
    /// Parse this value from an owned string.
    pub fn parse(s: String) -> Result<Self, EnvError> {
        if let Some(s) = s.strip_prefix("file:") {
            let path = <&Utf8Path>::from(s);
            Ok(Self::File(path.into()))
        } else if s == "syslog" {
            Ok(Self::Syslog)
        } else {
            Err(EnvError::InvalidLogTarget { value: s.into() })
        }
    }
}

//--- Conversion

impl LogTargetSpec {
    /// Build the internal configuration.
    pub fn build(self) -> LogTarget {
        match self {
            Self::File(path) => LogTarget::File(path),
            Self::Syslog => LogTarget::Syslog,
        }
    }
}

//----------- EnvError ---------------------------------------------------------

/// An error in processing environment variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvError {
    /// A non-UTF-8 value was specified.
    NonUtf8 {
        /// The name of the offending environment variable.
        var: &'static str,
    },

    /// An invalid log level was specified.
    InvalidLogLevel {
        /// The log level value.
        value: Box<str>,
    },

    /// An invalid log target was specified.
    InvalidLogTarget {
        /// The log target value.
        value: Box<str>,
    },
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvError::NonUtf8 { var } => {
                write!(f, "'${var}' was not valid UTF-8")
            }
            EnvError::InvalidLogLevel { value } => {
                write!(
                    f,
                    "'$NAMESHED_LOG_LEVEL' ({value:?}) is not a valid log level"
                )
            }
            EnvError::InvalidLogTarget { value } => {
                write!(
                    f,
                    "'$NAMESHED_LOG' ({value:?}) is not a valid logging target"
                )
            }
        }
    }
}

impl std::error::Error for EnvError {}
