//! The options for the nameshed command line client.

//------------ Sub-modules ---------------------------------------------------

mod args;
mod server;

//------------ Content -------------------------------------------------------

use clap::Parser;
use url::Url;

use crate::api::AuthToken;
use crate::cli::client::NameshedClient;
use crate::cli::report::{Report, ReportFormat};

//------------ Options -------------------------------------------------------

/// The command line options for the nameshed client.
#[derive(clap::Parser)]
#[command(version, about = "The nameshed command line client.")]
pub struct Options {
    #[command(flatten)]
    pub general: GeneralOptions,

    #[command(subcommand)]
    pub command: Command,
}

impl Options {
    /// Creates the options from the process arguments.
    ///
    /// If the arguments won’t result in usable options, exits the process.
    pub fn from_args() -> Self {
        Self::parse()
    }
}

//------------ GeneralOptions ------------------------------------------------

/// The options common between all command line tools.
#[derive(clap::Args)]
#[command(version)]
pub struct GeneralOptions {
    /// The full URI to the nameshed server.
    #[arg(
        short,
        long,
        env = "NAMESHED_CLI_SERVER",
        default_value = "http://localhost:8080/"
    )]
    pub server: Url,

    /// The secret token for the nameshed server.
    #[arg(short, long, env = "NAMESHED_CLI_TOKEN")]
    pub token: AuthToken,

    /// Report format
    #[arg(short, long, env = "NAMESHED_CLI_FORMAT", default_value = "text")]
    pub format: ReportFormat,

    /// Only show the API call and exit.
    #[arg(long)]
    pub api: bool,
}

//------------ Command -------------------------------------------------------

#[derive(clap::Subcommand)]
pub enum Command {
    /// Perform an authenticated health check.
    Health(server::Health),
}

impl Command {
    pub async fn run(self, client: &NameshedClient) -> Report {
        match self {
            Self::Health(cmd) => cmd.run(client).await.into(),
        }
    }
}
