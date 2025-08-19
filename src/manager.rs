//! Controlling the entire operation.

use arc_swap::ArcSwap;
use log::{debug, info};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::common::file_io::TheFileIo;
use crate::common::tsig::TsigKeyStore;
use crate::comms::ApplicationCommand;
use crate::metrics;
use crate::targets::central_command::{self, CentralCommandTarget};
use crate::targets::Target;
use crate::units::http_server::HttpServer;
use crate::units::key_manager::KeyManagerUnit;
use crate::units::zone_loader::ZoneLoader;
use crate::units::zone_server::{self, ZoneServerUnit};
use crate::units::zone_signer::{KmipServerConnectionSettings, TomlDenialConfig, ZoneSignerUnit};
use crate::units::Unit;
use domain::zonetree::ZoneTree;

//------------ Component -----------------------------------------------------

/// Facilities available to all components.
///
/// Upon being started, every component receives one of these. It provides
/// access to information and services available to all components.
pub struct Component {
    /// A reference to the metrics collection.
    #[allow(dead_code)]
    metrics: Option<metrics::Collection>,

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
            metrics: Default::default(),
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
        metrics: metrics::Collection,
        unsigned_zones: Arc<ArcSwap<ZoneTree>>,
        signed_zones: Arc<ArcSwap<ZoneTree>>,
        published_zones: Arc<ArcSwap<ZoneTree>>,
        tsig_key_store: TsigKeyStore,
        app_cmd_tx: Sender<(String, ApplicationCommand)>,
    ) -> Self {
        Component {
            metrics: Some(metrics),
            unsigned_zones,
            signed_zones,
            published_zones,
            tsig_key_store,
            app_cmd_tx,
        }
    }

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

    pub async fn send_command(&self, target_unit_name: &str, data: ApplicationCommand) {
        self.app_cmd_tx
            .send((target_unit_name.to_string(), data))
            .await
            .unwrap()
    }
}

//------------ Manager -------------------------------------------------------

#[allow(clippy::large_enum_variant)]
pub enum TargetCommand {
    Terminate,
}

impl Display for TargetCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetCommand::Terminate => f.write_str("Terminate"),
        }
    }
}

/// A manager for components and auxiliary services.
///
/// Requires a running Tokio reactor that has been "entered" (see Tokio
/// `Handle::enter()`).
pub struct Manager {
    /// Commands for the zone loader.
    loader_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// Commands for the review server.
    review_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// Commands for the key manager.
    key_manager_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// Commands for the zone signer.
    signer_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// Commands for the secondary review server.
    review2_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// Commands for the publish server.
    publish_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// Commands for the central command.
    center_tx: Option<mpsc::Sender<TargetCommand>>,

    /// Commands for the http server.
    http_tx: Option<mpsc::Sender<ApplicationCommand>>,

    /// The metrics collection maintained by this manager.
    metrics: metrics::Collection,

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
            loader_tx: None,
            review_tx: None,
            key_manager_tx: None,
            signer_tx: None,
            review2_tx: None,
            publish_tx: None,
            center_tx: None,
            http_tx: None,
            metrics: Default::default(),
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
            let Some(tx) = (match &*unit_name {
                "ZL" => self.loader_tx.as_ref(),
                "RS" => self.review_tx.as_ref(),
                "KM" => self.key_manager_tx.as_ref(),
                "ZS" => self.signer_tx.as_ref(),
                "RS2" => self.review2_tx.as_ref(),
                "PS" => self.publish_tx.as_ref(),
                "HS" => self.http_tx.as_ref(),
                _ => None,
            }) else {
                continue;
            };
            debug!("Forwarding application command to unit '{unit_name}'");
            tx.send(data).await.unwrap();
        }
    }

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
        self.spawn_internal(Self::spawn_unit, Self::spawn_target)
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
    fn spawn_internal<SpawnUnit, SpawnTarget>(
        &mut self,
        spawn_unit: SpawnUnit,
        spawn_target: SpawnTarget,
    ) where
        SpawnUnit: Fn(Component, Unit),
        SpawnTarget: Fn(Component, Target, Receiver<TargetCommand>),
    {
        let (zl_tx, zl_rx) = mpsc::channel(10);
        let (rs_tx, rs_rx) = mpsc::channel(10);
        let (km_tx, km_rx) = mpsc::channel(10);
        let (zs_tx, zs_rx) = mpsc::channel(10);
        let (rs2_tx, rs2_rx) = mpsc::channel(10);
        let (ps_tx, ps_rx) = mpsc::channel(10);
        let (http_tx, http_rx) = mpsc::channel(10);

        self.loader_tx = Some(zl_tx);
        self.review_tx = Some(rs_tx);
        self.key_manager_tx = Some(km_tx);
        self.signer_tx = Some(zs_tx);
        self.review2_tx = Some(rs2_tx);
        self.publish_tx = Some(ps_tx);
        self.http_tx = Some(http_tx);

        let (update_tx, update_rx) = mpsc::channel(10);

        {
            let name = String::from("CC");
            let new_target = Target::CentraLCommand(CentralCommandTarget {
                config: central_command::Config {},
                update_rx,
            });

            // Spawn the new target
            let component = Component::new(
                self.metrics.clone(),
                self.unsigned_zones.clone(),
                self.signed_zones.clone(),
                self.published_zones.clone(),
                self.tsig_key_store.clone(),
                self.app_cmd_tx.clone(),
            );

            info!("Starting target '{name}'");
            let (cmd_tx, cmd_rx) = mpsc::channel(100);
            spawn_target(component, new_target, cmd_rx);
            self.center_tx = Some(cmd_tx);
        }

        let mut kmip_server_conn_settings = HashMap::new();

        let hsm_relay_host = std::env::var("NAMESHED_HSM_RELAY_HOST").ok();
        let hsm_relay_port = std::env::var("NAMESHED_HSM_RELAY_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok());

        if let Some(server_addr) = hsm_relay_host {
            if let Some(server_port) = hsm_relay_port {
                kmip_server_conn_settings.insert(
                    "hsmrelay".to_string(),
                    KmipServerConnectionSettings {
                        server_addr,
                        server_port,
                        server_insecure: true,
                        server_username: std::env::var("NAMESHED_HSM_RELAY_USERNAME").ok(),
                        server_password: std::env::var("NAMESHED_HSM_RELAY_PASSWORD").ok(),
                        ..Default::default()
                    },
                );
            }
        }

        let client_cert_path = std::env::var("PYKMIP_CLIENT_CERT_PATH").ok();
        let client_key_path = std::env::var("PYKMIP_CLIENT_KEY_PATH").ok();

        if client_cert_path.is_some() && client_key_path.is_some() {
            kmip_server_conn_settings.insert(
                "pykmip".to_string(),
                KmipServerConnectionSettings {
                    server_addr: "127.0.0.1".into(),
                    server_port: 5696,
                    server_insecure: true,
                    client_cert_path: Some(
                        "/home/ximon/docker_data/pykmip/pykmip-data/selfsigned.crt".into(),
                    ),
                    client_key_path: Some(
                        "/home/ximon/docker_data/pykmip/pykmip-data/selfsigned.key".into(),
                    ),
                    ..Default::default()
                },
            );
        }

        let server_username = std::env::var("FORTANIX_USER").ok();
        let server_password = std::env::var("FORTANIX_PASS").ok();
        if server_username.is_some() && server_password.is_some() {
            kmip_server_conn_settings.insert(
                "fortanix".to_string(),
                KmipServerConnectionSettings {
                    server_addr: "eu.smartkey.io".into(),
                    server_insecure: true,
                    server_username,
                    server_password,
                    ..Default::default()
                },
            );
        }

        let zone_name = std::env::var("ZL_IN_ZONE").unwrap_or("example.com.".into());
        let zone_file = std::env::var("ZL_IN_ZONE_FILE").unwrap_or("".into());
        let xfr_in = std::env::var("ZL_XFR_IN").unwrap_or("127.0.0.1:8055 KEY sec1-key".into());
        let tsig_key_name = std::env::var("ZL_TSIG_KEY_NAME").unwrap_or("sec1-key".into());
        let tsig_key = std::env::var("ZL_TSIG_KEY")
            .unwrap_or("hmac-sha256:zlCZbVJPIhobIs1gJNQfrsS3xCxxsR9pMUrGwG8OgG8=".into());

        let units = [
            (
                String::from("ZL"),
                Unit::ZoneLoader(ZoneLoader {
                    listen: vec![
                        "tcp:127.0.0.1:8054".parse().unwrap(),
                        "udp:127.0.0.1:8054".parse().unwrap(),
                    ],
                    zones: Arc::new(HashMap::from([(zone_name.clone(), zone_file)])),
                    xfr_in: Arc::new(HashMap::from([(zone_name, xfr_in)])),
                    tsig_keys: HashMap::from([(tsig_key_name, tsig_key)]),
                    update_tx: update_tx.clone(),
                    cmd_rx: zl_rx,
                }),
            ),
            (
                String::from("RS"),
                Unit::ZoneServer(ZoneServerUnit {
                    listen: vec![
                        "tcp:127.0.0.1:8056".parse().unwrap(),
                        "udp:127.0.0.1:8056".parse().unwrap(),
                    ],
                    xfr_out: HashMap::from([(
                        "example.com".into(),
                        "127.0.0.1:8055 KEY sec1-key".into(),
                    )]),
                    // Temporarily disable hooks as the required HTTP functionality has been removed pending replacement.
                    hooks: vec![], // vec![String::from("/tmp/approve_or_deny.sh")],
                    mode: zone_server::Mode::Prepublish,
                    source: zone_server::Source::UnsignedZones,
                    update_tx: update_tx.clone(),
                    cmd_rx: rs_rx,
                }),
            ),
            (
                String::from("KM"),
                Unit::KeyManager(KeyManagerUnit {
                    dnst_keyset_bin_path: "/tmp/dnst".into(),
                    dnst_keyset_data_dir: "/tmp".into(),
                    update_tx: update_tx.clone(),
                    cmd_rx: km_rx,
                }),
            ),
            (
                String::from("ZS"),
                Unit::ZoneSigner(ZoneSignerUnit {
                    keys_path: "/tmp/keys".into(),
                    treat_single_keys_as_csks: true,
                    max_concurrent_operations: 1,
                    max_concurrent_rrsig_generation_tasks: 32,
                    use_lightweight_zone_tree: false,
                    denial_config: TomlDenialConfig::default(), //Nsec3(NonEmpty::new(TomlNsec3Config::default())),
                    rrsig_inception_offset_secs: 60 * 90,
                    rrsig_expiration_offset_secs: 60 * 60 * 24 * 14,
                    kmip_server_conn_settings,
                    update_tx: update_tx.clone(),
                    cmd_rx: zs_rx,
                }),
            ),
            (
                String::from("RS2"),
                Unit::ZoneServer(ZoneServerUnit {
                    listen: vec![
                        "tcp:127.0.0.1:8057".parse().unwrap(),
                        "udp:127.0.0.1:8057".parse().unwrap(),
                    ],
                    xfr_out: HashMap::from([(
                        "example.com".into(),
                        "127.0.0.1:8055 KEY sec1-key".into(),
                    )]),
                    // Temporarily disable hooks as the required HTTP functionality has been removed pending replacement.
                    hooks: vec![], // vec![String::from("/tmp/approve_or_deny_signed.sh")],
                    mode: zone_server::Mode::Prepublish,
                    source: zone_server::Source::SignedZones,
                    update_tx: update_tx.clone(),
                    cmd_rx: rs2_rx,
                }),
            ),
            (
                String::from("PS"),
                Unit::ZoneServer(ZoneServerUnit {
                    listen: vec![
                        "tcp:127.0.0.1:8058".parse().unwrap(),
                        "udp:127.0.0.1:8058".parse().unwrap(),
                    ],
                    xfr_out: HashMap::from([("example.com".into(), "127.0.0.1:8055".into())]),
                    hooks: vec![],
                    mode: zone_server::Mode::Publish,
                    source: zone_server::Source::PublishedZones,
                    update_tx: update_tx.clone(),
                    cmd_rx: ps_rx,
                }),
            ),
            (
                String::from("HS"),
                Unit::HttpServer(HttpServer {
                    // TODO: config/argument option
                    listen_addr: "127.0.0.1:8950".parse().unwrap(),
                    cmd_rx: http_rx,
                }),
            ),
        ];

        // Spawn and terminate units
        for (name, new_unit) in units {
            // Spawn the new unit
            let component = Component::new(
                self.metrics.clone(),
                self.unsigned_zones.clone(),
                self.signed_zones.clone(),
                self.published_zones.clone(),
                self.tsig_key_store.clone(),
                self.app_cmd_tx.clone(),
            );

            let _unit_type = std::mem::discriminant(&new_unit);
            info!("Starting unit '{name}'");
            spawn_unit(component, new_unit);
        }
    }

    pub fn terminate(&mut self) {
        let units = [
            ("ZL", self.loader_tx.take().unwrap()),
            ("RS", self.review_tx.take().unwrap()),
            ("ZS", self.signer_tx.take().unwrap()),
            ("RS2", self.review2_tx.take().unwrap()),
            ("PS", self.publish_tx.take().unwrap()),
            ("HS", self.http_tx.take().unwrap()),
        ];
        for (name, tx) in units {
            info!("Stopping unit '{name}'");
            tokio::spawn(async move {
                let _ = tx.send(ApplicationCommand::Terminate).await;
                tx.closed().await;
            });
        }

        {
            let cmd_tx = Arc::new(self.center_tx.take().unwrap());
            Self::terminate_target("CC", cmd_tx.clone());
            while !cmd_tx.is_closed() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn spawn_unit(component: Component, new_unit: Unit) {
        tokio::spawn(new_unit.run(component));
    }

    fn spawn_target(component: Component, new_target: Target, cmd_rx: Receiver<TargetCommand>) {
        tokio::spawn(new_target.run(component, cmd_rx));
    }

    fn terminate_target(name: &str, sender: Arc<Sender<TargetCommand>>) {
        info!("Stopping target '{name}'");
        tokio::spawn(async move {
            let _ = sender.send(TargetCommand::Terminate).await;
        });
    }

    /// Returns a new reference to the manager’s metrics collection.
    pub fn metrics(&self) -> metrics::Collection {
        self.metrics.clone()
    }
}

//------------ UnitSet -------------------------------------------------------

/// A set of units to be started.
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
