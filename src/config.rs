//! Configuration for the server.
//!
//! This module primarily contains the type [`Config`] that holds all the
//! configuration used. It can be loaded both from a TOML
//! formatted config file and command line options.

use std::{env, fmt, fs};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use clap::{App, Arg, ArgMatches};
use dirs::home_dir;
use log::{LevelFilter, error};
#[cfg(unix)] use syslog::Facility;
use crate::error::Failed;


//------------ Config --------------------------------------------------------  

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// The log levels to be logged.
    pub log_level: LevelFilter,

    /// The target to log to.
    pub log_target: LogTarget,
}

impl Config {
    /// Adds the basic arguments to a clapp app.
    ///
    /// The function follows clap’s builder pattern: it takes an app,
    /// adds a bunch of arguments to it and returns it at the end.
    pub fn config_args<'a: 'b, 'b>(app: App<'a, 'b>) -> App<'a, 'b> {
        app
        .arg(Arg::with_name("config")
             .short("c")
             .long("config")
             .takes_value(true)
             .value_name("PATH")
             .help("Read base configuration from this file")
        )
        .arg(Arg::with_name("plain-listen")
            .long("listen")
            .value_name("ADDR:PORT")
            .help("Listen on address/port for both plain UDP and TCP")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
        )
        .arg(Arg::with_name("udp-listen")
            .long("udp")
            .value_name("ADDR:PORT")
            .help("Listen on address/port for plain UDP")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
        )
        .arg(Arg::with_name("tcp-listen")
            .long("tcp")
            .value_name("ADDR:PORT")
            .help("Listen on address/port for plain TCP")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
        )
        .arg(Arg::with_name("verbose")
             .short("v")
             .long("verbose")
             .multiple(true)
             .help("Log more information, twice for even more")
        )
        .arg(Arg::with_name("quiet")
             .short("q")
             .long("quiet")
             .multiple(true)
             .conflicts_with("verbose")
             .help("Log less information, twice for no information")
        )
        .arg(Arg::with_name("syslog")
             .long("syslog")
             .help("Log to syslog")
        )
        .arg(Arg::with_name("syslog-facility")
             .long("syslog-facility")
             .takes_value(true)
             .default_value("daemon")
             .help("Facility to use for syslog logging")
        )
        .arg(Arg::with_name("logfile")
             .long("logfile")
             .takes_value(true)
             .value_name("PATH")
             .help("Log to this file")
        )
    }

    /// Creates a configuration from command line matches.
    ///
    /// The function attempts to create configuration from the command line
    /// arguments provided via `matches`. It will try to read a config file
    /// if provided via the config file option (`-c` or `--config`) or a
    /// file in `$HOME/.routinator.conf` otherwise. If the latter doesn’t
    /// exist either, starts with a default configuration.
    ///
    /// All relative paths given in command line arguments will be interpreted
    /// relative to `cur_dir`. Conversely, paths in the config file are
    /// treated as relative to the config file’s directory.
    ///
    /// If you are runming in server mode, you need to also apply the server
    /// arguments via [`apply_server_arg_matches`].
    ///
    /// [`apply_server_arg_matches`]: #method.apply_server_arg_matches
    pub fn from_arg_matches(
        matches: &ArgMatches,
        cur_dir: &Path,
    ) -> Result<Self, Failed> {
        let mut res = Self::create_base_config(
            Self::path_value_of(matches, "config", cur_dir)
                .as_ref().map(AsRef::as_ref)
        )?;

        res.apply_arg_matches(matches, cur_dir)?;

        Ok(res)
    }

    /// Creates the correct base configuration for the given config file path.
    /// 
    /// If no config path is given, tries to read the default config in
    /// `$HOME/.nameshed.conf`. If that doesn’t exist, creates a default
    /// config.
    fn create_base_config(path: Option<&Path>) -> Result<Self, Failed> {
        let file = match path {
            Some(path) => {
                match ConfigFile::read(path)? {
                    Some(file) => file,
                    None => {
                        error!("Cannot read config file {}", path.display());
                        return Err(Failed);
                    }
                }
            }
            None => {
                match home_dir() {
                    Some(dir) => match ConfigFile::read(
                                            &dir.join(".nameshed.conf"))? {
                        Some(file) => file,
                        None => return Ok(Self::default()),
                    }
                    None => return Ok(Self::default())
                }
            }
        };
        Self::from_config_file(file)
    }

    /// Creates a base config from a config file.
    fn from_config_file(mut file: ConfigFile) -> Result<Self, Failed> {
        let log_target = Self::log_target_from_config_file(&mut file)?;
        let res = Config {
            listen: file.take_from_str_array("listen")?.unwrap_or_else(||
                Vec::new()
            ),
            log_level: {
                file.take_from_str("log-level")?.unwrap_or(LevelFilter::Warn)
            },
            log_target,
        };
        file.check_exhausted()?;
        Ok(res)
    }

    /// Determines the logging target from the config file.
    ///
    /// This is the Unix version that also deals with syslog.
    #[cfg(unix)]
    fn log_target_from_config_file(
        file: &mut ConfigFile
    ) -> Result<LogTarget, Failed> {
        let facility = file.take_string("syslog-facility")?;
        let facility = facility.as_ref().map(AsRef::as_ref)
                               .unwrap_or("daemon");
        let facility = match Facility::from_str(facility) {
            Ok(value) => value,
            Err(_) => {
                error!(
                    "Failed in config file {}: invalid syslog-facility.",
                    file.path.display()
                );
                return Err(Failed);
            }
        };
        let log_target = file.take_string("log")?;
        let log_file = file.take_path("log-file")?;
        match log_target.as_ref().map(AsRef::as_ref) {
            Some("default") | None => Ok(LogTarget::Default(facility)),
            Some("syslog") => Ok(LogTarget::Syslog(facility)),
            Some("stderr") =>  Ok(LogTarget::Stderr),
            Some("file") => {
                match log_file {
                    Some(file) => Ok(LogTarget::File(file)),
                    None => {
                        error!(
                            "Failed in config file {}: \
                             log target \"file\" requires 'log-file' value.",
                            file.path.display()
                        );
                        Err(Failed)
                    }
                }
            }
            Some(value) => {
                error!(
                    "Failed in config file {}: \
                     invalid log target '{}'",
                     file.path.display(),
                     value
                );
                Err(Failed)
            }
        }
    }

    /// Determines the logging target from the config file.
    ///
    /// This is the non-Unix version that only logs to stderr or a file.
    #[cfg(not(unix))]
    fn log_target_from_config_file(
        file: &mut ConfigFile
    ) -> Result<LogTarget, Failed> {
        let log_target = file.take_string("log")?;
        let log_file = file.take_path("log-file")?;
        match log_target.as_ref().map(AsRef::as_ref) {
            Some("default") | Some("stderr") | None => Ok(LogTarget::Stderr),
            Some("file") => {
                match log_file {
                    Some(file) => Ok(LogTarget::File(file)),
                    None => {
                        error!(
                            "Failed in config file {}: \
                             log target \"file\" requires 'log-file' value.",
                            file.path.display()
                        );
                        Err(Failed)
                    }
                }
            }
            Some(value) => {
                error!(
                    "Failed in config file {}: \
                     invalid log target '{}'",
                    file.path.display(), value
                );
                Err(Failed)
            }
        }
    }

    /// Applies the basic command line arguments to a configuration.
    ///
    /// The path arguments in `matches` will be interpreted relative to
    /// `cur_dir`.
    #[allow(clippy::cognitive_complexity)]
    fn apply_arg_matches(
        &mut self,
        matches: &ArgMatches,
        cur_dir: &Path,
    ) -> Result<(), Failed> {
        // udp_listen
        if let Some(list) = matches.values_of("udp-listen") {
            for value in list {
                match SocketAddr::from_str(value) {
                    Ok(some) => self.listen.push(ListenAddr::Udp(some)),
                    Err(_) => {
                        error!("Invalid value for udp: {}", value);
                        return Err(Failed)
                    }
                }
            }
        }

        // tcp_listen
        if let Some(list) = matches.values_of("tcp-listen") {
            for value in list {
                match SocketAddr::from_str(value) {
                    Ok(some) => self.listen.push(ListenAddr::Tcp(some)),
                    Err(_) => {
                        error!("Invalid value for tcp: {}", value);
                        return Err(Failed)
                    }
                }
            }
        }

        // log_level
        match (matches.occurrences_of("verbose"),
                                            matches.occurrences_of("quiet")) {
            // This assumes that -v and -q are conflicting.
            (0, 0) => { }
            (1, 0) => self.log_level = LevelFilter::Info,
            (_, 0) => self.log_level = LevelFilter::Debug,
            (0, 1) => self.log_level = LevelFilter::Error,
            (0, _) => self.log_level = LevelFilter::Off,
            _ => { }
        }

        // log_target
        self.apply_log_matches(matches, cur_dir)?;

        Ok(())
    }

    /// Applies the logging-specific command line arguments to the config.
    ///
    /// This is the Unix version that also considers syslog as a valid
    /// target.
    #[cfg(unix)]
    fn apply_log_matches(
        &mut self,
        matches: &ArgMatches,
        cur_dir: &Path,
    ) -> Result<(), Failed> {
        if matches.is_present("syslog") {
            self.log_target = LogTarget::Syslog(
                match Facility::from_str(
                               matches.value_of("syslog-facility").unwrap()) {
                    Ok(value) => value,
                    Err(_) => {
                        error!("Invalid value for syslog-facility.");
                        return Err(Failed);
                    }
                }
            )
        }
        else if let Some(file) = matches.value_of("logfile") {
            if file == "-" {
                self.log_target = LogTarget::Stderr
            }
            else {
                self.log_target = LogTarget::File(cur_dir.join(file))
            }
        }
        Ok(())
    }

    /// Applies the logging-specific command line arguments to the config.
    ///
    /// This is the non-Unix version that does not use syslog.
    #[cfg(not(unix))]
    #[allow(clippy::unnecessary_wraps)]
    fn apply_log_matches(
        &mut self,
        matches: &ArgMatches,
        cur_dir: &Path,
    ) -> Result<(), Failed> {
        if let Some(file) = matches.value_of("logfile") {
            if file == "-" {
                self.log_target = LogTarget::Stderr
            }
            else {
                self.log_target = LogTarget::File(cur_dir.join(file))
            }
        }
        Ok(())
    }

    /// Returns a TOML representation of the config.
    pub fn to_toml(&self) -> toml::Value {
        unimplemented!()
    }

    /// Returns a path value in arg matches.
    ///
    /// This expands a relative path based on the given directory.
    fn path_value_of(
        matches: &ArgMatches,
        key: &str,
        dir: &Path
    ) -> Option<PathBuf> {
        matches.value_of(key).map(|path| dir.join(path))
    }
}


//--- Default

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: Vec::new(),
            log_level: LevelFilter::Warn,
            log_target: LogTarget::default(),
        }
    }
}


//--- Display

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.to_toml())
    }
}


//------------ ListenAddr ----------------------------------------------------

/// An address and the protocol to serve queries on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListenAddr {
    /// Plain, unencrypted UDP.
    Udp(SocketAddr),

    /// Plain, unencrypted TCP.
    Tcp(SocketAddr),
}

impl FromStr for ListenAddr {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (protocol, addr) = match value.split_once(":") {
            Some(stuff) => stuff,
            None => {
                return Err("expected string '<protocol>:<addr>'".into())
            }
        };
        let addr = match SocketAddr::from_str(addr) {
            Ok(addr) => addr,
            Err(_) => {
                return Err(format!("invalid listen address '{}'", addr))
            }
        };
        match protocol {
            "udp" => Ok(ListenAddr::Udp(addr)),
            "tcp" => Ok(ListenAddr::Tcp(addr)),
            other => {
                Err(format!("unknown protocol '{}'", other))
            }
        }
    }
}


//------------ LogTarget -----------------------------------------------------

/// The target to log to.
#[derive(Clone, Debug)]
pub enum LogTarget {
    /// Default.
    ///
    /// Logs to `Syslog(facility)` in daemon mode and `Stderr` otherwise.
    #[cfg(unix)]
    Default(Facility),

    /// Syslog.
    ///
    /// The argument is the syslog facility to use.
    #[cfg(unix)]
    Syslog(Facility),

    /// Stderr.
    Stderr,

    /// A file.
    ///
    /// The argument is the file name.
    File(PathBuf)
}


//--- Default

#[cfg(unix)]
impl Default for LogTarget {
    fn default() -> Self {
        LogTarget::Default(Facility::LOG_DAEMON)
    }
}

#[cfg(not(unix))]
impl Default for LogTarget {
    fn default() -> Self {
        LogTarget::Stderr
    }
}


//--- PartialEq and Eq

impl PartialEq for LogTarget {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            #[cfg(unix)]
            (&LogTarget::Default(s), &LogTarget::Default(o)) => {
                (s as usize) == (o as usize)
            }
            #[cfg(unix)]
            (&LogTarget::Syslog(s), &LogTarget::Syslog(o)) => {
                (s as usize) == (o as usize)
            }
            (&LogTarget::Stderr, &LogTarget::Stderr) => true,
            (&LogTarget::File(ref s), &LogTarget::File(ref o)) => {
                s == o
            }
            _ => false
        }
    }
}

impl Eq for LogTarget { }


//------------ ConfigFile ----------------------------------------------------

/// The content of a config file.
///
/// This is a thin wrapper around `toml::Table` to make dealing with it more
/// convenient.
#[derive(Clone, Debug)]
struct ConfigFile {
    /// The content of the file.
    content: toml::value::Table,

    /// The path to the config file.
    path: PathBuf,

    /// The directory we found the file in.
    ///
    /// This is used in relative paths.
    dir: PathBuf,
}

impl ConfigFile {
    /// Reads the config file at the given path.
    ///
    /// If there is no such file, returns `None`. If there is a file but it
    /// is broken, aborts.
    #[allow(clippy::verbose_file_reads)]
    fn read(path: &Path) -> Result<Option<Self>, Failed> {
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(_) => return Ok(None)
        };
        let mut config = String::new();
        if let Err(err) = file.read_to_string(&mut config) {
            error!(
                "Failed to read config file {}: {}",
                path.display(), err
            );
            return Err(Failed);
        }
        Self::parse(&config, path).map(Some)
    }

    /// Parses the content of the file from a string.
    fn parse(content: &str, path: &Path) -> Result<Self, Failed> {
        let content = match toml::from_str(content) {
            Ok(toml::Value::Table(content)) => content,
            Ok(_) => {
                error!(
                    "Failed to parse config file {}: Not a mapping.",
                    path.display()
                );
                return Err(Failed);
            }
            Err(err) => {
                error!(
                    "Failed to parse config file {}: {}",
                    path.display(), err
                );
                return Err(Failed);
            }
        };
        let dir = if path.is_relative() {
            path.join(match env::current_dir() {
                Ok(dir) => dir,
                Err(err) => {
                    error!(
                        "Fatal: Can't determine current directory: {}.",
                        err
                    );
                    return Err(Failed);
                }
            }).parent().unwrap().into() // a file always has a parent
        }
        else {
            path.parent().unwrap().into()
        };
        Ok(ConfigFile {
            content,
            path: path.into(),
            dir
        })
    }

    /*
    /// Takes a boolean value from the config file.
    ///
    /// The value is taken from the given `key`. Returns `Ok(None)` if there
    /// is no such key. Returns an error if the key exists but the value
    /// isn’t a booelan.
    fn take_bool(&mut self, key: &str) -> Result<Option<bool>, Failed> {
        match self.content.remove(key) {
            Some(value) => {
                if let toml::Value::Boolean(res) = value {
                    Ok(Some(res))
                }
                else {
                    error!(
                        "Failed in config file {}: \
                         '{}' expected to be a boolean.",
                        self.path.display(), key
                    );
                    Err(Failed)
                }
            }
            None => Ok(None)
        }
    }

    /// Takes an unsigned integer value from the config file.
    ///
    /// The value is taken from the given `key`. Returns `Ok(None)` if there
    /// is no such key. Returns an error if the key exists but the value
    /// isn’t an integer or if it is negative.
    fn take_u64(&mut self, key: &str) -> Result<Option<u64>, Failed> {
        match self.content.remove(key) {
            Some(value) => {
                if let toml::Value::Integer(res) = value {
                    if res < 0 {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a positive integer.",
                            self.path.display(), key
                        );
                        Err(Failed)
                    }
                    else {
                        Ok(Some(res as u64))
                    }
                }
                else {
                    error!(
                        "Failed in config file {}: \
                         '{}' expected to be an integer.",
                        self.path.display(), key
                    );
                    Err(Failed)
                }
            }
            None => Ok(None)
        }
    }

    /// Takes an unsigned integer value from the config file.
    ///
    /// The value is taken from the given `key`. Returns `Ok(None)` if there
    /// is no such key. Returns an error if the key exists but the value
    /// isn’t an integer or if it is negative.
    fn take_usize(&mut self, key: &str) -> Result<Option<usize>, Failed> {
        match self.content.remove(key) {
            Some(value) => {
                if let toml::Value::Integer(res) = value {
                    usize::try_from(res).map(Some).map_err(|_| {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a positive integer.",
                            self.path.display(), key
                        );
                        Failed
                    })
                }
                else {
                    error!(
                        "Failed in config file {}: \
                         '{}' expected to be an integer.",
                        self.path.display(), key
                    );
                    Err(Failed)
                }
            }
            None => Ok(None)
        }
    }

    /// Takes a small unsigned integer value from the config file.
    ///
    /// While the result is returned as an `usize`, it must be in the
    /// range of a `u16`.
    ///
    /// The value is taken from the given `key`. Returns `Ok(None)` if there
    /// is no such key. Returns an error if the key exists but the value
    /// isn’t an integer or if it is out of bounds.
    fn take_small_usize(&mut self, key: &str) -> Result<Option<usize>, Failed> {
        match self.content.remove(key) {
            Some(value) => {
                if let toml::Value::Integer(res) = value {
                    if res < 0 {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a positive integer.",
                            self.path.display(), key
                        );
                        Err(Failed)
                    }
                    else if res > ::std::u16::MAX.into() {
                        error!(
                            "Failed in config file {}: \
                            value for '{}' is too large.",
                            self.path.display(), key
                        );
                        Err(Failed)
                    }
                    else {
                        Ok(Some(res as usize))
                    }
                }
                else {
                    error!(
                        "Failed in config file {}: \
                         '{}' expected to be a integer.",
                        self.path.display(), key
                    );
                    Err(Failed)
                }
            }
            None => Ok(None)
        }
    }
    */

    /// Takes a string value from the config file.
    ///
    /// The value is taken from the given `key`. Returns `Ok(None)` if there
    /// is no such key. Returns an error if the key exists but the value
    /// isn’t a string.
    fn take_string(&mut self, key: &str) -> Result<Option<String>, Failed> {
        match self.content.remove(key) {
            Some(value) => {
                if let toml::Value::String(res) = value {
                    Ok(Some(res))
                }
                else {
                    error!(
                        "Failed in config file {}: \
                         '{}' expected to be a string.",
                        self.path.display(), key
                    );
                    Err(Failed)
                }
            }
            None => Ok(None)
        }
    }

    /// Takes a string encoded value from the config file.
    ///
    /// The value is taken from the given `key`. It is expected to be a
    /// string and will be converted to the final type via `FromStr::from_str`.
    ///
    /// Returns `Ok(None)` if the key doesn’t exist. Returns an error if the
    /// key exists but the value isn’t a string or conversion fails.
    fn take_from_str<T>(&mut self, key: &str) -> Result<Option<T>, Failed>
    where T: FromStr, T::Err: fmt::Display {
        match self.take_string(key)? {
            Some(value) => {
                match T::from_str(&value) {
                    Ok(some) => Ok(Some(some)),
                    Err(err) => {
                        error!(
                            "Failed in config file {}: \
                             illegal value in '{}': {}.",
                            self.path.display(), key, err
                        );
                        Err(Failed)
                    }
                }
            }
            None => Ok(None)
        }
    }

    /// Takes a path value from the config file.
    ///
    /// The path is taken from the given `key`. It must be a string value.
    /// It is treated as relative to the directory of the config file. If it
    /// is indeed a relative path, it is expanded accordingly and an absolute
    /// path is returned.
    ///
    /// Returns `Ok(None)` if the key does not exist. Returns an error if the
    /// key exists but the value isn’t a string.
    fn take_path(&mut self, key: &str) -> Result<Option<PathBuf>, Failed> {
        self.take_string(key).map(|opt| opt.map(|path| self.dir.join(path)))
    }

    /*
    /// Takes a mandatory path value from the config file.
    ///
    /// This is the pretty much the same as [`take_path`] but also returns
    /// an error if the key does not exist.
    ///
    /// [`take_path`]: #method.take_path
    fn take_mandatory_path(&mut self, key: &str) -> Result<PathBuf, Failed> {
        match self.take_path(key)? {
            Some(res) => Ok(res),
            None => {
                error!(
                    "Failed in config file {}: missing required '{}'.",
                    self.path.display(), key
                );
                Err(Failed)
            }
        }
    }

    /// Takes an array of strings from the config file.
    ///
    /// The value is taken from the entry with the given `key` and, if
    /// present, the entry is removed. The value must be an array of strings.
    /// If the key is not present, returns `Ok(None)`. If the entry is present
    /// but not an array of strings, returns an error.
    fn take_string_array(
        &mut self,
        key: &str
    ) -> Result<Option<Vec<String>>, Failed> {
        match self.content.remove(key) {
            Some(toml::Value::Array(vec)) => {
                let mut res = Vec::new();
                for value in vec.into_iter() {
                    if let toml::Value::String(value) = value {
                        res.push(value)
                    }
                    else {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a array of strings.",
                            self.path.display(),
                            key
                        );
                        return Err(Failed);
                    }
                }
                Ok(Some(res))
            }
            Some(_) => {
                error!(
                    "Failed in config file {}: \
                     '{}' expected to be a array of strings.",
                    self.path.display(), key
                );
                Err(Failed)
            }
            None => Ok(None)
        }
    }
    */

    /// Takes an array of string encoded values from the config file.
    ///
    /// The value is taken from the entry with the given `key` and, if
    /// present, the entry is removed. The value must be an array of strings.
    /// Each string is converted to the output type via `FromStr::from_str`.
    ///
    /// If the key is not present, returns `Ok(None)`. If the entry is present
    /// but not an array of strings or if converting any of the strings fails,
    /// returns an error.
    fn take_from_str_array<T>(
        &mut self,
        key: &str
    ) -> Result<Option<Vec<T>>, Failed>
    where T: FromStr, T::Err: fmt::Display {
        match self.content.remove(key) {
            Some(toml::Value::Array(vec)) => {
                let mut res = Vec::new();
                for value in vec.into_iter() {
                    if let toml::Value::String(value) = value {
                        match T::from_str(&value) {
                            Ok(value) => res.push(value),
                            Err(err) => {
                                error!(
                                    "Failed in config file {}: \
                                     Invalid value in '{}': {}",
                                    self.path.display(), key, err
                                );
                                return Err(Failed)
                            }
                        }
                    }
                    else {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a array of strings.",
                            self.path.display(),
                            key
                        );
                        return Err(Failed)
                    }
                }
                Ok(Some(res))
            }
            Some(_) => {
                error!(
                    "Failed in config file {}: \
                     '{}' expected to be a array of strings.",
                    self.path.display(), key
                );
                Err(Failed)
            }
            None => Ok(None)
        }
    }

    /*
    /// Takes an array of paths from the config file.
    ///
    /// The values are taken from the given `key` which must be an array of
    /// strings. Each path is treated as relative to the directory of the
    /// config file. All paths are expanded if necessary and are returned as
    /// absolute paths.
    ///
    /// Returns `Ok(None)` if the key does not exist. Returns an error if the
    /// key exists but the value isn’t an array of string.
    fn take_path_array(
        &mut self,
        key: &str
    ) -> Result<Option<Vec<PathBuf>>, Failed> {
        match self.content.remove(key) {
            Some(toml::Value::String(value)) => {
                Ok(Some(vec![self.dir.join(value)]))
            }
            Some(toml::Value::Array(vec)) => {
                let mut res = Vec::new();
                for value in vec.into_iter() {
                    if let toml::Value::String(value) = value {
                        res.push(self.dir.join(value))
                    }
                    else {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a array of paths.",
                            self.path.display(),
                            key
                        );
                        return Err(Failed);
                    }
                }
                Ok(Some(res))
            }
            Some(_) => {
                error!(
                    "Failed in config file {}: \
                     '{}' expected to be a array of paths.",
                    self.path.display(), key
                );
                Err(Failed)
            }
            None => Ok(None)
        }
    }

    /// Takes a string-to-string hashmap from the config file.
    fn take_string_map(
        &mut self,
        key: &str
    ) -> Result<Option<HashMap<String, String>>, Failed> {
        match self.content.remove(key) {
            Some(toml::Value::Array(vec)) => {
                let mut res = HashMap::new();
                for value in vec.into_iter() {
                    let mut pair = match value {
                        toml::Value::Array(pair) => pair.into_iter(),
                        _ => {
                            error!(
                                "Failed in config file {}: \
                                '{}' expected to be a array of string pairs.",
                                self.path.display(),
                                key
                            );
                            return Err(Failed);
                        }
                    };
                    let left = match pair.next() {
                        Some(toml::Value::String(value)) => value,
                        _ => {
                            error!(
                                "Failed in config file {}: \
                                '{}' expected to be a array of string pairs.",
                                self.path.display(),
                                key
                            );
                            return Err(Failed);
                        }
                    };
                    let right = match pair.next() {
                        Some(toml::Value::String(value)) => value,
                        _ => {
                            error!(
                                "Failed in config file {}: \
                                '{}' expected to be a array of string pairs.",
                                self.path.display(),
                                key
                            );
                            return Err(Failed);
                        }
                    };
                    if pair.next().is_some() {
                        error!(
                            "Failed in config file {}: \
                            '{}' expected to be a array of string pairs.",
                            self.path.display(),
                            key
                        );
                        return Err(Failed);
                    }
                    if res.insert(left, right).is_some() {
                        error!(
                            "Failed in config file {}: \
                            'duplicate item in '{}'.",
                            self.path.display(),
                            key
                        );
                        return Err(Failed);
                    }
                }
                Ok(Some(res))
            }
            Some(_) => {
                error!(
                    "Failed in config file {}: \
                     '{}' expected to be a array of string pairs.",
                    self.path.display(), key
                );
                Err(Failed)
            }
            None => Ok(None)
        }
    }
    */

    /// Checks whether the config file is now empty.
    ///
    /// If it isn’t, logs a complaint and returns an error.
    fn check_exhausted(&self) -> Result<(), Failed> {
        if !self.content.is_empty() {
            print!(
                "Failed in config file {}: Unknown settings ",
                self.path.display()
            );
            let mut first = true;
            for key in self.content.keys() {
                if !first {
                    print!(",");
                }
                else {
                    first = false
                }
                print!("{}", key);
            }
            error!(".");
            Err(Failed)
        }
        else {
            Ok(())
        }
    }
}

