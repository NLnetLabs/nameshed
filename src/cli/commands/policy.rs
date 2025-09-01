use futures::TryFutureExt;
use log::error;

use crate::{
    api::{PolicyListResult, PolicyReloadResult},
    cli::client::CascadeApiClient,
};

#[derive(Clone, Debug, clap::Args)]
pub struct Policy {
    #[command(subcommand)]
    command: PolicyCommand,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum PolicyCommand {
    /// List registered policies
    #[command(name = "list")]
    List,

    /// Show the settings contained in a policy
    #[command(name = "show")]
    Show { name: String },

    /// Reload all the policies from the files
    Reload,
}

impl Policy {
    pub async fn execute(self, client: CascadeApiClient) -> Result<(), ()> {
        match self.command {
            PolicyCommand::List => {
                let res: PolicyListResult = client
                    .get("policy/list")
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                for policy in res.policies {
                    println!("{policy}");
                }
            }
            PolicyCommand::Show { name } => {
                let res: PolicyListResult = client
                    .get(&format!("policy/{name}"))
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Policy info: {res:?}");
            }
            PolicyCommand::Reload => {
                let _res: PolicyReloadResult = client
                    .post("policy/reload")
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Policies reloaded");
            }
        }
        Ok(())
    }
}
