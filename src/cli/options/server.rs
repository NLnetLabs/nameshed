//! Options relating to managing the nameshed server.

use crate::api;
use crate::cli::client::NameshedClient;
use crate::common::httpclient::Error;

//------------ Health --------------------------------------------------------

#[derive(clap::Parser)]
pub struct Health;

impl Health {
    pub async fn run(
        self,
        client: &NameshedClient,
        // ) -> Result<api::status::Success, Error> {
    ) -> Result<String, Error> {
        client.health().await
    }
}
