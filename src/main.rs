use clap::{crate_authors, crate_version, error::ErrorKind, Command};
use log::{error, info, warn};
use nameshed::{
    log::{ExitError, LogConfig, Terminate},
    manager::Manager,
};
use std::process::exit;
use tokio::{
    runtime::{self, Runtime},
    signal::{self, unix::signal, unix::SignalKind},
};

fn run_with_cmdline_args() -> Result<(), Terminate> {
    LogConfig::init_logging()?;

    let app = Command::new("nameshed")
        .version(crate_version!())
        .author(crate_authors!())
        .next_line_help(true);

    let _matches = app.try_get_matches().map_err(|err| {
        let _ = err.print();
        match err.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => Terminate::normal(),
            _ => Terminate::other(2),
        }
    })?;

    // TODO: Drop privileges, get listen fd from systemd, create PID file,
    // fork, detach from the parent process, change user and group, etc. In a
    // word: daemonize. Prior art:
    //   - https://github.com/NLnetLabs/routinator/blob/main/src/operation.rs#L509
    //   - https://github.com/NLnetLabs/routinator/blob/main/src/process.rs#L241
    //   - https://github.com/NLnetLabs/routinator/blob/main/src/process.rs#L363

    LogConfig::default().switch_logging(true)?;

    let mut manager = Manager::new();
    let runtime = run_with_config(&mut manager)?;
    runtime.block_on(handle_signals(manager))?;
    Ok(())
}

async fn handle_signals(mut manager: Manager) -> Result<(), ExitError> {
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

fn run_with_config(manager: &mut Manager) -> Result<Runtime, ExitError> {
    let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Make the runtime the default for Tokio related functions that assume a
    // default runtime.
    let _guard = runtime.enter();

    nameshed::http::Server::new(vec!["0.0.0.0:8080".parse().unwrap()])
        .run(manager.metrics(), manager.http_resources())?;

    manager.spawn();

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
