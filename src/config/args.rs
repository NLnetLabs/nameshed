//! Configuration from command-line arguments.

use camino::{Utf8Path, Utf8PathBuf};
use clap::{
    builder::{EnumValueParser, PathBufValueParser, PossibleValue, TypedValueParser, ValueParser},
    Arg, ArgMatches, Command, ValueEnum, ValueHint,
};

use super::{Config, LogLevel, LogTarget, SettingSource};

//----------- ArgSpec ----------------------------------------------------------

/// Configuration-related command-line arguments.
#[derive(Clone, Debug)]
pub struct ArgsSpec {
    /// The configuration file to load.
    pub config: Option<Box<Utf8Path>>,

    /// The minimum severity of messages to log.
    pub log_level: Option<LogLevel>,

    /// The target of log messages.
    pub log_target: Option<LogTargetSpec>,

    /// Whether Nameshed should fork on startup.
    pub daemonize: bool,
}

impl ArgsSpec {
    /// Set up a [`clap::Command`] with config-related arguments.
    pub fn setup(cmd: Command) -> Command {
        cmd.args([
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("PATH")
                .value_parser(ValueParser::new(
                    PathBufValueParser::new().try_map(Utf8PathBuf::try_from),
                ))
                .value_hint(ValueHint::FilePath)
                .help("The configuration file to load"),
            Arg::new("log_level")
                .long("log-level")
                .value_name("LEVEL")
                .value_parser(EnumValueParser::<LogLevel>::new())
                .help("The minimum severity of messages to log"),
            Arg::new("log_file")
                .long("log-file")
                .value_name("PATH")
                .value_parser(ValueParser::new(
                    PathBufValueParser::new().try_map(Utf8PathBuf::try_from),
                ))
                .value_hint(ValueHint::FilePath)
                .help("The file to write logs to"),
            Arg::new("syslog")
                .long("syslog")
                .conflicts_with("log_file")
                .hide(cfg!(not(unix)))
                .action(clap::ArgAction::SetTrue)
                .help("Whether to output to syslog"),
            Arg::new("daemonize")
                .short('d')
                .long("daemonize")
                .action(clap::ArgAction::SetTrue)
                .help("Whether Nameshed should fork on startup"),
        ])
    }

    /// Process parsed command-line arguments.
    pub fn process(matches: &ArgMatches) -> Self {
        Self {
            config: matches
                .get_one::<Utf8PathBuf>("config")
                .map(|p| p.as_path().into()),
            log_level: matches.get_one::<LogLevel>("log_level").copied(),
            log_target: matches
                .get_one::<Utf8PathBuf>("log_file")
                .map(|p| LogTargetSpec::File(p.as_path().into()))
                .or_else(|| matches.get_flag("syslog").then_some(LogTargetSpec::Syslog)),
            daemonize: matches.get_flag("daemonize"),
        }
    }

    /// Merge this into a [`Config`].
    pub fn merge(self, config: &mut Config) {
        let daemon = &mut config.daemon;
        let source = SettingSource::Args;
        daemon.log_level.merge_value(self.log_level, source);
        daemon
            .log_target
            .merge_value(self.log_target.map(|t| t.build()), source);
        daemon.config_file.merge_value(self.config, source);
        daemon
            .daemonize
            .merge_value(self.daemonize.then_some(true), source);
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

//------------------------------------------------------------------------------

impl ValueEnum for LogLevel {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warning,
            LogLevel::Error,
            LogLevel::Critical,
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.as_str()))
    }
}
