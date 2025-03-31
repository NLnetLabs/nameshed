//! Options relating to managing the nameshed server.

use clap::Error;

use crate::cli::client::NameshedClient;
use crate::api;


//------------ Health --------------------------------------------------------

#[derive(clap::Parser)]
pub struct Health;

impl Health {
    pub async fn run(
        self, client: &NameshedClient
    ) -> Result<api::status::Success, Error> {
        // client.authorized().await
        Ok(api::status::Success)
    }
}
