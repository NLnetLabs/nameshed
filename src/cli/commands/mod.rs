//! The commands of _nameshedc_.

pub mod zone;

use crate::log::ExitError;

#[derive(Clone, Debug, clap::Subcommand)]
pub enum Command {
    /// Manage zones
    #[command(name = "zone")]
    Zone(self::zone::Zone),

    // /// Manage keys
    // #[command(name = "key")]
    // Key(self::key::Key),
    // - Command: add/remove/modify a zone
    // - Command: add/remove/modify a key for a zone
    // - Command: add/remove/modify a key

    // /// Manage signing operations
    // #[command(name = "signer")]
    // Signer(self::signer::Signer),
    // - Command: add/remove/modify a zone // TODO: ask Arya what we meant by that
    // - Command: resign a zone immediately (optionally with custom config)

    // /// Show the manual pages
    // Help(self::help::Help),
}

impl Command {
    pub async fn execute(self) -> Result<(), ExitError> {
        match self {
            Self::Zone(zone) => zone.execute().await,
            // Self::Help(help) => help.execute(),
        }
    }
}
