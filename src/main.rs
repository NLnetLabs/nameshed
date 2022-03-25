//! The daemon binary.

use std::env::current_dir;
use std::process::exit;
use clap::{App, crate_authors, crate_version};
use log::error;
use nameshed::{Config, ExitError};

// Since `main` with a result currently insists on printing a message, but
// in our case we only get an `ExitError` if all is said and done, we make our
// own, more quiet version.
fn _main() -> Result<(), ExitError> {
    nameshed::operation::prepare()?;
    let cur_dir = match current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            error!(
                "Fatal: cannot get current directory ({}). Aborting.",
                err
            );
            return Err(ExitError::Generic);
        }
    };
    let matches = Config::config_args(
        App::new("nameshed")
            .version(crate_version!())
            .author(crate_authors!())
            .about("a name server")
    ).get_matches();
    let config = Config::from_arg_matches(
        &matches, &cur_dir
    )?;
    nameshed::operation::run(config)
}

fn main() {
    match _main() {
        Ok(_) => exit(0),
        Err(ExitError::Generic) => exit(1),
    }
}


