//! The nameshed command line client.

use std::{env, process};
use nameshed::cli::client::NameshedClient;
use nameshed::cli::options::Options;


//------------ main ----------------------------------------------------------

#[tokio::main]
async fn main() {
    let options = Options::from_args();
    let client = NameshedClient::new(
        options.general.server, options.general.token
    );
    let report = options.command.run(&client).await;
    let status = report.report(options.general.format);
    process::exit(status);
}

