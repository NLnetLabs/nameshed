use bytes::Bytes;
use domain::base::Name;
use futures::TryFutureExt;
use log::error;

use crate::api::{ServerStatusResult, ZoneStatusResult};
use crate::cli::client::NameshedApiClient;

#[derive(Clone, Debug, clap::Args)]
pub struct Status {
    #[command(subcommand)]
    command: Option<StatusCommand>,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum StatusCommand {
    /// Show status of a specific zone
    #[command(name = "zone")]
    Zone { name: Name<Bytes> },

    /// Show status of DNSSEC keys
    #[command(name = "keys")]
    Keys,
}

// From discussion in August 2025
// - get status (what zones are there, what are things doing)
// - get dnssec status on zone
//   - maybe have it both on server level status command (so here) and in the zone command?

impl Status {
    pub async fn execute(self, client: NameshedApiClient) -> Result<(), ()> {
        match self.command {
            Some(StatusCommand::Zone { name }) => {
                // TODO: move to function that can be called by the general
                // status command with a zone arg?
                let url = format!("/zone/{name}/status");
                let response: ZoneStatusResult = client
                    .get(&url)
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Success: Sent zone reload command for {}", name)
            }
            Some(_) => todo!(),
            None => {
                let response: ServerStatusResult = client
                    .get("/status")
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Server status: {:?}", response)
            }
        }
        Ok(())
    }
}
