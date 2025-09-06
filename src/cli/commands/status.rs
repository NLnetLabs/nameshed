use futures::TryFutureExt;
use log::error;

use crate::api::ServerStatusResult;
use crate::cli::client::CascadeApiClient;

#[derive(Clone, Debug, clap::Args)]
pub struct Status {
    #[command(subcommand)]
    command: Option<StatusCommand>,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum StatusCommand {
    /// Show status of DNSSEC keys
    #[command(name = "keys")]
    Keys,
}

// From discussion in August 2025
// - get status (what zones are there, what are things doing)
// - get dnssec status on zone
//   - maybe have it both on server level status command (so here) and in the zone command?

impl Status {
    pub async fn execute(self, client: CascadeApiClient) -> Result<(), ()> {
        match self.command {
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
