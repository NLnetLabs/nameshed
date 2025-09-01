//! Logging from Nameshed.

use std::io::Write;
use std::net::Ipv4Addr;
use std::sync::RwLock;

use camino::Utf8Path;

use crate::config::{LogLevel, LogTarget, LoggingConfig};

//----------- Logger -----------------------------------------------------------

/// The state of the Nameshed logger.
pub struct Logger {
    /// The inner state of the logger.
    inner: RwLock<Option<Inner>>,

    /// The fallback logger.
    fallback: std::io::Stderr,
}

impl Logger {
    /// Launch the Nameshed logger.
    ///
    /// ## Panics
    ///
    /// Panics if a [`log`] logger has been set already.
    pub fn launch() -> &'static Logger {
        let this = Box::leak(Box::new(Self {
            inner: RwLock::new(None),
            fallback: std::io::stderr(),
        }));

        log::set_max_level(log::LevelFilter::Info);
        log::set_logger(this).unwrap();

        this
    }

    /// Prepare a change to the logger.
    pub fn prepare(
        &self,
        config: &LoggingConfig,
    ) -> Result<Option<PreparedChange>, std::io::Error> {
        let Ok(inner) = self.inner.read() else {
            // A panic occurred while the lock was held.  Don't do anything.
            return Ok(None);
        };

        if let Some(inner) = &*inner {
            let primary = if !inner.primary.matches(&config.target.value) {
                Some(PrimaryLogger::new(&config.target.value)?)
            } else {
                None
            };

            let level = config.level.value.into();

            let trace_targets = Some(
                config
                    .trace_targets
                    .iter()
                    .map(|v| v.value.clone())
                    .collect(),
            )
            .filter(|trace_targets| &inner.trace_targets != trace_targets);

            if primary.is_none() && inner.level == level && trace_targets.is_none() {
                return Ok(None);
            }

            Ok(Some(PreparedChange {
                primary,
                level,
                trace_targets,
            }))
        } else {
            Ok(Some(PreparedChange {
                primary: Some(PrimaryLogger::new(&config.target.value)?),
                level: config.level.value.into(),
                trace_targets: Some(
                    config
                        .trace_targets
                        .iter()
                        .map(|v| v.value.clone())
                        .collect(),
                ),
            }))
        }
    }

    /// Apply a prepared change to the logger.
    ///
    /// ## Panics
    ///
    /// Panics if the prepared change is inconsistent with the current state.
    pub fn apply(&self, change: PreparedChange) {
        let Ok(mut inner) = self.inner.write() else {
            // A panic occurred while the lock was held.  Don't do anything.
            return;
        };

        if let Some(inner) = &mut *inner {
            if let Some(primary) = change.primary {
                inner.primary = primary;
            }
            inner.level = change.level;
            if let Some(trace_targets) = change.trace_targets {
                inner.trace_targets = trace_targets;
            }

            if !inner.trace_targets.is_empty() {
                log::set_max_level(log::LevelFilter::Trace);
            } else {
                log::set_max_level(inner.level);
            }
        } else {
            let state = inner.insert(Inner {
                primary: change.primary.unwrap(),
                level: change.level,
                trace_targets: change.trace_targets.unwrap(),
            });

            if !state.trace_targets.is_empty() {
                log::set_max_level(log::LevelFilter::Trace);
            } else {
                log::set_max_level(state.level);
            }
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        if let Ok(inner) = self.inner.read() {
            if let Some(inner) = &*inner {
                return inner.enabled(metadata);
            }
        }

        metadata.level() <= log::LevelFilter::Info
    }

    fn log(&self, record: &log::Record) {
        if let Ok(inner) = self.inner.read() {
            if let Some(inner) = &*inner {
                return inner.log(record);
            }
        }

        let mut logger = &self.fallback;
        let _ = writeln!(&mut logger, "{}", record.args());
    }

    fn flush(&self) {
        if let Ok(inner) = self.inner.read() {
            if let Some(inner) = &*inner {
                return inner.flush();
            }
        }

        let mut logger = &self.fallback;
        let _ = logger.flush();
    }
}

//----------- Inner ------------------------------------------------------------

/// The inner state of a [`Logger`].
struct Inner {
    /// The primary logger.
    primary: PrimaryLogger,

    /// A log level filter.
    level: log::LevelFilter,

    /// A list of log targets to trace.
    trace_targets: foldhash::HashSet<Box<str>>,
}

impl log::Log for Inner {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        if metadata.level() <= self.level {
            return true;
        }

        metadata.level() == log::Level::Trace && self.trace_targets.contains(metadata.target())
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        self.primary.log(record)
    }

    fn flush(&self) {
        self.primary.flush()
    }
}

//----------- PrimaryLogger ----------------------------------------------------

/// A primary logger.
enum PrimaryLogger {
    /// A file logger.
    //
    // TODO: Attach a per-thread buffer here.
    File {
        /// The actual file.
        file: std::fs::File,

        /// The path to the file.
        path: Box<Utf8Path>,
    },

    /// A syslog logger.
    #[cfg(unix)]
    Syslog(syslog::BasicLogger),
}

impl PrimaryLogger {
    /// Initialize a new [`PrimaryLogger`].
    pub fn new(config: &LogTarget) -> Result<Self, std::io::Error> {
        match config {
            LogTarget::File(path) => {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&**path)?;

                Ok(Self::File {
                    file,
                    path: path.clone(),
                })
            }

            LogTarget::Syslog => {
                let formatter = syslog::Formatter3164::default();
                let result = syslog::unix(formatter.clone())
                    .or_else(|_| syslog::tcp(formatter.clone(), (Ipv4Addr::LOCALHOST, 601)))
                    .or_else(|_| {
                        syslog::udp(
                            formatter.clone(),
                            (Ipv4Addr::LOCALHOST, 0),
                            (Ipv4Addr::LOCALHOST, 514),
                        )
                    });
                let logger = result.map_err(|err| match err {
                    syslog::Error::Initialization(err) => std::io::Error::other(err),
                    syslog::Error::Write(err) => err,
                    syslog::Error::Io(err) => err,
                })?;

                Ok(Self::Syslog(syslog::BasicLogger::new(logger)))
            }
        }
    }

    /// Whether this matches a configured logging target.
    pub fn matches(&self, config: &LogTarget) -> bool {
        match (self, config) {
            (Self::File { path: l, .. }, LogTarget::File(r)) => l == r,
            (Self::Syslog(_), LogTarget::Syslog) => true,
            _ => false,
        }
    }
}

impl log::Log for PrimaryLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        match self {
            PrimaryLogger::File { file, .. } => {
                let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let value = format!(
                    "[{now}] {} {}: {}\n",
                    record.level(),
                    record.target(),
                    record.args()
                );
                let mut file: &std::fs::File = file;
                let _ = file.write_all(value.as_bytes());
            }
            #[cfg(unix)]
            PrimaryLogger::Syslog(logger) => logger.log(record),
        }
    }

    fn flush(&self) {
        match self {
            PrimaryLogger::File { file, .. } => {
                let mut file: &std::fs::File = file;
                let _ = file.flush();
            }

            #[cfg(unix)]
            PrimaryLogger::Syslog(logger) => logger.flush(),
        }
    }
}

//------------------------------------------------------------------------------

/// A prepared change to the [`Logger`].
pub struct PreparedChange {
    /// The primary logger, if changed.
    primary: Option<PrimaryLogger>,

    /// The log level filter.
    level: log::LevelFilter,

    /// The trace targets, if changed.
    trace_targets: Option<foldhash::HashSet<Box<str>>>,
}

impl From<LogLevel> for log::LevelFilter {
    fn from(value: LogLevel) -> Self {
        match value {
            LogLevel::Trace => log::LevelFilter::Trace,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Warning => log::LevelFilter::Warn,
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Critical => log::LevelFilter::Error, // TODO
        }
    }
}
