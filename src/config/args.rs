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
            Arg::new("log_target")
                .short('l')
                .long("log")
                .value_name("TARGET")
                .value_parser(ValueParser::new(LogTargetSpecParser))
                .help("Where logs should be written to"),
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
            log_target: matches.get_one::<LogTargetSpec>("log_target").cloned(),
            daemonize: matches.get_flag("daemonize"),
        }
    }

    /// Merge this into a [`Config`].
    pub fn merge(self, config: &mut Config) {
        let daemon = &mut config.daemon;
        let source = SettingSource::Args;
        daemon.logging.level.merge_value(self.log_level, source);
        daemon
            .logging
            .target
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

//--- Parsing

#[derive(Clone, Debug, Default)]
pub struct LogTargetSpecParser;

impl clap::builder::TypedValueParser for LogTargetSpecParser {
    type Value = LogTargetSpec;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        // NOTE: Clap's own value parser types use 'Error::invalid_value()' to
        // produce the appropriate parsing errors, but this is not a publicly
        // visible function.  It performs a lot of useful work, like printing
        // possible values and suggesting the closest one.  To work around this,
        // we delegate to one of those value parsers on error.

        let s = value.to_str().ok_or_else(|| {
            let parser = clap::builder::StringValueParser::default();
            parser.parse_ref(cmd, arg, value).unwrap_err()
        })?;

        if s == "stdout" {
            Ok(LogTargetSpec::File("/dev/stdout".into()))
        } else if s == "stderr" {
            Ok(LogTargetSpec::File("/dev/stderr".into()))
        } else if let Some(s) = s.strip_prefix("file:") {
            let path = <&Utf8Path>::from(s);
            Ok(LogTargetSpec::File(path.into()))
        } else if s == "syslog" {
            Ok(LogTargetSpec::Syslog)
        } else {
            let parser = clap::builder::PossibleValuesParser::new([
                "stdout",
                "stderr",
                "file:<PATH>",
                "syslog",
            ]);
            Err(parser.parse_ref(cmd, arg, value).unwrap_err())
        }
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        let values = ["stdout", "stderr", "file:<PATH>", "syslog"];
        Some(Box::new(values.into_iter().map(PossibleValue::new)))
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
