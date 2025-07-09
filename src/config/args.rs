//! Configuration from command-line arguments.

use camino::{Utf8Path, Utf8PathBuf};
use clap::{
    builder::{EnumValueParser, PathBufValueParser, PossibleValue, TypedValueParser, ValueParser},
    Arg, ArgMatches, Command, ValueEnum, ValueHint,
};

use super::LogLevel;

//----------- ArgSpec ----------------------------------------------------------

/// Configuration-related command-line arguments.
#[derive(Clone, Debug)]
pub struct ArgsSpec {
    /// The configuration file to load.
    pub config: Box<Utf8Path>,

    /// The minimum severity of messages to log.
    pub log_level: Option<LogLevel>,

    /// The file to write logs to.
    pub log_file: Option<Box<Utf8Path>>,
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
                // TODO: Import from 'config::file::default_path()'.
                .default_value("/etc/nameshed/config.toml")
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
        ])
    }

    /// Process parsed command-line arguments.
    pub fn process(matches: &ArgMatches) -> Self {
        Self {
            config: matches
                .get_one::<Utf8PathBuf>("config")
                .expect("'config' has a default value")
                .as_path()
                .into(),
            log_level: matches.get_one::<LogLevel>("log_level").copied(),
            log_file: matches
                .get_one::<Utf8PathBuf>("log_file")
                .map(|p| p.as_path().into()),
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
