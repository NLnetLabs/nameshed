use cascade::{
    center::{self, Center},
    comms::ApplicationCommand,
    config::Config,
    manager::{self, TargetCommand},
};
use clap::{crate_authors, crate_version};
use std::{
    process::ExitCode,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

fn main() -> ExitCode {
    // Initialize the logger in fallback mode.
    let logger = cascade::log::Logger::launch();

    // Set up the command-line interface.
    let cmd = clap::Command::new("cascade")
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

    // Construct the configuration.
    let config = match Config::init(&matches) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Cascade couldn't be configured: {error}");
            return ExitCode::FAILURE;
        }
    };

    if matches.get_flag("check_config") {
        return ExitCode::SUCCESS;
    }

    // TODO: daemonbase
    logger.apply(logger.prepare(&config.daemon.logging).unwrap().unwrap());

    // Prepare Cascade.
    let (app_cmd_tx, mut app_cmd_rx) = mpsc::unbounded_channel();
    let (update_tx, update_rx) = mpsc::unbounded_channel();
    let center = Arc::new(Center {
        state: Mutex::new(center::State::new(config)),
        logger,
        unsigned_zones: Default::default(),
        signed_zones: Default::default(),
        published_zones: Default::default(),
        old_tsig_key_store: Default::default(),
        app_cmd_tx,
        update_tx,
    });

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
        // Spawn Cascade's units.
        let mut center_tx = None;
        let mut unit_txs = Default::default();
        manager::spawn(&center, update_rx, &mut center_tx, &mut unit_txs);

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

                _ = manager::forward_app_cmds(&mut app_cmd_rx, &unit_txs) => {}
            }
        };

        // Shut down Cascade.
        center_tx
            .as_ref()
            .unwrap()
            .send(TargetCommand::Terminate)
            .unwrap();
        center_tx.as_ref().unwrap().closed().await;
        for (_name, tx) in unit_txs {
            tx.send(ApplicationCommand::Terminate).unwrap();
            tx.closed().await;
        }
        result
    })
}
