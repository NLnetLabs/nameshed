use super::Unit;
use crate::comms::{ApplicationCommand, GraphStatus, Terminated};
use crate::manager::Component;
use crate::metrics;
use crate::payload::Update;
use core::fmt::Display;
use core::time::Duration;
use domain::dnssec::sign::keys::keyset::{KeySet, UnixTime};
use domain::zonetree::Zone;
use log::{error, info};
use serde::Deserialize;
use serde_with::serde_as;
use std::collections::HashMap;
use std::fs::metadata;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::MissedTickBehavior;

#[derive(Debug)]
pub struct KeyManagerUnit {
    pub dnst_keyset_bin_path: PathBuf,
    pub dnst_keyset_data_dir: PathBuf,
    pub update_tx: mpsc::Sender<Update>,
    pub cmd_rx: mpsc::Receiver<ApplicationCommand>,
}

impl KeyManagerUnit {
    pub async fn run(self, component: Component) -> Result<(), Terminated> {
        // TODO: metrics and status reporting

        KeyManager::new(
            component,
            self.dnst_keyset_bin_path,
            self.dnst_keyset_data_dir,
            self.update_tx,
        )
        .run(self.cmd_rx)
        .await?;

        Ok(())
    }
}

//------------ KeyManager ----------------------------------------------------

struct KeyManager {
    component: Component,
    dnst_keyset_bin_path: PathBuf,
    dnst_keyset_data_dir: PathBuf,
    update_tx: mpsc::Sender<Update>,
    ks_info: Mutex<HashMap<String, KeySetInfo>>,
}

impl KeyManager {
    fn new(
        component: Component,
        dnst_keyset_bin_path: PathBuf,
        dnst_keyset_data_dir: PathBuf,
        update_tx: mpsc::Sender<Update>,
    ) -> Self {
        Self {
            component,
            dnst_keyset_bin_path,
            dnst_keyset_data_dir,
            update_tx,
            ks_info: Mutex::new(HashMap::new()),
        }
    }

    async fn run(
        self,
        cmd_rx: mpsc::Receiver<ApplicationCommand>,
    ) -> Result<(), crate::comms::Terminated> {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // NOTE: We are not expecting any application commands right now.
        let _ = cmd_rx;

        loop {
            interval.tick().await;
            self.tick().await;
        }
    }

    async fn tick(&self) {
        let zone_tree = self.component.unsigned_zones();
        let mut ks_info = self.ks_info.lock().await;
        for zone in zone_tree.load().iter_zones() {
            let apex_name = zone.apex_name().to_string();
            let state_path = Path::new("/tmp/").join(format!("{apex_name}.state"));
            if !state_path.exists() {
                continue;
            }

            let info = ks_info.get(&apex_name);
            let Some(info) = info else {
                let value = get_keyset_info(state_path);
                let _ = ks_info.insert(apex_name, value);
                continue;
            };

            let keyset_state_modified = file_modified(&state_path).unwrap();
            if keyset_state_modified != info.keyset_state_modified {
                // Keyset state file is modified. Update our data and
                // signal the signer to re-sign the zone.
                let new_info = get_keyset_info(&state_path);
                let _ = ks_info.insert(apex_name, new_info);
                self.update_tx
                    .send(Update::ResignZoneEvent {
                        zone_name: zone.apex_name().clone(),
                    })
                    .await
                    .unwrap();
                continue;
            }

            let Some(ref cron_next) = info.cron_next else {
                continue;
            };

            if *cron_next < UnixTime::now() {
                // Run cron
                let cfg_path = self.dnst_keyset_data_dir.join(format!("{apex_name}.cfg"));
                let mut args = vec!["keyset", "-c"];
                args.push(cfg_path.to_str().unwrap());
                args.push("cron");
                println!(
                    "Invoking keyset cron for zone {apex_name} with {}",
                    args.join(" ")
                );
                let Ok(res) = Command::new(&self.dnst_keyset_bin_path).args(args).output() else {
                    error!(
                        "Failed to invoke keyset binary at '{}",
                        self.dnst_keyset_bin_path.display()
                    );

                    // Clear cron_next.
                    let info = KeySetInfo {
                        cron_next: None,
                        keyset_state_modified: info.keyset_state_modified.clone(),
                        retries: 0,
                    };
                    let _ = ks_info.insert(apex_name, info);
                    continue;
                };

                if res.status.success() {
                    println!("CRON OUT: {}", String::from_utf8_lossy(&res.stdout));

                    // We expect cron to change the state file. If
                    // that is the case, get a new KeySetInfo and notify
                    // the signer.
                    let new_info = get_keyset_info(&state_path);
                    if new_info.keyset_state_modified != info.keyset_state_modified {
                        // Something happened. Update ks_info and signal the
                        // signer.
                        let new_info = get_keyset_info(&state_path);
                        let _ = ks_info.insert(apex_name, new_info);
                        self.update_tx
                            .send(Update::ResignZoneEvent {
                                zone_name: zone.apex_name().clone(),
                            })
                            .await
                            .unwrap();
                        continue;
                    }

                    // Nothing happened. Assume that the timing could be off.
                    // Try again in a minute. After a few tries log an error
                    // and give up.
                    let cron_next = cron_next.clone() + Duration::from_secs(60);
                    let new_info = KeySetInfo {
                        cron_next: Some(cron_next),
                        keyset_state_modified: info.keyset_state_modified.clone(),
                        retries: info.retries + 1,
                    };
                    if new_info.retries >= CRON_MAX_RETRIES {
                        error!(
                            "The command 'dnst keyset cron' for config {} failed to update state file {}", cfg_path.display(), state_path.display()
                        );

                        // Clear cron_next.
                        let info = KeySetInfo {
                            cron_next: None,
                            keyset_state_modified: info.keyset_state_modified.clone(),
                            retries: 0,
                        };
                        let _ = ks_info.insert(apex_name, info);
                        continue;
                    }
                    let _ = ks_info.insert(apex_name, new_info);
                    continue;
                } else {
                    println!("CRON ERR: {}", String::from_utf8_lossy(&res.stderr));
                    // Clear cron_next.
                    let info = KeySetInfo {
                        cron_next: None,
                        keyset_state_modified: info.keyset_state_modified.clone(),
                        retries: 0,
                    };
                    let _ = ks_info.insert(apex_name, info);
                }
            }
        }
    }
}

//------------ KeySetInfo ----------------------------------------------------

#[derive(Clone)]
pub struct KeySetInfo {
    keyset_state_modified: UnixTime,
    cron_next: Option<UnixTime>,
    retries: u32,
}

// Maximum number of times to try the cron command when the state file does
// not change.
const CRON_MAX_RETRIES: u32 = 5;

fn file_modified(filename: impl AsRef<Path>) -> Result<UnixTime, String> {
    let md = metadata(filename).unwrap();
    let modified = md.modified().unwrap();
    modified
        .try_into()
        .map_err(|e| format!("unable to convert from SystemTime: {e}"))
}

fn get_keyset_info(state_path: impl AsRef<Path>) -> KeySetInfo {
    // Get the modified time of the state file before we read
    // state file itself. This is safe if there is a concurrent
    // update.
    let keyset_state_modified = file_modified(&state_path).unwrap();

    /// Persistent state for the keyset command.
    /// Copied frmo the keyset branch of dnst.
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct KeySetState {
        /// Domain KeySet state.
        keyset: KeySet,

        dnskey_rrset: Vec<String>,
        ds_rrset: Vec<String>,
        cds_rrset: Vec<String>,
        ns_rrset: Vec<String>,
        cron_next: Option<UnixTime>,
    }

    let state = std::fs::read_to_string(state_path).unwrap();
    let state: KeySetState = serde_json::from_str(&state).unwrap();

    KeySetInfo {
        keyset_state_modified,
        cron_next: state.cron_next,
        retries: 0,
    }
}
