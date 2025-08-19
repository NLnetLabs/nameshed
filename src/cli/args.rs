use std::net::SocketAddr;

use clap::{command, Parser};

use crate::log::ExitError;

use super::commands::Command;

#[derive(Clone, Debug, Parser)]
#[command(version, disable_help_subcommand = true)]
pub struct Args {
    /// The nameshed server instance to connect to
    #[arg(
        short = 's',
        long = "server",
        value_name = "IP:PORT",
        default_value = "127.0.0.1:8950"
    )]
    pub server: SocketAddr,

    /// Verbosity: 0-5 or a level name ("off", "error", "warn", "info", "debug" or "trace")
    #[arg(
        short = 'v',
        long = "verbosity",
        value_name = "LEVEL",
        default_value = "warn",
    )]
    pub verbosity: crate::log::LogFilter,

    #[command(subcommand)]
    pub command: Command,
}

impl Args {
    pub async fn execute(self) -> Result<(), ExitError> {
        self.command.execute().await
    }
}
