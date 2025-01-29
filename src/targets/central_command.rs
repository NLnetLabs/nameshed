use std::fmt::{Debug, Display};
use std::{ops::ControlFlow, sync::Arc};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use log::info;
use non_empty_vec::NonEmpty;
use serde::Deserialize;
use serde_with::serde_as;
use tokio::sync::mpsc;

use crate::common::status_reporter::{AnyStatusReporter, Chainable, Named, TargetStatusReporter};
use crate::comms::{
    AnyDirectUpdate, ApplicationCommand, DirectLink, DirectUpdate, GraphStatus, Terminated,
};
use crate::manager::{Component, TargetCommand, WaitPoint};
use crate::metrics;
use crate::payload::Update;
use crate::targets::Target;

#[derive(Debug, Deserialize)]
pub struct CentralCommandTarget {
    /// The set of units to receive messages from.
    sources: NonEmpty<DirectLink>,

    #[serde(flatten)]
    config: Config,
}

#[cfg(test)]
impl From<Config> for CentralCommandTarget {
    fn from(config: Config) -> Self {
        let (_gate, mut gate_agent) = crate::comms::Gate::new(0);
        let link = gate_agent.create_link();
        let sources = NonEmpty::new(link.into());
        Self { sources, config }
    }
}

impl CentralCommandTarget {
    pub async fn run(
        self,
        component: Component,
        cmd: mpsc::Receiver<TargetCommand>,
        waitpoint: WaitPoint,
    ) -> Result<(), Terminated> {
        CentralCommand::new(self.config, component)
            .run(self.sources, cmd, waitpoint)
            .await
    }
}

pub(super) struct CentralCommand {
    component: Component,
    config: Arc<ArcSwap<Config>>,
    status_reporter: Arc<CentralCommandStatusReporter>,
}

impl CentralCommand {
    pub fn new(config: Config, mut component: Component) -> Self {
        let config = Arc::new(ArcSwap::from_pointee(config));

        let metrics = Arc::new(CentralCommandMetrics::new());
        component.register_metrics(metrics.clone());

        let status_reporter =
            Arc::new(CentralCommandStatusReporter::new(component.name(), metrics));

        Self {
            component,
            config,
            status_reporter,
        }
    }

    pub async fn run(
        mut self,
        mut sources: NonEmpty<DirectLink>,
        cmd_rx: mpsc::Receiver<TargetCommand>,
        waitpoint: WaitPoint,
    ) -> Result<(), Terminated> {
        let component = &mut self.component;
        let _unit_name = component.name().clone();

        let arc_self = Arc::new(self);

        // Register as a direct update receiver with the linked gates.
        for link in sources.iter_mut() {
            link.connect(arc_self.clone(), false).await.unwrap();
        }

        // Wait for other components to be, and signal to other components
        // that we are, ready to start. All units and targets start together,
        // otherwise data passed from one component to another may be lost if
        // the receiving component is not yet ready to accept it.
        waitpoint.running().await;

        arc_self.do_run(Some(sources), cmd_rx).await
    }

    pub async fn do_run(
        self: &Arc<Self>,
        mut sources: Option<NonEmpty<DirectLink>>,
        mut cmd_rx: mpsc::Receiver<TargetCommand>,
    ) -> Result<(), Terminated> {
        loop {
            if let Err(Terminated) = self.process_events(&mut sources, &mut cmd_rx).await {
                self.status_reporter.terminated();
                return Err(Terminated);
            }
        }
    }

    pub async fn process_events(
        self: &Arc<Self>,
        sources: &mut Option<NonEmpty<DirectLink>>,
        cmd_rx: &mut mpsc::Receiver<TargetCommand>,
    ) -> Result<(), Terminated> {
        loop {
            tokio::select! {
                // Disable tokio::select!() random branch selection
                biased;

                // If nothing happened above, check for new internal Rotonda
                // target commands to handle.
                cmd = cmd_rx.recv() => {
                    if let Some(cmd) = &cmd {
                        self.status_reporter.command_received(cmd);
                    }

                    match cmd {
                        Some(TargetCommand::Reconfigure { new_config: Target::CentraLCommand(new_config) }) => {
                            if self.reconfigure(sources, new_config).await.is_break() {
                                // NOOP
                            }
                        }

                        Some(TargetCommand::ReportLinks { report }) => {
                            if let Some(sources) = sources {
                                report.set_sources(sources);
                            }
                            report.set_graph_status(
                                self.status_reporter.metrics(),
                            );
                        }

                        None | Some(TargetCommand::Terminate) => {
                            return Err(Terminated);
                        }
                    }
                }
            }
        }
    }

    async fn reconfigure(
        self: &Arc<Self>,
        sources: &mut Option<NonEmpty<DirectLink>>,
        CentralCommandTarget {
            sources: new_sources,
            config: new_config,
        }: CentralCommandTarget,
    ) -> ControlFlow<()> {
        if let Some(sources) = sources {
            self.status_reporter
                .upstream_sources_changed(sources.len(), new_sources.len());

            *sources = new_sources;

            // Register as a direct update receiver with the new
            // set of linked gates.
            for link in sources.iter_mut() {
                link.connect(self.clone(), false).await.unwrap();
            }
        }

        // Store the changed configuration
        self.config.store(Arc::new(new_config));

        // Report that we have finished handling the reconfigure command
        self.status_reporter.reconfigured();

        // if reconnect {
        //     // Advise the caller to stop using the current MQTT client
        //     // and instead to re-create it using the new config
        //     ControlFlow::Break(())
        // } else {
        //     // Advise the caller to keep using the current MQTT client
        //     ControlFlow::Continue(())
        // }

        ControlFlow::Continue(())
    }
}

#[async_trait]
impl DirectUpdate for CentralCommand {
    async fn direct_update(&self, event: Update) {
        info!("[{}]: Event received: {event:?}", self.component.name());
        match event {
            Update::ZoneUpdatedEvent {
                zone_name,
                zone_serial,
            } => {
                info!(
                    "[{}]: Instructing review server to publish the updated zone",
                    self.component.name()
                );
                self.component
                    .send_command(
                        "RS".to_string(),
                        ApplicationCommand::PublishZone {
                            zone_name,
                            zone_serial,
                        },
                    )
                    .await;
            }

            Update::ZoneApprovedEvent {
                zone_name,
                zone_serial,
            } => {
                info!(
                    "[{}]: Instructing zone signer to sign the approved zone",
                    self.component.name()
                );
                self.component
                    .send_command(
                        "ZS".to_string(),
                        ApplicationCommand::SignZone {
                            zone_name,
                            zone_serial,
                        },
                    )
                    .await;
            }
        }
    }
}

impl AnyDirectUpdate for CentralCommand {}

impl std::fmt::Debug for CentralCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CentralCommand").finish()
    }
}

//------------ Config --------------------------------------------------------

#[serde_as]
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    // /// MQTT server host[:port] to publish to
    // pub destination: Destination,

    // #[serde(default = "Config::default_qos")]
    // pub qos: i32,

    // pub client_id: ClientId,

    // #[serde(default = "Config::default_topic_template")]
    // pub topic_template: String,

    // /// How long to wait in seconds before connecting again if the connection
    // /// is closed.
    // #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    // #[serde(default = "Config::default_connect_retry_secs")]
    // pub connect_retry_secs: Duration,

    // /// How long to wait before timing out an attempt to publish a message.
    // #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    // #[serde(default = "Config::default_publish_max_secs")]
    // pub publish_max_secs: Duration,

    // /// How many messages to buffer if publishing encounters delays
    // #[serde(default = "Config::default_queue_size")]
    // pub queue_size: u16,

    // #[serde(default)]
    // pub username: Option<String>,

    // #[serde(default)]
    // pub password: Option<String>,
}

impl Config {
    // /// The default MQTT quality of service setting to use
    // ///   0 - At most once delivery
    // ///   1 - At least once delivery
    // ///   2 - Exactly once delivery
    // pub fn default_qos() -> i32 {
    //     2
    // }

    // /// The default re-connect timeout in seconds.
    // pub fn default_connect_retry_secs() -> Duration {
    //     Duration::from_secs(60)
    // }

    // /// The default publish timeout in seconds.
    // pub fn default_publish_max_secs() -> Duration {
    //     Duration::from_secs(5)
    // }

    // /// The default MQTT topic prefix.
    // pub fn default_topic_template() -> String {
    //     "rotonda/{id}".to_string()
    // }

    // /// The default re-connect timeout in seconds.
    // pub fn default_queue_size() -> u16 {
    //     1000
    // }
}

//------------ CentralCommandStatusReporter ----------------------------------

#[derive(Debug, Default)]
pub struct CentralCommandStatusReporter {
    name: String,
    metrics: Arc<CentralCommandMetrics>,
}

impl CentralCommandStatusReporter {
    pub fn new<T: Display>(name: T, metrics: Arc<CentralCommandMetrics>) -> Self {
        Self {
            name: format!("{}", name),
            metrics,
        }
    }

    pub fn metrics(&self) -> Arc<CentralCommandMetrics> {
        self.metrics.clone()
    }
}

impl TargetStatusReporter for CentralCommandStatusReporter {}

impl AnyStatusReporter for CentralCommandStatusReporter {
    fn metrics(&self) -> Option<Arc<dyn crate::metrics::Source>> {
        Some(self.metrics.clone())
    }
}

impl Chainable for CentralCommandStatusReporter {
    fn add_child<T: Display>(&self, child_name: T) -> Self {
        Self::new(self.link_names(child_name), self.metrics.clone())
    }
}

impl Named for CentralCommandStatusReporter {
    fn name(&self) -> &str {
        &self.name
    }
}

//------------ CentralCommandMetrics -----------------------------------------

#[derive(Debug, Default)]
pub struct CentralCommandMetrics {
    // pub connection_established_state: AtomicBool,
    // pub connection_lost_count: AtomicUsize,
    // pub connection_error_count: AtomicUsize,
    // pub publish_error_count: AtomicUsize,
    // pub in_flight_count: AtomicU16,
    // // pub not_acknowledged_count: AtomicUsize,
    // topics: Arc<FrimMap<Arc<String>, Arc<TopicMetrics>>>,
}

impl GraphStatus for CentralCommandMetrics {
    fn status_text(&self) -> String {
        // match self.connection_established_state.load(SeqCst) {
        //     true => {
        //         format!(
        //             "in-flight: {}\npublished: {}\nerrors: {}",
        //             self.in_flight_count.load(SeqCst),
        //             self.topics.guard().iter().fold(0, |acc, v| acc
        //                 + v.1.publish_counts.load(SeqCst)),
        //             self.publish_error_count.load(SeqCst),
        //         )
        //     }
        //     false => "N/A".to_string(),
        // }
        "TODO".to_string()
    }

    fn okay(&self) -> Option<bool> {
        Some(false)
    }
}

impl CentralCommandMetrics {
    // const CONNECTION_ESTABLISHED_METRIC: Metric = Metric::new(
    //     "mqtt_target_connection_established",
    //     "the state of the connection to the MQTT broker: 0=down, 1=up",
    //     MetricType::Gauge,
    //     MetricUnit::State,
    // );
    // const CONNECTION_LOST_COUNT_METRIC: Metric = Metric::new(
    //     "mqtt_target_connection_lost_count",
    //     "the number of times the connection to the MQTT broker was lost",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
    // const CONNECTION_ERROR_COUNT_METRIC: Metric = Metric::new(
    //     "mqtt_target_connection_error_count",
    //     "the number of times an error occurred with the connection to the MQTT broker",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
    // const PUBLISH_COUNT_PER_TOPIC_METRIC: Metric = Metric::new(
    //     "mqtt_target_publish_count",
    //     "the number of messages requested for publication to the MQTT broker per topic",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
    // const PUBLISH_ERROR_COUNT_PER_TOPIC_METRIC: Metric = Metric::new(
    //     "mqtt_target_publish_error_count",
    //     "the number of messages that could not be queued for publication",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
    // const IN_FLIGHT_COUNT_PER_TOPIC_METRIC: Metric = Metric::new(
    //     "mqtt_target_in_flight_count",
    //     "the number of messages requested for publication but not yet sent to the MQTT broker per topic",
    //     MetricType::Gauge,
    //     MetricUnit::Total,
    // );
    // // The rumqttc library has this count internally but doesn't expose it to
    // // us. const PUBLISH_NOT_ACKNOWLEDGED_COUNT_METRIC: Metric = Metric::new(
    // //     "mqtt_target_published_not_acknowledged_count", "the number of QoS
    // //     1 or QoS 2 messages for which confirmation by the MQTT broker is
    // //     pending (QoS 1) or incomplete (QoS 2)", MetricType::Counter,
    // //     MetricUnit::Total, );
}

impl CentralCommandMetrics {
    pub fn new() -> Self {
        CentralCommandMetrics::default()
    }
}

impl metrics::Source for CentralCommandMetrics {
    fn append(&self, _unit_name: &str, _target: &mut metrics::Target) {
        // target.append_simple(
        //     &Self::CONNECTION_ESTABLISHED_METRIC,
        //     Some(unit_name),
        //     self.connection_established_state.load(SeqCst) as u8,
        // );
        // target.append_simple(
        //     &Self::CONNECTION_LOST_COUNT_METRIC,
        //     Some(unit_name),
        //     self.connection_lost_count.load(SeqCst),
        // );
        // target.append_simple(
        //     &Self::CONNECTION_ERROR_COUNT_METRIC,
        //     Some(unit_name),
        //     self.connection_error_count.load(SeqCst),
        // );
        // target.append_simple(
        //     &Self::IN_FLIGHT_COUNT_PER_TOPIC_METRIC,
        //     Some(unit_name),
        //     self.in_flight_count.load(SeqCst),
        // );
        // target.append_simple(
        //     &Self::PUBLISH_ERROR_COUNT_PER_TOPIC_METRIC,
        //     Some(unit_name),
        //     self.publish_error_count.load(SeqCst),
        // );
        // // target.append_simple(
        // //     &Self::PUBLISH_NOT_ACKNOWLEDGED_COUNT_METRIC,
        // //     Some(unit_name),
        // //     self.not_acknowledged_count.load(SeqCst),
        // // );
        // for (topic, metrics) in self.topics.guard().iter() {
        //     let topic = topic.as_str();
        //     append_labelled_metric(
        //         unit_name,
        //         target,
        //         "topic",
        //         topic,
        //         Self::PUBLISH_COUNT_PER_TOPIC_METRIC,
        //         metrics.publish_counts.load(SeqCst),
        //     );
        // }
    }
}
