//! Configuration from command-line arguments.

use camino::{Utf8Path, Utf8PathBuf};
use clap::{
    builder::{EnumValueParser, PathBufValueParser, PossibleValue, TypedValueParser, ValueParser},
    Arg, ArgMatches, Command, ValueEnum, ValueHint,
};

use super::{Config, LogLevel, SettingSource};

//----------- ArgSpec ----------------------------------------------------------

/// Configuration-related command-line arguments.
#[derive(Clone, Debug)]
pub struct ArgsSpec {
    /// The configuration file to load.
    pub config: Option<Box<Utf8Path>>,

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
                .map(|p| p.as_path().into()),
            log_level: matches.get_one::<LogLevel>("log_level").copied(),
            log_file: matches
                .get_one::<Utf8PathBuf>("log_file")
                .map(|p| p.as_path().into()),
        }
    }

    /// Merge this into a [`Config`].
    pub fn merge(self, config: &mut Config) {
        let daemon = &mut config.daemon;
        let source = SettingSource::Args;
        daemon.log_level.merge_value(self.log_level, source);
        daemon.log_file.merge_value(self.log_file, source);
        daemon.config_file.merge_value(self.config, source);
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
