//! The commands of _nameshedc_.

pub mod policy;
pub mod status;
pub mod zone;

use super::client::NameshedApiClient;

#[derive(Clone, Debug, clap::Subcommand)]
pub enum Command {
    /// Manage zones
    #[command(name = "zone")]
    Zone(self::zone::Zone),

    /// Get the status of different systems
    #[command(name = "status")]
    Status(self::status::Status),
    // - get status (what zones are there, what are things doing)
    // - get dnssec status on zone
    //
    /// Manage policies
    #[command(name = "policy")]
    Policy(self::policy::Policy),
    //
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
    pub async fn execute(self, client: NameshedApiClient) -> Result<(), ()> {
        match self {
            Self::Zone(zone) => zone.execute(client).await,
            Self::Status(status) => status.execute(client).await,
            Self::Policy(policy) => policy.execute(client).await,
            // Self::Help(help) => help.execute(),
        }
    }
}
