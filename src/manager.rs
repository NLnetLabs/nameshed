//! Controlling the entire operation.

use arc_swap::ArcSwap;
use futures::future::{join_all, select, Either};
use log::{debug, error, info};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Barrier;
use tracing::warn;
use uuid::Uuid;

use crate::common::file_io::TheFileIo;
use crate::common::tsig::TsigKeyStore;
use crate::comms::{ApplicationCommand, DirectLink, Gate, GateAgent, Link};
use crate::targets::central_command::{self, CentralCommandTarget};
use crate::targets::Target;
use crate::units::zone_loader::ZoneLoaderUnit;
use crate::units::zone_server::{self, ZoneServerUnit};
use crate::units::zone_signer::{TomlDenialConfig, ZoneSignerUnit};
use crate::units::Unit;
use crate::{http, metrics};
use domain::zonetree::ZoneTree;

//------------ Component -----------------------------------------------------

/// Facilities available to all components.
///
/// Upon being started, every component receives one of these. It provides
/// access to information and services available to all components.
pub struct Component {
    /// The component’s name.
    name: Arc<str>,

    /// The component's type name.
    type_name: &'static str,

    /// An HTTP client.
    http_client: Option<HttpClient>,

    /// A reference to the metrics collection.
    metrics: Option<metrics::Collection>,

    /// A reference to the HTTP resources collection.
    http_resources: http::Resources,

    // /// A reference to the compiled Roto script.
    // roto_compiled: Option<Arc<CompiledRoto>>,
    /// A reference to the unsigned zones.
    unsigned_zones: Arc<ArcSwap<ZoneTree>>,

    /// A reference to the signed zones.
    signed_zones: Arc<ArcSwap<ZoneTree>>,

    /// A reference to the publshed zones.
    published_zones: Arc<ArcSwap<ZoneTree>>,

    /// A reference to the TsigKeyStore.
    tsig_key_store: TsigKeyStore,

    /// A sender for requesting the Manager to forward an application command
    /// to a unit by name.
    app_cmd_tx: Sender<(String, ApplicationCommand)>,
}

#[cfg(test)]
impl Default for Component {
    fn default() -> Self {
        Self {
            name: "MOCK".into(),
            type_name: "MOCK",
            http_client: Default::default(),
            metrics: Default::default(),
            http_resources: Default::default(),
            // roto_compiled: Default::default(),
            unsigned_zones: Default::default(),
            signed_zones: Default::default(),
            published_zones: Default::default(),
            tsig_key_store: Default::default(),
            app_cmd_tx: tokio::sync::mpsc::channel(0).0,
        }
    }
}

impl Component {
    /// Creates a new component from its, well, components.
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: String,
        type_name: &'static str,
        http_client: HttpClient,
        metrics: metrics::Collection,
        http_resources: http::Resources,
        // roto_compiled: Option<Arc<CompiledRoto>>,
        unsigned_zones: Arc<ArcSwap<ZoneTree>>,
        signed_zones: Arc<ArcSwap<ZoneTree>>,
        published_zones: Arc<ArcSwap<ZoneTree>>,
        tsig_key_store: TsigKeyStore,
        app_cmd_tx: Sender<(String, ApplicationCommand)>,
    ) -> Self {
        Component {
            name: name.into(),
            type_name,
            http_client: Some(http_client),
            metrics: Some(metrics),
            http_resources,
            // roto_compiled,
            unsigned_zones,
            signed_zones,
            published_zones,
            tsig_key_store,
            app_cmd_tx,
        }
    }

    /// Returns the name of the component.
    pub fn name(&self) -> &Arc<str> {
        &self.name
    }

    /// Returns the type name of the component.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// Returns a reference to an HTTP Client.
    pub fn http_client(&self) -> &HttpClient {
        self.http_client.as_ref().unwrap()
    }

    pub fn http_resources(&self) -> &http::Resources {
        &self.http_resources
    }

    // pub fn roto_compiled(&self) -> &Option<Arc<CompiledRoto>> {
    //     &self.roto_compiled
    // }

    pub fn unsigned_zones(&self) -> &Arc<ArcSwap<ZoneTree>> {
        &self.unsigned_zones
    }

    pub fn signed_zones(&self) -> &Arc<ArcSwap<ZoneTree>> {
        &self.signed_zones
    }

    pub fn published_zones(&self) -> &Arc<ArcSwap<ZoneTree>> {
        &self.published_zones
    }

    pub fn tsig_key_store(&self) -> &TsigKeyStore {
        &self.tsig_key_store
    }

    /// Register a metrics source.
    pub fn register_metrics(&mut self, source: Arc<dyn metrics::Source>) {
        if let Some(metrics) = &self.metrics {
            metrics.register(self.name.clone(), Arc::downgrade(&source));
        }
    }

    /// Register an HTTP resource.
    pub fn register_http_resource(
        &mut self,
        process: Arc<dyn http::ProcessRequest>,
        rel_base_url: &str,
    ) {
        debug!("registering resource {:?}", &rel_base_url);
        self.http_resources.register(
            Arc::downgrade(&process),
            self.name.clone(),
            self.type_name,
            rel_base_url,
            false,
        )
    }

    /// Register a sub HTTP resource.
    pub fn register_sub_http_resource(
        &mut self,
        process: Arc<dyn http::ProcessRequest>,
        rel_base_url: &str,
    ) {
        debug!("registering resource {:?}", &rel_base_url);
        self.http_resources.register(
            Arc::downgrade(&process),
            self.name.clone(),
            self.type_name,
            rel_base_url,
            true,
        )
    }

    pub async fn send_command(&self, target_unit_name: &str, data: ApplicationCommand) {
        self.app_cmd_tx
            .send((target_unit_name.to_string(), data))
            .await
            .unwrap()
    }
}

//------------ Manager -------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum LinkType {
    Queued,
    Direct,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct LinkInfo {
    link_type: LinkType,
    id: Uuid,
    gate_id: Uuid,
    connected_gate_slot: Option<Uuid>,
}

impl From<&Link> for LinkInfo {
    fn from(link: &Link) -> Self {
        Self {
            link_type: LinkType::Queued,
            id: link.id(),
            gate_id: link.gate_id(),
            connected_gate_slot: link.connected_gate_slot(),
        }
    }
}

impl From<&DirectLink> for LinkInfo {
    fn from(link: &DirectLink) -> Self {
        Self {
            link_type: LinkType::Direct,
            id: link.id(),
            gate_id: link.gate_id(),
            connected_gate_slot: link.connected_gate_slot(),
        }
    }
}

#[allow(clippy::large_enum_variant)]
pub enum TargetCommand {
    Reconfigure { new_config: Target },

    Terminate,
}

impl Display for TargetCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetCommand::Reconfigure { .. } => f.write_str("Reconfigure"),
            TargetCommand::Terminate => f.write_str("Terminate"),
        }
    }
}

/// A manager for components and auxiliary services.
///
/// Requires a running Tokio reactor that has been "entered" (see Tokio
/// `Handle::enter()`).
pub struct Manager {
    /// The gate agent for the zone loader.
    loader_agent: Option<GateAgent>,

    /// The gate agent for the review server.
    review_agent: Option<GateAgent>,

    /// The gate agent for the zone signer.
    signer_agent: Option<GateAgent>,

    /// The gate agent for the secondary review server.
    review2_agent: Option<GateAgent>,

    /// The gate agent for the publish server.
    publish_agent: Option<GateAgent>,

    /// Commands for the central command.
    center_tx: Option<mpsc::Sender<TargetCommand>>,

    /// An HTTP client.
    http_client: HttpClient,

    /// The metrics collection maintained by this manager.
    metrics: metrics::Collection,

    /// The HTTP resources collection maintained by this manager.
    http_resources: http::Resources,

    // /// A reference to the compiled Roto script.
    // roto_compiled: Option<Arc<CompiledRoto>>,
    #[allow(dead_code)]
    file_io: TheFileIo,

    tsig_key_store: TsigKeyStore,

    unsigned_zones: Arc<ArcSwap<ZoneTree>>,

    signed_zones: Arc<ArcSwap<ZoneTree>>,

    published_zones: Arc<ArcSwap<ZoneTree>>,

    app_cmd_tx: Sender<(String, ApplicationCommand)>,

    app_cmd_rx: Arc<tokio::sync::Mutex<Receiver<(String, ApplicationCommand)>>>,
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Manager {
    /// Creates a new manager.
    pub fn new() -> Self {
        let (app_cmd_tx, app_cmd_rx) = tokio::sync::mpsc::channel(10);

        let tsig_key_store = Default::default();
        let unsigned_zones = Default::default();
        let signed_zones = Default::default();
        let published_zones = Default::default();

        #[allow(clippy::let_and_return, clippy::default_constructed_unit_structs)]
        let manager = Manager {
            loader_agent: None,
            review_agent: None,
            signer_agent: None,
            review2_agent: None,
            publish_agent: None,
            center_tx: None,
            http_client: Default::default(),
            metrics: Default::default(),
            http_resources: Default::default(),
            // roto_compiled: Default::default(),
            file_io: TheFileIo::default(),
            unsigned_zones,
            signed_zones,
            published_zones,
            tsig_key_store,
            app_cmd_tx,
            app_cmd_rx: Arc::new(tokio::sync::Mutex::new(app_cmd_rx)),
        };

        manager
    }

    #[cfg(test)]
    pub fn set_file_io(&mut self, file_io: TheFileIo) {
        self.file_io = file_io;
    }

    pub async fn accept_application_commands(&self) {
        while let Some((unit_name, data)) = self.app_cmd_rx.lock().await.recv().await {
            let Some(agent) = (match &*unit_name {
                "ZL" => self.loader_agent.as_ref(),
                "RS" => self.review_agent.as_ref(),
                "ZS" => self.signer_agent.as_ref(),
                "RS2" => self.review2_agent.as_ref(),
                "PS" => self.publish_agent.as_ref(),
                _ => None,
            }) else {
                continue;
            };
            debug!("Forwarding application command to unit '{unit_name}'");
            agent.send_application_command(data).await.unwrap();
        }
    }

    // pub fn compile_roto_script(
    //     &mut self,
    //     roto_scripts_path: &Option<std::path::PathBuf>,
    // ) -> Result<(), String> {
    //     let path = if let Some(p) = roto_scripts_path {
    //         p
    //     } else {
    //         info!("no roto scripts path to load filters from");
    //         return Ok(());
    //     };

    //     let i = roto::read_files([path.to_string_lossy()]).map_err(|e| e.to_string())?;
    //     let c = i
    //         .compile(create_runtime().unwrap(), usize::BITS / 8)
    //         .map_err(|e| e.to_string())?;

    //     self.roto_compiled = Some(Arc::new(Mutex::new(c)));
    //     Ok(())
    // }

    /// Spawns all units and targets in the config into the given runtime.
    ///
    /// # Panics
    ///
    /// The method panics if the config hasn’t been successfully prepared via
    /// the same manager earlier.
    ///
    /// # Hot reloading
    ///
    /// Running units and targets that do not exist in the config by the same
    /// name, or exist by the same name but have a different type, will be
    /// terminated.
    ///
    /// Running units and targets with the same name and type as in the config
    /// will be signalled to reconfigure themselves per the new config (even
    /// if unchanged, it is the responsibility of the unit/target to decide
    /// how to react to the "new" config).
    ///
    /// Units receive the reconfigure signal via their gate. The gate will
    /// automatically update itself and its clones to use the new set of
    /// Senders that correspond to changes in the set of downstream units and
    /// targets that link to the unit.
    ///
    /// Units and targets receive changes to their set of links (if any) as
    /// part of the "new" config payload of the reconfigure signal. It is the
    /// responsibility of the unit/target to switch from the old links to the
    /// new links and, if desired, to drain old link queues before ceasing to
    /// query them further.
    pub fn spawn(&mut self) {
        self.spawn_internal(
            Self::spawn_unit,
            Self::spawn_target,
            Self::reconfigure_unit,
            Self::reconfigure_target,
            Self::terminate_unit,
            Self::terminate_target,
        )
    }

    /// Separated out from [spawn](Self::spawn) for testing purposes.
    ///
    /// Pass the new unit to the existing unit to reconfigure itself. If the
    /// set of downstream units and targets that refer to the unit being
    /// reconfigured have changed, we need to ensure that the gates and links
    /// in use correspond to the newly configured topology. For example:
    ///
    /// Before:
    ///
    /// ```text
    ///     unit                                    target
    ///     ┌───┐                                   ┌───┐
    ///     │ a │gate◀─────────────────────────link│ c │
    ///     └───┘                                   └───┘
    /// ```
    /// After:
    ///
    /// ```text
    ///     running:
    ///     --------
    ///     unit                                    target
    ///     ┌───┐                                   ┌───┐
    ///     │ a │gate◀─────────────────────────link│ c │
    ///     └───┘                                   └───┘
    ///
    ///     pending:
    ///     --------
    ///     unit                unit                target
    ///     ┌───┐               ┌───┐               ┌───┐
    ///     │ a'│gate'◀───link'│ b'│gate'◀───link'│ c'│
    ///     └───┘               └───┘               └───┘
    /// ```
    /// In this example unit a and target c still exist in the config file,
    /// possibly with changed settings, and new unit b has been added. The
    /// pending gates and links for new units that correspond to existing
    /// units are NOT the same links and gates, hence they have been marked in
    /// the diagram with ' to distinguish them.
    ///
    /// At this point we haven't started unit b' yet so what we actually have
    /// is:
    ///
    /// ```text
    ///     current:
    ///     --------
    ///     unit                                    target
    ///     ┌───┐                                   ┌───┐
    ///     │ a │gate◀─────────────────────────link│ c │
    ///     └───┘                                   └───┘
    ///
    ///     unit                 unit               target
    ///     ┌───┐               ┌───┐               ┌───┐
    ///     │ a'│gate◀╴╴╴╴link'│ b'│gate'◀╴╴╴link'│ c'│
    ///     └───┘               └───┘               └───┘
    /// ```
    /// Versus:
    ///
    /// ```text
    ///     desired:
    ///     --------
    ///     unit                unit                target
    ///     ┌───┐               ┌───┐               ┌───┐
    ///     │ a │gate◀────link'│ b'│gate'◀───link'│ c │
    ///     └───┘               └───┘               └───┘
    /// ```
    /// If we blindly replace unit a with a' and target c with c' we risk
    /// breaking existing connections or discarding state unnecessarily. So
    /// instead we want the existing units and targets to decide for
    /// themselves what needs to be done to adjust to the configuration
    /// changes.
    ///
    /// If we weren't reconfiguring unit a we wouldn't have a problem, the new
    /// a' would correctly establish a link with the new b'. So it's unit a
    /// that we have to fix.
    ///
    /// Unit a has a gate with a data update Sender that corresponds with the
    /// Receiver of the link held by target c. The gate of unit a may actually
    /// have been cloned and thus there may be multiple Senders corresponding
    /// to target c. The gate of unit a may also be linked to other links held
    /// by other units and targets than in our simple example. We need to drop
    /// the wrong data update Senders and replace them with new ones referring
    /// to unit b' (well, at this point we don't know that it's unit b' we're
    /// talking about, just that we want gate a to have the same update
    /// Senders as gate a' (as all of the newly created gates and links have
    /// the desired topological setup).
    ///
    /// Note that the only way we have of influencing the data update senders
    /// of clones to match changes we make to the update sender of the
    /// original gate that was cloned is to send commands to the cloned gates.
    ///
    /// So, to reconfigure units we send their gate a Reconfigure command
    /// containing the new configuration. Note that this includes any newly
    /// constructed Links so the unit can .close() its current link(s) to
    /// prevent new incoming messages, process any messages still queued in
    /// the link, then when Link::query() returns UnitStatus::Gone because the
    /// queue is empty and the receiver is closed, we can at that point switch
    /// to using the new links. This is done inside each unit.
    ///
    /// The Reconfiguring GateStatus update will be propagated by the gate to
    /// its clones, if any. This allows spawned tasks each handling a client
    /// connection, e.g. from different routers, to each handle any
    /// reconfiguration required in the best way.
    ///
    /// For Targets we do something similar but as they don't have a Gate we
    /// pass a new MPSC Channel receiver to them and hold the corresponding
    /// sender here.
    ///
    /// Finally, unit and/or target configurations that have been commented
    /// out but for which a unit/target was already running, require that we
    /// detect the missing config and send a Terminate command to the orphaned
    /// unit/target.
    #[allow(clippy::too_many_arguments)]
    fn spawn_internal<SpawnUnit, SpawnTarget, ReconfUnit, ReconfTarget, TermUnit, TermTarget>(
        &mut self,
        spawn_unit: SpawnUnit,
        spawn_target: SpawnTarget,
        reconfigure_unit: ReconfUnit,
        reconfigure_target: ReconfTarget,
        terminate_unit: TermUnit,
        terminate_target: TermTarget,
    ) where
        SpawnUnit: Fn(Component, Unit, Gate, WaitPoint),
        SpawnTarget: Fn(Component, Target, Receiver<TargetCommand>, WaitPoint),
        ReconfUnit: Fn(&str, GateAgent, Unit, Gate),
        ReconfTarget: Fn(&str, Sender<TargetCommand>, Target),
        TermUnit: Fn(&str, Arc<GateAgent>),
        TermTarget: Fn(&str, Arc<Sender<TargetCommand>>),
    {
        let num_targets = 1;
        let num_units = 5;
        let coordinator = Coordinator::new(num_targets + num_units);

        // Forward-declare gates to the various components.
        let mut zl = LoadUnit::new(8);
        let mut rs = LoadUnit::new(8);
        let mut zs = LoadUnit::new(8);
        let mut rs2 = LoadUnit::new(8);
        let mut ps = LoadUnit::new(8);

        {
            let name = String::from("CC");
            let new_target = Target::CentraLCommand(CentralCommandTarget {
                sources: vec![
                    zl.agent.create_link().into(),
                    rs.agent.create_link().into(),
                    zs.agent.create_link().into(),
                    rs2.agent.create_link().into(),
                    ps.agent.create_link().into(),
                ]
                .try_into()
                .unwrap(),
                config: central_command::Config {},
            });

            // Spawn the new target
            let component = Component::new(
                name.clone(),
                new_target.type_name(),
                self.http_client.clone(),
                self.metrics.clone(),
                self.http_resources.clone(),
                // self.roto_compiled.clone(),
                self.unsigned_zones.clone(),
                self.signed_zones.clone(),
                self.published_zones.clone(),
                self.tsig_key_store.clone(),
                self.app_cmd_tx.clone(),
            );

            let (cmd_tx, cmd_rx) = mpsc::channel(100);
            spawn_target(
                component,
                new_target,
                cmd_rx,
                coordinator.clone().track(name.clone()),
            );
            self.center_tx = Some(cmd_tx);
        }

        let units = [
            (
                String::from("ZL"),
                Unit::ZoneLoader(ZoneLoaderUnit {
                    http_api_path: Arc::new(String::from("/zl/")),
                    listen: vec![
                        "tcp:127.0.0.1:8054".parse().unwrap(),
                        "udp:127.0.0.1:8054".parse().unwrap(),
                    ],
                    zones: Arc::new(HashMap::from([("example.com".into(), "".into())])),
                    xfr_in: Arc::new(HashMap::from([(
                        "example.com".into(),
                        "127.0.0.1:8055 KEY sec1-key".into(),
                    )])),
                    tsig_keys: HashMap::from([(
                        "sec1-key".into(),
                        "hmac-sha256:zlCZbVJPIhobIs1gJNQfrsS3xCxxsR9pMUrGwG8OgG8=".into(),
                    )]),
                }),
                zl.gate.unwrap(),
                zl.agent,
                &mut self.loader_agent,
            ),
            (
                String::from("RS"),
                Unit::ZoneServer(ZoneServerUnit {
                    http_api_path: Arc::new(String::from("/rs/")),
                    listen: vec![
                        "tcp:127.0.0.1:8056".parse().unwrap(),
                        "udp:127.0.0.1:8056".parse().unwrap(),
                    ],
                    xfr_out: HashMap::from([(
                        "example.com".into(),
                        "127.0.0.1:8055 KEY sec1-key".into(),
                    )]),
                    hooks: vec![String::from("/tmp/approve_or_deny.sh")],
                    mode: zone_server::Mode::Prepublish,
                    source: zone_server::Source::UnsignedZones,
                }),
                rs.gate.unwrap(),
                rs.agent,
                &mut self.review_agent,
            ),
            (
                String::from("ZS"),
                Unit::ZoneSigner(ZoneSignerUnit {
                    http_api_path: Arc::new(String::from("/zs/")),
                    keys_path: "/tmp/keys".into(),
                    treat_single_keys_as_csks: true,
                    max_concurrent_operations: 1,
                    max_concurrent_rrsig_generation_tasks: 8,
                    use_lightweight_zone_tree: false,
                    denial_config: TomlDenialConfig::default(),
                    rrsig_inception_offset_secs: 60 * 90,
                    rrsig_expiration_offset_secs: 60 * 60 * 24 * 14,
                }),
                zs.gate.unwrap(),
                zs.agent,
                &mut self.signer_agent,
            ),
            (
                String::from("RS2"),
                Unit::ZoneServer(ZoneServerUnit {
                    http_api_path: Arc::new(String::from("/rs2/")),
                    listen: vec![
                        "tcp:127.0.0.1:8057".parse().unwrap(),
                        "udp:127.0.0.1:8057".parse().unwrap(),
                    ],
                    xfr_out: HashMap::from([(
                        "example.com".into(),
                        "127.0.0.1:8055 KEY sec1-key".into(),
                    )]),
                    hooks: vec![String::from("/tmp/approve_or_deny_signed.sh")],
                    mode: zone_server::Mode::Prepublish,
                    source: zone_server::Source::SignedZones,
                }),
                rs2.gate.unwrap(),
                rs2.agent,
                &mut self.review2_agent,
            ),
            (
                String::from("PS"),
                Unit::ZoneServer(ZoneServerUnit {
                    http_api_path: Arc::new(String::from("/ps/")),
                    listen: vec![
                        "tcp:127.0.0.1:8058".parse().unwrap(),
                        "udp:127.0.0.1:8058".parse().unwrap(),
                    ],
                    xfr_out: HashMap::from([("example.com".into(), "127.0.0.1:8055".into())]),
                    hooks: vec![],
                    mode: zone_server::Mode::Publish,
                    source: zone_server::Source::PublishedZones,
                }),
                ps.gate.unwrap(),
                ps.agent,
                &mut self.publish_agent,
            ),
        ];

        // Spawn, reconfigure and terminate units
        for (name, new_unit, mut new_gate, new_agent, agent_out) in units {
            new_gate.set_name(&name);

            // Spawn the new unit
            let component = Component::new(
                name.clone(),
                new_unit.type_name(),
                self.http_client.clone(),
                self.metrics.clone(),
                self.http_resources.clone(),
                // self.roto_compiled.clone(),
                self.unsigned_zones.clone(),
                self.signed_zones.clone(),
                self.published_zones.clone(),
                self.tsig_key_store.clone(),
                self.app_cmd_tx.clone(),
            );

            let unit_type = std::mem::discriminant(&new_unit);
            spawn_unit(
                component,
                new_unit,
                new_gate,
                coordinator.clone().track(name.clone()),
            );
            *agent_out = Some(new_agent);
        }

        tokio::spawn(async move {
            coordinator
                .wait(|pending_component_names, status| {
                    warn!(
                        "Components {} are taking a long time to become {}.",
                        pending_component_names.join(", "),
                        status
                    );
                })
                .await;
        });
    }

    pub fn terminate(&mut self) {
        let units = [
            ("ZL", self.loader_agent.take().unwrap()),
            ("RS", self.review_agent.take().unwrap()),
            ("ZS", self.signer_agent.take().unwrap()),
            ("RS2", self.review2_agent.take().unwrap()),
            ("PS", self.publish_agent.take().unwrap()),
        ];
        for (name, agent) in units {
            let agent = Arc::new(agent);
            Self::terminate_unit(name, agent.clone());
            while !agent.is_terminated() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        {
            let cmd_tx = Arc::new(self.center_tx.take().unwrap());
            Self::terminate_target("CC", cmd_tx.clone());
            while !cmd_tx.is_closed() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn spawn_unit(component: Component, new_unit: Unit, new_gate: Gate, waitpoint: WaitPoint) {
        info!("Starting unit '{}'", component.name);
        tokio::spawn(new_unit.run(component, new_gate, waitpoint));
    }

    fn spawn_target(
        component: Component,
        new_target: Target,
        cmd_rx: Receiver<TargetCommand>,
        waitpoint: WaitPoint,
    ) {
        info!("Starting target '{}'", component.name);
        tokio::spawn(new_target.run(component, cmd_rx, waitpoint));
    }

    fn reconfigure_unit(name: &str, agent: GateAgent, new_config: Unit, new_gate: Gate) {
        info!("Reconfiguring unit '{}'", name);
        let name = name.to_owned();
        tokio::spawn(async move {
            if let Err(err) = agent.reconfigure(new_config, new_gate).await {
                error!(
                    "Internal error: reconfigure command could not be sent to unit '{}': {}",
                    name, err
                );
            }
        });
    }

    fn reconfigure_target(name: &str, sender: Sender<TargetCommand>, new_config: Target) {
        info!("Reconfiguring target '{}'", name);
        let name = name.to_owned();
        tokio::spawn(async move {
            if let Err(err) = sender.send(TargetCommand::Reconfigure { new_config }).await {
                error!(
                    "Internal error: reconfigure command could not be sent to target '{}': {}",
                    name, err
                );
            }
        });
    }

    fn terminate_unit(name: &str, agent: Arc<GateAgent>) {
        info!("Stopping unit '{}'", name);
        tokio::spawn(async move {
            agent.terminate().await;
        });
    }

    fn terminate_target(name: &str, sender: Arc<Sender<TargetCommand>>) {
        info!("Stopping target '{}'", name);
        tokio::spawn(async move {
            let _ = sender.send(TargetCommand::Terminate).await;
        });
    }

    /// Returns a new reference to the manager’s metrics collection.
    pub fn metrics(&self) -> metrics::Collection {
        self.metrics.clone()
    }

    /// Returns a new reference the the HTTP resources collection.
    pub fn http_resources(&self) -> http::Resources {
        self.http_resources.clone()
    }
}

//------------ Checkpoint ----------------------------------------------------

pub struct WaitPoint {
    coordinator: Arc<Coordinator>,
    name: String,
    ready: bool,
}

impl WaitPoint {
    pub fn new(coordinator: Arc<Coordinator>, name: String) -> Self {
        Self {
            coordinator,
            name,
            ready: false,
        }
    }

    pub async fn ready(&mut self) {
        self.coordinator.clone().ready(&self.name).await;
        self.ready = true;
    }

    pub async fn running(mut self) {
        // Targets don't need to signal ready & running separately so they
        // just invoke this fn, but we still need to make sure that the
        // barrier is reached twice otherwise the client of the Coordinator
        // will be left waiting forever.
        if !self.ready {
            self.ready().await;
        }
        self.coordinator.ready(&self.name).await
    }
}

pub struct Coordinator {
    barrier: Barrier,
    max_components: usize,
    pending: Arc<RwLock<HashSet<String>>>,
}

impl Coordinator {
    pub const SLOW_COMPONENT_ALARM_DURATION: Duration = Duration::from_secs(60);

    pub fn new(max_components: usize) -> Arc<Self> {
        let barrier = Barrier::new(max_components + 1);
        let pending = Arc::new(RwLock::new(HashSet::new()));
        Arc::new(Self {
            barrier,
            max_components,
            pending,
        })
    }

    pub fn track(self: Arc<Self>, name: String) -> WaitPoint {
        if self.pending.write().unwrap().insert(name.clone()) {
            if self.pending.read().unwrap().len() > self.max_components {
                panic!("Coordinator::track() called more times than expected");
            }
            WaitPoint::new(self, name)
        } else {
            unreachable!();
        }
    }

    // Note: should be invoked twice:
    //   - The first time the the units and targets reach the barrier when all
    //     are ready. The barrier is then automatically reset and ready for
    //     use again.
    //   - The second time the units and targets reach the barrier when all
    //     are running.
    pub async fn ready(self: Arc<Self>, name: &str) {
        if self
            .pending
            .read()
            .unwrap()
            .get(&name.to_string())
            .is_some()
        {
            self.barrier.wait().await;
        } else {
            unreachable!();
        }
    }

    pub async fn wait<T>(self: Arc<Self>, mut alarm: T)
    where
        T: FnMut(Vec<String>, &str),
    {
        // Units and targets need to reach the barrier twice: once to signal
        // that they are ready to run but are not yet actually running, and
        // once when they are running.
        self.clone().wait_internal(&mut alarm, "ready").await;
        self.wait_internal(&mut alarm, "running").await;
    }

    pub async fn wait_internal<T>(self: Arc<Self>, alarm: &mut T, status: &str)
    where
        T: FnMut(Vec<String>, &str),
    {
        debug!("Waiting for all components to become {}...", status);
        let num_unused_barriers = self.max_components - self.pending.read().unwrap().len() + 1;
        let unused_barriers: Vec<_> = (0..num_unused_barriers)
            .map(|_| self.barrier.wait())
            .collect();
        let slow_startup_alarm = Box::pin(tokio::time::sleep(Self::SLOW_COMPONENT_ALARM_DURATION));
        match select(join_all(unused_barriers), slow_startup_alarm).await {
            Either::Left(_) => {}
            Either::Right((_, incomplete_join_all)) => {
                // Raise the alarm about the slow components
                let pending_component_names = self
                    .pending
                    .read()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect::<Vec<String>>();
                alarm(pending_component_names, status);

                // Previous wait was interrupted, keep waiting
                incomplete_join_all.await;
            }
        }
        info!("All components are {}.", status);
    }
}

//------------ UnitSet -------------------------------------------------------

/// A set of units to be started.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct UnitSet {
    units: HashMap<String, Unit>,
}

impl UnitSet {
    pub fn units(&self) -> &HashMap<String, Unit> {
        &self.units
    }
}

impl From<HashMap<String, Unit>> for UnitSet {
    fn from(v: HashMap<String, Unit>) -> Self {
        Self { units: v }
    }
}

//------------ TargetSet -----------------------------------------------------

/// A set of targets to be started.
#[derive(Default)]
pub struct TargetSet {
    targets: HashMap<String, Target>,
}

impl TargetSet {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn targets(&self) -> &HashMap<String, Target> {
        &self.targets
    }
}

impl From<HashMap<String, Target>> for TargetSet {
    fn from(v: HashMap<String, Target>) -> Self {
        Self { targets: v }
    }
}

//------------ LoadUnit ------------------------------------------------------

/// A unit referenced during loading.
struct LoadUnit {
    /// The gate of the unit.
    ///
    /// This is some only if the unit is newly created and has not yet been
    /// spawned onto a runtime.
    gate: Option<Gate>,

    /// A gate agent for the unit.
    agent: GateAgent,
}

impl LoadUnit {
    fn new(queue_size: usize) -> Self {
        let (gate, agent) = Gate::new(queue_size);
        LoadUnit {
            gate: Some(gate),
            agent,
        }
    }
}

impl From<GateAgent> for LoadUnit {
    fn from(agent: GateAgent) -> Self {
        LoadUnit { gate: None, agent }
    }
}

// //------------ Loading FilterName --------------------------------------------

// thread_local!(
//     static ROTO_FILTER_NAMES: RefCell<Option<HashSet<FilterName>>> =
//         RefCell::new(Some(Default::default()))
// );

// /// Loads a filter name with the given name.
// ///
// /// # Panics
// ///
// /// This function panics if it is called outside of a run of
// /// [`Manager::load`].
// pub fn load_filter_name(filter_name: FilterName) -> FilterName {
//     ROTO_FILTER_NAMES.with(|filter_names| {
//         let mut filter_names = filter_names.borrow_mut();
//         let filter_names = filter_names.as_mut().unwrap();
//         filter_names.insert(filter_name.clone());
//         filter_name
//     })
// }

//------------ Tests ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        fmt::Display,
        ops::{Deref, DerefMut},
        sync::atomic::{AtomicU8, Ordering::SeqCst},
    };

    use super::*;

    static SOME_COMPONENT: &str = "some-component";
    static OTHER_COMPONENT: &str = "other-component";

    #[tokio::test]
    async fn coordinator_with_no_components_should_finish_immediately() {
        let coordinator = Coordinator::new(0);
        let mut alarm_fired = false;
        coordinator.wait(|_, _| alarm_fired = true).await;
        assert!(!alarm_fired);
    }

    #[tokio::test]
    #[should_panic]
    async fn coordinator_track_too_many_components_causes_panic() {
        let coordinator = Coordinator::new(0);
        coordinator.track(SOME_COMPONENT.to_string());
    }

    #[tokio::test]
    #[should_panic]
    async fn coordinator_track_component_twice_causes_panic() {
        let coordinator = Coordinator::new(0);
        coordinator.clone().track(SOME_COMPONENT.to_string());
        coordinator.track(SOME_COMPONENT.to_string());
    }

    #[tokio::test]
    #[should_panic]
    async fn coordinator_unknown_ready_component_twice_causes_panic() {
        let coordinator = Coordinator::new(0);
        coordinator.ready(SOME_COMPONENT).await;
    }

    #[tokio::test(start_paused = true)]
    async fn coordinator_with_one_ready_component_should_not_raise_alarm() {
        let coordinator = Coordinator::new(1);
        let mut alarm_fired = false;
        let wait_point = coordinator.clone().track(SOME_COMPONENT.to_string());
        let join_handle = tokio::task::spawn(wait_point.running());
        assert!(!join_handle.is_finished());
        coordinator.wait(|_, _| alarm_fired = true).await;
        join_handle.await.unwrap();
        assert!(!alarm_fired);
    }

    #[tokio::test(start_paused = true)]
    async fn coordinator_with_two_ready_components_should_not_raise_alarm() {
        let coordinator = Coordinator::new(2);
        let mut alarm_fired = false;
        let wait_point1 = coordinator.clone().track(SOME_COMPONENT.to_string());
        let wait_point2 = coordinator.clone().track(OTHER_COMPONENT.to_string());
        let join_handle1 = tokio::task::spawn(wait_point1.running());
        let join_handle2 = tokio::task::spawn(wait_point2.running());
        assert!(!join_handle1.is_finished());
        assert!(!join_handle2.is_finished());
        coordinator.wait(|_, _| alarm_fired = true).await;
        join_handle1.await.unwrap();
        join_handle2.await.unwrap();
        assert!(!alarm_fired);
    }

    #[tokio::test(start_paused = true)]
    async fn coordinator_with_component_with_slow_ready_phase_should_raise_alarm() {
        let coordinator = Coordinator::new(1);
        let alarm_fired_count = Arc::new(AtomicU8::new(0));
        let wait_point = coordinator.clone().track(SOME_COMPONENT.to_string());

        // Deliberately don't call wait_point.ready() or wait_point.running()
        let join_handle = {
            let alarm_fired_count = alarm_fired_count.clone();
            tokio::task::spawn(coordinator.wait(move |_, _| {
                alarm_fired_count.fetch_add(1, SeqCst);
            }))
        };

        // Advance time beyond the maximum time allowed for the 'ready' state to be reached
        let advance_time_by = Coordinator::SLOW_COMPONENT_ALARM_DURATION;
        let advance_time_by = advance_time_by.checked_add(Duration::from_secs(1)).unwrap();
        tokio::time::sleep(advance_time_by).await;

        // Check that the alarm fired once
        assert_eq!(alarm_fired_count.load(SeqCst), 1);

        // Set the component state to the final state 'running'
        wait_point.running().await;

        // Which should unblock the coordinator wait
        join_handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn coordinator_with_component_with_slow_running_phase_should_raise_alarm() {
        let coordinator = Coordinator::new(1);
        let alarm_fired_count = Arc::new(AtomicU8::new(0));
        let mut wait_point = coordinator.clone().track(SOME_COMPONENT.to_string());

        // Deliberately don't call wait_point.ready() or wait_point.running()
        let join_handle = {
            let alarm_fired_count = alarm_fired_count.clone();
            tokio::task::spawn(coordinator.wait(move |_, _| {
                alarm_fired_count.fetch_add(1, SeqCst);
            }))
        };

        // Advance time beyond the maximum time allowed for the 'ready' state to be reached
        let advance_time_by = Coordinator::SLOW_COMPONENT_ALARM_DURATION;
        let advance_time_by = advance_time_by.checked_add(Duration::from_secs(1)).unwrap();
        tokio::time::sleep(advance_time_by).await;

        // Check that the alarm fired once
        assert_eq!(alarm_fired_count.load(SeqCst), 1);

        // Achieve the 'ready' state in the component under test, but not yet the 'running' state
        wait_point.ready().await;

        // Advance time beyond the maximum time allowed for the 'running' state to be reached
        let advance_time_by = Coordinator::SLOW_COMPONENT_ALARM_DURATION;
        let advance_time_by = advance_time_by.checked_add(Duration::from_secs(1)).unwrap();
        tokio::time::sleep(advance_time_by).await;

        // Check that the alarm fired again
        assert_eq!(alarm_fired_count.load(SeqCst), 2);

        // Set the component state to the final state 'running'
        wait_point.running().await;

        // Which should unblock the coordinator wait
        join_handle.await.unwrap();
    }

    // --- Test helpers ------------------------------------------------------

    type UnitOrTargetName = String;

    #[derive(Debug, Eq, PartialEq)]
    enum SpawnAction {
        SpawnUnit,
        SpawnTarget,
        ReconfigureUnit,
        ReconfigureTarget,
        TerminateUnit,
        TerminateTarget,
    }

    impl Display for SpawnAction {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                SpawnAction::SpawnUnit => f.write_str("SpawnUnit"),
                SpawnAction::SpawnTarget => f.write_str("SpawnTarget"),
                SpawnAction::ReconfigureUnit => f.write_str("ReconfigureUnit"),
                SpawnAction::ReconfigureTarget => f.write_str("ReconfigureTarget"),
                SpawnAction::TerminateUnit => f.write_str("TerminateUnit"),
                SpawnAction::TerminateTarget => f.write_str("TerminateTarget"),
            }
        }
    }

    #[derive(Debug)]
    enum UnitOrTargetConfig {
        None,
        UnitConfig(Unit),
        TargetConfig(Target),
    }

    #[derive(Debug)]
    struct SpawnLogItem {
        pub name: UnitOrTargetName,
        pub action: SpawnAction,
        pub config: UnitOrTargetConfig,
    }

    impl SpawnLogItem {
        fn new(name: UnitOrTargetName, action: SpawnAction, _config: UnitOrTargetConfig) -> Self {
            Self {
                name,
                action,
                config: _config,
            }
        }
    }

    impl Display for SpawnLogItem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "'{}' for unit/target '{}'", self.action, self.name)
        }
    }

    #[derive(Debug, Default)]
    struct SpawnLog(pub Vec<SpawnLogItem>);

    impl SpawnLog {
        pub fn new() -> Self {
            Self(vec![])
        }
    }

    impl Display for SpawnLog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            writeln!(f, "[")?;
            for item in &self.0 {
                writeln!(f, "  {}", item)?;
            }
            writeln!(f, "]")
        }
    }

    impl Deref for SpawnLog {
        type Target = Vec<SpawnLogItem>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl DerefMut for SpawnLog {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    thread_local!(
        static SPAWN_LOG: RefCell<SpawnLog> = RefCell::new(SpawnLog::new())
    );

    fn assert_log_contains(log: &SpawnLog, name: &str, action: SpawnAction) {
        assert!(
            log.iter()
                .any(|item| item.name == name && item.action == action),
            "No '{}' action for unit/target '{}' found in spawn log: {}",
            action,
            name,
            log
        );
    }

    fn get_log_item<'a>(log: &'a SpawnLog, name: &str, action: SpawnAction) -> &'a SpawnLogItem {
        let found = log
            .iter()
            .find(|item| item.name == name && item.action == action);
        assert!(found.is_some());
        found.unwrap()
    }

    fn spawn_unit(c: Component, u: Unit, _: Gate, _: WaitPoint) {
        log_spawn_action(
            c.name.to_string(),
            SpawnAction::SpawnUnit,
            UnitOrTargetConfig::UnitConfig(u),
        );
    }

    fn spawn_target(c: Component, t: Target, _: Receiver<TargetCommand>, _: WaitPoint) {
        log_spawn_action(
            c.name.to_string(),
            SpawnAction::SpawnTarget,
            UnitOrTargetConfig::TargetConfig(t),
        );
    }

    fn reconfigure_unit(name: &str, _: GateAgent, u: Unit, _: Gate) {
        log_spawn_action(
            name.to_string(),
            SpawnAction::ReconfigureUnit,
            UnitOrTargetConfig::UnitConfig(u),
        );
    }

    fn reconfigure_target(name: &str, _: Sender<TargetCommand>, t: Target) {
        log_spawn_action(
            name.to_string(),
            SpawnAction::ReconfigureTarget,
            UnitOrTargetConfig::TargetConfig(t),
        );
    }

    fn terminate_unit(name: &str, _: Arc<GateAgent>) {
        log_spawn_action(
            name.to_string(),
            SpawnAction::TerminateUnit,
            UnitOrTargetConfig::None,
        );
    }

    fn terminate_target(name: &str, _: Arc<Sender<TargetCommand>>) {
        log_spawn_action(
            name.to_string(),
            SpawnAction::TerminateTarget,
            UnitOrTargetConfig::None,
        );
    }

    fn clear_spawn_action_log() {
        SPAWN_LOG.with(|log| log.borrow_mut().clear());
    }

    fn log_spawn_action(name: String, action: SpawnAction, cfg: UnitOrTargetConfig) {
        SPAWN_LOG.with(|log| log.borrow_mut().push(SpawnLogItem::new(name, action, cfg)));
    }

    fn spawn(manager: &mut Manager) {
        clear_spawn_action_log();
        manager.spawn_internal(
            spawn_unit,
            spawn_target,
            reconfigure_unit,
            reconfigure_target,
            terminate_unit,
            terminate_target,
        );
    }

    fn init_manager() -> Manager {
        // ROTO_FILTER_NAMES.with(|filter_names| filter_names.replace(Some(Default::default())));
        Manager::new()
    }
}
