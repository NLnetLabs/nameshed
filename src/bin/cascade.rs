use std::process::ExitCode;

use cascade::{
    cli::args::Args,
    config::{LogTarget, LoggingConfig, Setting},
    log::Logger,
};
use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    let logger = Logger::launch();

    let args = Args::parse();

    let log_config = LoggingConfig {
        level: Setting::new(args.log_level),
        target: Setting::new(LogTarget::File("/dev/stdout".into())),
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
