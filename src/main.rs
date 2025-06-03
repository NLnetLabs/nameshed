use clap::{crate_authors, crate_version, error::ErrorKind, Command};
use log::{debug, error, info, warn};
use nameshed::{
    config::{Config, ConfigFile, Source},
    log::{ExitError, Terminate},
    manager::Manager,
};
use std::env::current_dir;
use std::process::exit;
use tokio::{
    runtime::{self, Runtime},
    signal::{self, unix::signal, unix::SignalKind},
};

fn run_with_cmdline_args() -> Result<(), Terminate> {
    Config::init()?;

    let app = Command::new("rotonda")
        .version(crate_version!())
        .author(crate_authors!())
        .next_line_help(true);

    let config_args = Config::config_args(app);
    let matches = config_args.try_get_matches().map_err(|err| {
        let _ = err.print();
        match err.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => Terminate::normal(),
            _ => Terminate::other(2),
        }
    })?;

    let cur_dir = current_dir().map_err(|err| {
        error!("Fatal: cannot get current directory ({}). Aborting.", err);
        ExitError
    })?;

    // TODO: Drop privileges, get listen fd from systemd, create PID file,
    // fork, detach from the parent process, change user and group, etc. In a
    // word: daemonize. Prior art:
    //   - https://github.com/NLnetLabs/routinator/blob/main/src/operation.rs#L509
    //   - https://github.com/NLnetLabs/routinator/blob/main/src/process.rs#L241
    //   - https://github.com/NLnetLabs/routinator/blob/main/src/process.rs#L363

    let mut manager = Manager::new();
    let (config_source, config) = Config::from_arg_matches(&matches, &cur_dir, &mut manager)?;
    debug!("application working directory {:?}", cur_dir);
    debug!("configuration source file {:?}", config_source);
    debug!("roto script {:?}", &config.roto_script);
    let roto_script = config.roto_script.clone();
    let runtime = run_with_config(&mut manager, config)?;
    runtime.block_on(handle_signals(config_source, roto_script, manager))?;
    Ok(())
}

async fn handle_signals(
    config_source: Source,
    roto_script: Option<std::path::PathBuf>,
    mut manager: Manager,
) -> Result<(), ExitError> {
    let mut hup_signals = signal(SignalKind::hangup()).map_err(|err| {
        error!("Fatal: cannot listen for HUP signals ({}). Aborting.", err);
        ExitError
    })?;

    loop {
        // let ctrl_c = signal::ctrl_c();
        // pin_mut!(ctrl_c);

        // let hup = hup_signals.recv();
        // pin_mut!(hup);

        tokio::select! {
            res = hup_signals.recv() => {
                match res {
                    None => {
                        error!("Fatal: listening for SIGHUP signals failed. Aborting.");
                        manager.terminate();
                        return Err(ExitError);
                    }
                    Some(_) => {
                        // HUP signal received
                        match config_source.path() {
                            Some(config_path) => {
                                info!(
                                    "SIGHUP signal received, re-reading configuration file '{}'",
                                    config_path.display()
                                );
                                match ConfigFile::load(&config_path) {
                                    Ok(config_file) => {
                                        match Config::from_config_file(config_file, &mut manager) {
                                            Err(_) => {
                                                error!(
                                                    "Failed to re-read config file '{}'",
                                                    config_path.display()
                                                );
                                            }
                                            Ok((_source, mut config)) => {
                                                manager.spawn(&mut config);
                                                info!("Configuration changes applied");
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        error!(
                                            "Failed to re-read config file '{}': {}",
                                            config_path.display(),
                                            err
                                        );
                                    }
                                }
                            }
                            None => {
                                if let Some(ref rsp) = roto_script {
                                    info!(
                                        "SIGHUP signal received, re-loading roto scripts \
                                    from location {:?}",
                                        rsp
                                    );
                                } else {
                                    error!("No location for roto scripts. Not reloading");
                                    continue;
                                }
                                // match manager.compile_roto_script(&roto_script) {
                                //     Ok(_) => {
                                //         info!("Done reloading roto scripts");
                                //     }
                                //     Err(e) => {
                                //         error!("Cannot reload roto scripts: {e}. Not reloading");
                                //     }
                                // };
                            }
                        }
                    }
                }
            }

            res = signal::ctrl_c() => {
                match res {
                    Err(err) => {
                        error!(
                            "Fatal: listening for CTRL-C (SIGINT) signals failed \
                            ({}). Aborting.",
                            err
                        );
                        manager.terminate();
                        return Err(ExitError);
                    }
                    Ok(_) => {
                        // CTRL-C received
                        warn!("CTRL-C (SIGINT) received, shutting down.");
                        manager.terminate();
                        return Ok(());
                    }
                }
            }

            _ = manager.accept_application_commands() => {

            }
        }
    }
}

fn run_with_config(manager: &mut Manager, mut config: Config) -> Result<Runtime, ExitError> {
    let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Make the runtime the default for Tokio related functions that assume a
    // default runtime.
    let _guard = runtime.enter();

    config
        .http
        .run(manager.metrics(), manager.http_resources())?;

    manager.spawn(&mut config);

    Ok(runtime)
}

fn main() {
    let exit_code = match run_with_cmdline_args() {
        Ok(_) => Terminate::normal(),
        Err(terminate) => terminate,
    }
    .exit_code();

    info!("Exiting with exit code {exit_code}");

    exit(exit_code);
}
