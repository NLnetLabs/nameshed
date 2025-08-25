use std::process::ExitCode;

use clap::Parser;
use nameshed::cli::args::Args;
use nameshed::log::LogConfig;

#[tokio::main]
async fn main() -> ExitCode {
    LogConfig::init_logging().unwrap();

    let args = Args::parse();

    let mut logging = LogConfig::default();
    logging.log_level = args.verbosity.clone();
    logging.switch_logging(false).unwrap();

    match args.execute().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
