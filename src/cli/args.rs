use std::net::SocketAddr;

use clap::{command, Parser};

use crate::config::LogLevel;

use super::client::CascadeApiClient;
use super::commands::Command;

#[derive(Clone, Debug, Parser)]
#[command(version, disable_help_subcommand = true)]
pub struct Args {
    /// The cascade server instance to connect to
    #[arg(
        short = 's',
        long = "server",
        value_name = "IP:PORT",
        default_value = "127.0.0.1:8950"
    )]
    pub server: SocketAddr,

    /// The minimum severity of messages to log
    #[arg(long = "log-level", value_name = "LEVEL", default_value = "warning")]
    pub log_level: LogLevel,

    #[command(subcommand)]
    pub command: Command,
}

impl Args {
    pub async fn execute(self) -> Result<(), ()> {
        let client = CascadeApiClient::new(format!("http://{}", self.server));
        self.command.execute(client).await
    }
}
