use std::fmt::{Debug, Display};
use std::{ops::ControlFlow, sync::Arc};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use indoc::formatdoc;
use log::info;
use non_empty_vec::NonEmpty;
use serde::Deserialize;
use serde_with::serde_as;
use tokio::sync::mpsc;

use crate::common::status_reporter::{AnyStatusReporter, Chainable, Named, TargetStatusReporter};
use crate::comms::{
    AnyDirectUpdate, ApplicationCommand, DirectLink, DirectUpdate, GraphStatus, Terminated,
};
use crate::http::{PercentDecodedPath, ProcessRequest};
use crate::manager::{Component, TargetCommand, WaitPoint};
use crate::metrics;
use crate::payload::Update;
use crate::targets::Target;

#[derive(Debug)]
pub struct CentralCommandTarget {
    /// The set of units to receive messages from.
    pub sources: NonEmpty<DirectLink>,

    pub config: Config,
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
    http_processor: Arc<CentralCommandApi>,
}

impl CentralCommand {
    pub fn new(config: Config, mut component: Component) -> Self {
        let config = Arc::new(ArcSwap::from_pointee(config));

        let metrics = Arc::new(CentralCommandMetrics::new());
        component.register_metrics(metrics.clone());

        let http_processor = Arc::new(CentralCommandApi::new());
        component.register_http_resource(http_processor.clone(), "/");

        let status_reporter =
            Arc::new(CentralCommandStatusReporter::new(component.name(), metrics));

        Self {
            component,
            config,
            status_reporter,
            http_processor,
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
            link.connect(arc_self.clone(), false)
                .await
                .map_err(|_| Terminated)?;
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
        let (msg, target, cmd) = match event {
            Update::UnsignedZoneUpdatedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing review server to publish the unsigned zone",
                "RS",
                ApplicationCommand::SeekApprovalForUnsignedZone {
                    zone_name,
                    zone_serial,
                },
            ),

            Update::UnsignedZoneApprovedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing zone signer to sign the approved zone",
                "ZS",
                ApplicationCommand::SignZone {
                    zone_name,
                    zone_serial,
                },
            ),

            Update::ZoneSignedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing review server to publish the signed zone",
                "RS2",
                ApplicationCommand::SeekApprovalForSignedZone {
                    zone_name,
                    zone_serial,
                },
            ),

            Update::SignedZoneApprovedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing publication server to publish the signed zone",
                "PS",
                ApplicationCommand::PublishSignedZone {
                    zone_name,
                    zone_serial,
                },
            ),
        };

        info!("[{}]: {msg}", self.component.name());
        self.component.send_command(target, cmd).await;
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
pub struct Config {}

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
pub struct CentralCommandMetrics {}

impl GraphStatus for CentralCommandMetrics {
    fn status_text(&self) -> String {
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
    }
}

//------------ CentralCommandApi ---------------------------------------------

struct CentralCommandApi;

impl CentralCommandApi {
    fn new() -> Self {
        Self
    }
}

// API: GET /<http api path>/<zone name>/<zone serial>/{approve,reject}/<approval token>
//
// TODO: Should we expire old pending approvals, e.g. a hook script failed and
// they never got approved or rejected?
#[async_trait]
impl ProcessRequest for CentralCommandApi {
    async fn process_request(
        &self,
        request: &hyper::Request<hyper::Body>,
    ) -> Option<hyper::Response<hyper::Body>> {
        let req_path = request.uri().decoded_path();
        if request.method() == hyper::Method::GET && req_path == "/" {
            Some(self.build_status_response().await)
        } else {
            None
        }
    }
}

impl CentralCommandApi {
    pub async fn build_status_response(&self) -> hyper::Response<hyper::Body> {
        let mut response_body = self.build_response_header().await;

        self.build_status_response_body(&mut response_body).await;

        self.build_response_footer(&mut response_body);

        hyper::Response::builder()
            .header("Content-Type", "text/html")
            .body(hyper::Body::from(response_body))
            .unwrap()
    }

    async fn build_response_header(&self) -> String {
        formatdoc! {
            r#"
            <!DOCTYPE html>
            <html lang="en">
                <head>
                  <meta charset="UTF-8">
                </head>
                <body>
                <h1>Nameshed</h1>
            "#,
        }
    }

    async fn build_status_response_body(&self, response_body: &mut String) {
        let body = formatdoc! {
            r#"
            <ul>
                <li>Zone Loader [ZL]: <a href="/zl/">view</a></li>
                <li>Unsigned Zone Preview Server [RS]: <a href="/rs/">view</a></li>
                <li>Zone Signer [ZS]: <a href="/zs/">view</a></li>
                <li>Signed Zone Preview Server [RS2]: <a href="/rs2/">view</a></li>
                <li>Public Server [PS]: <a href="/ps/">view</a></li>
            </ul>
            "#,
        };

        response_body.push_str(&body);
    }

    fn build_response_footer(&self, response_body: &mut String) {
        response_body.push_str("  </body>\n");
        response_body.push_str("</html>\n");
    }
}
