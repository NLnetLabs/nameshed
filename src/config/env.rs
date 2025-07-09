//! Configuration from environment variables.

use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};

use super::LogLevel;

//----------- EnvSpec ----------------------------------------------------------

/// Configuration-related environment variables.
#[derive(Clone, Debug)]
pub struct EnvSpec {
    /// The configuration file to load.
    pub config: Option<Box<Utf8Path>>,

    /// The minimum severity of messages to log.
    pub log_level: Option<LogLevel>,

    /// The file to write logs to.
    pub log_file: Option<Box<Utf8Path>>,
}

impl EnvSpec {
    /// Process environment variables.
    pub fn process() -> Result<Self, EnvError> {
        Ok(Self {
            config: match std::env::var("NAMESHED_CONFIG") {
                Ok(value) => Some(Utf8PathBuf::from(value).into_boxed_path()),
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(EnvError::NonUtf8 {
                        var: "NAMESHED_CONFIG",
                    });
                }
                Err(std::env::VarError::NotPresent) => None,
            },
            log_level: match std::env::var("NAMESHED_CONFIG") {
                Ok(value) => Some(match &*value {
                    "trace" => LogLevel::Trace,
                    "debug" => LogLevel::Debug,
                    "info" => LogLevel::Info,
                    "warning" => LogLevel::Warning,
                    "error" => LogLevel::Error,
                    "critical" => LogLevel::Critical,
                    _ => {
                        return Err(EnvError::InvalidLogLevel {
                            value: value.into_boxed_str(),
                        })
                    }
                }),
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(EnvError::NonUtf8 {
                        var: "NAMESHED_CONFIG",
                    });
                }
                Err(std::env::VarError::NotPresent) => None,
            },
            log_file: match std::env::var("NAMESHED_LOG_FILE") {
                Ok(value) => Some(Utf8PathBuf::from(value).into_boxed_path()),
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(EnvError::NonUtf8 {
                        var: "NAMESHED_LOG_FILE",
                    });
                }
                Err(std::env::VarError::NotPresent) => None,
            },
        })
    }
}

//----------- EnvError ---------------------------------------------------------

/// An error in processing environment variables.
#[derive(Clone, PartialEq, Eq)]
pub enum EnvError {
    /// A non-UTF-8 value was observed.
    NonUtf8 {
        /// The name of the offending environment variable.
        var: &'static str,
    },

    /// An invalid log level was used.
    InvalidLogLevel {
        /// The log level value.
        value: Box<str>,
    },
}

impl fmt::Debug for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
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
        }
    }
}
