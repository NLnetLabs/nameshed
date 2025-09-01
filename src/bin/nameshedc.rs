use std::process::ExitCode;

use clap::Parser;
use nameshed::{
    cli::args::Args,
    config::{LogTarget, LoggingConfig, Setting, SettingSource},
    log::Logger,
};

#[tokio::main]
async fn main() -> ExitCode {
    let logger = Logger::launch();

    let args = Args::parse();

    let log_config = LoggingConfig {
        // TODO: Use the right sources
        level: Setting {
            source: SettingSource::Args,
            value: args.log_level,
        },
        target: Setting {
            source: SettingSource::Default,
            value: LogTarget::File("/dev/stdout".into()),
        },
        trace_targets: Default::default(),
    };
    if let Some(change) = logger.prepare(&log_config).unwrap() {
        logger.apply(change);
    }

    match args.execute().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
