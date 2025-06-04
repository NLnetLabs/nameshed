use super::Unit;
use crate::common::status_reporter::{AnyStatusReporter, Chainable, Named, UnitStatusReporter};
use crate::comms::{Gate, GateMetrics, GateStatus, GraphStatus, Terminated};
use crate::manager::{Component, WaitPoint};
use crate::metrics;
use crate::payload::Update;
use core::fmt::Display;
use core::time::Duration;
use domain::zonetree::Zone;
use log::{error, info};
use serde::Deserialize;
use serde_with::serde_as;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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
                    eprintln!("TICK");
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
                                    match &cmd {
                                        _ => { /* Not for us */ }
                                    }
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
        for zone in zone_tree.load().iter_zones() {
            let apex_name = zone.apex_name().to_string();
            let cfg_path = dnst_keyset_data_dir.join(format!("{apex_name}.cfg"));
            let state_path = dnst_keyset_data_dir.join(format!("{apex_name}.state"));
            let mut args = vec!["keyset", "-c"];
            args.push(cfg_path.to_str().unwrap());
            args.push("-s");
            args.push(state_path.to_str().unwrap());
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
                continue;
            };
            if res.status.success() {
                println!("CRON OUT: {}", String::from_utf8_lossy(&res.stdout));
                // TOOD: Check the cron output and decide if a resign event should be sent.
                // if ... {
                //     self.gate
                //         .update_data(Update::ResignZoneEvent {
                //             zone_name: zone.apex_name().clone(),
                //         })
                //         .await;
                // }
            } else {
                println!("CRON ERR: {}", String::from_utf8_lossy(&res.stderr));
            }
        }
    }
}

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
