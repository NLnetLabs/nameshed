use log::error;

use crate::http;
use crate::log::ExitError;

#[derive(Clone, Debug, clap::Args)]
pub struct Zone {
    #[command(subcommand)]
    command: ZoneCommand,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum ZoneCommand {
    /// Register a new zone
    #[command(name = "register")]
    Register,

    /// List registered zones
    #[command(name = "list")]
    List,
}

// From brainstorm in beginning of April 2025
// - Command: process an UPDATE message
// - Command: process a NOTIFY message
// - Command: reload a zone immediately
// - Command: register a new zone
// - Command: de-register a zone
// - Command: reconfigure a zone

impl Zone {
    pub async fn execute(self) -> Result<(), ExitError> {
        match self.command {
            ZoneCommand::Register => {}
            ZoneCommand::List => {
                println!(
                    "Response: {:?}",
                    http::get_text("http://127.0.0.1:8950/hallo").await.map_err(|e| {
                        error!("HTTP request failed: {e}");
                        ExitError
                    })?
                )
            }
        }
        Ok(())
    }
}
