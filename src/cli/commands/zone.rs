use bytes::Bytes;
use domain::base::Name;
use futures::TryFutureExt;
use log::error;

use crate::api::{ZoneAdd, ZoneAddResult, ZoneSource, ZoneStatusResult, ZonesListResult};
use crate::cli::client::CascadeApiClient;

#[derive(Clone, Debug, clap::Args)]
pub struct Zone {
    #[command(subcommand)]
    command: ZoneCommand,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum ZoneCommand {
    /// Register a new zone
    #[command(name = "add")]
    Add {
        name: Name<Bytes>,
        /// The zone source can be an IP address (with or without port,
        /// defaults to port 53) or a file path.
        // TODO: allow supplying different tcp and/or udp port?
        #[arg(long = "source")]
        source: ZoneSource,

        /// Policy to use for this zone
        #[arg(long = "policy")]
        policy: String,
    },

    /// Remove a zone
    #[command(name = "remove")]
    Remove { name: Name<Bytes> },

    /// List registered zones
    #[command(name = "list")]
    List,

    /// Reload a zone
    #[command(name = "reload")]
    Reload { zone: Name<Bytes> },

    /// Get the status of a single zone
    #[command(name = "status")]
    Status { zone: Name<Bytes> },
}

// From brainstorm in beginning of April 2025
// - Command: reload a zone immediately
// - Command: register a new zone
// - Command: de-register a zone
// - Command: reconfigure a zone

// From discussion in August 2025
// At least:
// - register zone
// - list zones
// - get status (what zones are there, what are things doing)
// - get dnssec status on zone
// - reload zone (i.e. from file)

impl Zone {
    pub async fn execute(self, client: CascadeApiClient) -> Result<(), ()> {
        match self.command {
            ZoneCommand::Add {
                name,
                source,
                policy,
            } => {
                let res: ZoneAddResult = client
                    .post("zone/add")
                    .json(&ZoneAdd {
                        name,
                        source,
                        policy,
                    })
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Registered zone {}", res.name);
            }
            ZoneCommand::Remove { name } => {
                let res: ZoneAddResult = client
                    .post(&format!("zone/{name}/remove"))
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Removed zone {}", res.name);
            }
            ZoneCommand::List => {
                let response: ZonesListResult = client
                    .get("zones/list")
                    .send()
                    .and_then(|r| r.json())
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                for zone in response.zones {
                    Self::print_zone_status(zone);
                }
            }
            ZoneCommand::Reload { zone } => {
                let url = format!("zone/{zone}/reload");
                client
                    .post(&url)
                    .send()
                    .and_then(|r| async { r.error_for_status() })
                    .await
                    .map_err(|e| {
                        error!("HTTP request failed: {e}");
                    })?;

                println!("Success: Sent zone reload command for {}", zone);
            }
            ZoneCommand::Status { zone } => {
                Self::status(client, zone).await?;
            }
        }
        Ok(())
    }

    async fn status(client: CascadeApiClient, zone: Name<Bytes>) -> Result<(), ()> {
        // TODO: move to function that can be called by the general
        // status command with a zone arg?
        let url = format!("zone/{}/status", zone);
        let response: ZoneStatusResult = client
            .get(&url)
            .send()
            .and_then(|r| r.json())
            .await
            .map_err(|e| {
                error!("HTTP request failed: {e}");
            })?;

        Self::print_zone_status(response);
        Ok(())
    }

    fn print_zone_status(zone: ZoneStatusResult) {
        println!("{}", zone.name);
        println!("  source: {}", zone.source);
        println!("  policy: {}", zone.policy);
        println!("  stage: {}", zone.stage);

        if let Some(key_status) = zone.key_status {
            println!("  key:");
            for line in key_status.lines() {
                println!("    {line}");
            }
        } else {
            println!("  key: <none>");
        }
    }
}
