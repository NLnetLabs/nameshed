use super::Unit;
use crate::common::status_reporter::{AnyStatusReporter, Chainable, Named, UnitStatusReporter};
use crate::comms::{Gate, GateMetrics, GateStatus, GraphStatus, Terminated};
use crate::manager::{Component, WaitPoint};
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
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;

#[serde_as]
#[derive(Clone, Debug, Deserialize)]
pub struct KeyManagerUnit {
    dnst_keyset_bin_path: PathBuf,

    dnst_keyset_data_dir: PathBuf,
}

impl KeyManagerUnit {
    pub async fn run(
        self,
        mut component: Component,
        gate: Gate,
        mut waitpoint: WaitPoint,
    ) -> Result<(), Terminated> {
        let unit_name = component.name().clone();

        // Setup our metrics
        let metrics = Arc::new(KeyManagerMetrics::new(&gate));
        component.register_metrics(metrics.clone());

        // Setup our status reporting
        let status_reporter = Arc::new(KeyManagerStatusReporter::new(&unit_name, metrics.clone()));

        // Wait for other components to be, and signal to other components
        // that we are, ready to start. All units and targets start together,
        // otherwise data passed from one component to another may be lost if
        // the receiving component is not yet ready to accept it.
        gate.process_until(waitpoint.ready()).await?;

        // Signal again once we are out of the process_until() so that anyone
        // waiting to send important gate status updates won't send them while
        // we are in process_until() which will just eat them without handling
        // them.
        waitpoint.running().await;

        KeyManager::new(component, gate, metrics, status_reporter)
            .run(self.dnst_keyset_bin_path, self.dnst_keyset_data_dir)
            .await?;

        Ok(())
    }
}

//------------ KeyManager ----------------------------------------------------

struct KeyManager {
    component: Component,
    gate: Gate,
    metrics: Arc<KeyManagerMetrics>,
    status_reporter: Arc<KeyManagerStatusReporter>,
    ks_info: Mutex<HashMap<String, KeySetInfo>>,
}

impl KeyManager {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Component,
        gate: Gate,
        metrics: Arc<KeyManagerMetrics>,
        status_reporter: Arc<KeyManagerStatusReporter>,
    ) -> Self {
        Self {
            component,
            gate,
            metrics,
            status_reporter,
            ks_info: Mutex::new(HashMap::new()),
        }
    }

    async fn run(
        self,
        mut dnst_keyset_bin_path: PathBuf,
        mut dnst_keyset_data_dir: PathBuf,
    ) -> Result<(), crate::comms::Terminated> {
        let component_name = self.component.name().clone();

        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.tick(dnst_keyset_bin_path.as_path(), dnst_keyset_data_dir.as_path()).await;
                }

                res = self.gate.process() => {
                    match res {
                        Err(Terminated) => {
                            self.status_reporter.terminated();
                            return Ok(());
                        }

                        Ok(status) => {
                            self.status_reporter.gate_status_announced(&status);
                            match status {
                                GateStatus::Reconfiguring {
                                    new_config: Unit::KeyManager(KeyManagerUnit {
                                        dnst_keyset_bin_path: new_dnst_keyset_bin_path,
                                        dnst_keyset_data_dir: new_dnst_keyset_data_dir
                                    }),
                                } => {
                                    // Runtime reconfiguration of this unit has been
                                    // requested.
                                    dnst_keyset_bin_path = new_dnst_keyset_bin_path;
                                    dnst_keyset_data_dir = new_dnst_keyset_data_dir;

                                    // Report that we have finished handling the reconfigure command
                                    self.status_reporter.reconfigured();
                                }

                                GateStatus::ReportLinks { report } => {
                                    report.declare_source();
                                    report.set_graph_status(self.metrics.clone());
                                }

                                GateStatus::ApplicationCommand { cmd } => {
                                    info!("[{component_name}]: Received command: {cmd:?}");
                                }

                                _ => { /* Nothing to do */ }
                            }
                        }
                    }
                }
            }
        }
    }

    async fn tick(&self, dnst_keyset_bin_path: &Path, dnst_keyset_data_dir: &Path) {
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
                self.gate
                    .update_data(Update::ResignZoneEvent {
                        zone_name: zone.apex_name().clone(),
                    })
                    .await;
                continue;
            }

            let Some(ref cron_next) = info.cron_next else {
                continue;
            };

            if *cron_next < UnixTime::now() {
                // Run cron
                let cfg_path = dnst_keyset_data_dir.join(format!("{apex_name}.cfg"));
                let mut args = vec!["keyset", "-c"];
                args.push(cfg_path.to_str().unwrap());
                args.push("cron");
                println!(
                    "Invoking keyset cron for zone {apex_name} with {}",
                    args.join(" ")
                );
                let Ok(res) = Command::new(dnst_keyset_bin_path).args(args).output() else {
                    error!(
                        "Failed to invoke keyset binary at '{}",
                        dnst_keyset_bin_path.display()
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
                        self.gate
                            .update_data(Update::ResignZoneEvent {
                                zone_name: zone.apex_name().clone(),
                            })
                            .await;
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

//------------ KeyManagerMetrics ---------------------------------------------

#[derive(Debug, Default)]
pub struct KeyManagerMetrics {
    gate: Option<Arc<GateMetrics>>, // optional to make testing easier
}

impl GraphStatus for KeyManagerMetrics {
    fn status_text(&self) -> String {
        "TODO".to_string()
    }

    fn okay(&self) -> Option<bool> {
        Some(false)
    }
}

impl KeyManagerMetrics {
    // const LISTENER_BOUND_COUNT_METRIC: Metric = Metric::new(
    //     "bmp_tcp_in_listener_bound_count",
    //     "the number of times the TCP listen port was bound to",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
}

impl KeyManagerMetrics {
    pub fn new(gate: &Gate) -> Self {
        Self {
            gate: Some(gate.metrics()),
        }
    }
}

impl metrics::Source for KeyManagerMetrics {
    fn append(&self, unit_name: &str, target: &mut metrics::Target) {
        if let Some(gate) = &self.gate {
            gate.append(unit_name, target);
        }

        // target.append_simple(
        //     &Self::LISTENER_BOUND_COUNT_METRIC,
        //     Some(unit_name),
        //     self.listener_bound_count.load(SeqCst),
        // );
    }
}

//------------ KeyManagerStatusReporter --------------------------------------

#[derive(Debug, Default)]
pub struct KeyManagerStatusReporter {
    name: String,
    metrics: Arc<KeyManagerMetrics>,
}

impl KeyManagerStatusReporter {
    pub fn new<T: Display>(name: T, metrics: Arc<KeyManagerMetrics>) -> Self {
        Self {
            name: format!("{}", name),
            metrics,
        }
    }

    pub fn _typed_metrics(&self) -> Arc<KeyManagerMetrics> {
        self.metrics.clone()
    }
}

impl UnitStatusReporter for KeyManagerStatusReporter {}

impl AnyStatusReporter for KeyManagerStatusReporter {
    fn metrics(&self) -> Option<Arc<dyn crate::metrics::Source>> {
        Some(self.metrics.clone())
    }
}

impl Chainable for KeyManagerStatusReporter {
    fn add_child<T: Display>(&self, child_name: T) -> Self {
        Self::new(self.link_names(child_name), self.metrics.clone())
    }
}

impl Named for KeyManagerStatusReporter {
    fn name(&self) -> &str {
        &self.name
    }
}

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
