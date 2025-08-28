use clap::{crate_authors, crate_version};
use nameshed::{config::Config, log::LogConfig, manager::Manager};
use std::process::ExitCode;

fn main() -> ExitCode {
    // TODO: Clean up
    LogConfig::init_logging().unwrap();

    // Set up the command-line interface.
    let cmd = clap::Command::new("nameshed")
        .version(crate_version!())
        .author(crate_authors!())
        .next_line_help(true)
        .arg(
            clap::Arg::new("check_config")
                .long("check-config")
                .action(clap::ArgAction::SetTrue)
                .help("Check the configuration and exit"),
        );
    let cmd = Config::setup_cli(cmd);

    // Process command-line arguments.
    let matches = cmd.get_matches();

    // TODO: Load the state instead of the config file, and merge the args and
    // env with whatever's in there.  Only load the config file when the user
    // explicitly requests it.

    // Construct the configuration.
    let _config = match Config::process(&matches) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Nameshed couldn't be configured: {error}");
            return ExitCode::FAILURE;
        }
    };

    if matches.get_flag("check_config") {
        return ExitCode::SUCCESS;
    }

    // TODO: daemonbase

    // Set up an async runtime.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Couldn't start Tokio: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Enter the runtime.
    runtime.block_on(async {
        // TODO: Clean up.

        LogConfig::default().switch_logging(true).unwrap();
        let mut manager = Manager::new();
        manager.spawn();

        // Let the manager run and handle external events.
        let result = loop {
            tokio::select! {
                // Watch for CTRL-C (SIGINT).
                res = tokio::signal::ctrl_c() => {
                    if let Err(error) = res {
                        log::error!(
                            "Listening for CTRL-C (SIGINT) failed: {error}"
                        );
                        break ExitCode::FAILURE;
                    }
                    break ExitCode::SUCCESS;
                }

                // TODO: Clean up.
                _ = manager.accept_application_commands() => {}
            }
        };

        // Shut down Nameshed.
        manager.terminate().await;
        result
    })
}
