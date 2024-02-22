//! Managing the process Nameshed runs in.

use crate::config::{Config, LogTarget};
use crate::error::Failed;
use log::{error, LevelFilter};
use std::io;
use std::path::Path;

//------------ Process -------------------------------------------------------

/// A representation of the process the server runs in.
///
/// This type provides access to the configuration and the environment in a
/// platform independent way.
pub struct Process {
    config: Config,
}

impl Process {
    pub fn init() -> Result<(), Failed> {
        Self::init_logging()?;

        Ok(())
    }

    /// Creates a new process object.
    ///
    pub fn new(config: Config) -> Self {
        Process { config }
    }

    /// Returns a reference to the config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Returns an exclusive reference to the config.
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }
}

/// # Logging
///
impl Process {
    /// Initialize logging.
    ///
    /// All diagnostic output is done via logging, never to
    /// stderr directly. Thus, it is important to initalize logging before
    /// doing anything else that may result in such output. This function
    /// does exactly that. It sets a maximum log level of `warn`, leading
    /// only printing important information, and directs all logging to
    /// stderr.
    fn init_logging() -> Result<(), Failed> {
        log::set_max_level(LevelFilter::Warn);
        if let Err(err) = log_reroute::init() {
            eprintln!("Failed to initialize logger: {}.\nAborting.", err);
            return Err(Failed);
        };
        let dispatch = fern::Dispatch::new()
            .level(LevelFilter::Error)
            .chain(io::stderr())
            .into_log()
            .1;
        log_reroute::reroute_boxed(dispatch);
        Ok(())
    }

    /// Switches logging to the configured target.
    ///
    /// Once the configuration has been successfully loaded, logging should
    /// be switched to whatever the user asked for via this method.
    #[allow(unused_variables)] // for cfg(not(unix))
    pub fn switch_logging(&self, daemon: bool) -> Result<(), Failed> {
        let logger = match self.config.log_target {
            #[cfg(unix)]
            LogTarget::Default(fac) => {
                if daemon {
                    self.syslog_logger(fac)?
                } else {
                    self.stderr_logger(false)
                }
            }
            #[cfg(unix)]
            LogTarget::Syslog(fac) => self.syslog_logger(fac)?,
            LogTarget::Stderr => self.stderr_logger(daemon),
            LogTarget::File(ref path) => self.file_logger(path)?,
        };

        log_reroute::reroute_boxed(logger.into_log().1);
        log::set_max_level(self.config.log_level);
        Ok(())
    }

    /// Creates a syslog logger and configures correctly.
    #[cfg(unix)]
    fn syslog_logger(&self, facility: syslog::Facility) -> Result<fern::Dispatch, Failed> {
        let process = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| String::from("routinator"));
        let formatter = syslog::Formatter3164 {
            facility,
            hostname: None,
            process,
            pid: nix::unistd::getpid().as_raw(),
        };
        let logger = syslog::unix(formatter.clone())
            .or_else(|_| syslog::tcp(formatter.clone(), ("127.0.0.1", 601)))
            .or_else(|_| syslog::udp(formatter, ("127.0.0.1", 0), ("127.0.0.1", 514)));
        match logger {
            Ok(logger) => Ok(self
                .fern_logger(false)
                .chain(Box::new(syslog::BasicLogger::new(logger)) as Box<dyn log::Log>)),
            Err(err) => {
                error!("Cannot connect to syslog: {}", err);
                Err(Failed)
            }
        }
    }

    /// Creates a stderr logger.
    ///
    /// If we are in daemon mode, we add a timestamp to the output.
    fn stderr_logger(&self, daemon: bool) -> fern::Dispatch {
        self.fern_logger(daemon).chain(io::stderr())
    }

    /// Creates a file logger using the file provided by `path`.
    fn file_logger(&self, path: &Path) -> Result<fern::Dispatch, Failed> {
        let file = match fern::log_file(path) {
            Ok(file) => file,
            Err(err) => {
                error!("Failed to open log file '{}': {}", path.display(), err);
                return Err(Failed);
            }
        };
        Ok(self.fern_logger(true).chain(file))
    }

    /// Creates and returns a fern logger.
    fn fern_logger(&self, timestamp: bool) -> fern::Dispatch {
        let mut res = fern::Dispatch::new();
        if timestamp {
            res = res.format(|out, message, _record| {
                out.finish(format_args!(
                    "{} {} {}",
                    chrono::Local::now().format("[%Y-%m-%d %H:%M:%S]"),
                    _record.module_path().unwrap_or(""),
                    message
                ))
            });
        }
        res = res
            .level(self.config.log_level)
            .level_for("rustls", LevelFilter::Error);
        /*
        if self.config.log_level == LevelFilter::Debug {
            res = res
                .level_for("tokio_reactor", LevelFilter::Info)
                .level_for("hyper", LevelFilter::Info)
                .level_for("reqwest", LevelFilter::Info)
                .level_for("h2", LevelFilter::Info)
                .level_for("sled", LevelFilter::Info);
        }
        */
        res
    }
}
