//! Controlling the entire operation.

use arc_swap::ArcSwap;
use log::{debug, info};
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::common::file_io::TheFileIo;
use crate::common::tsig::TsigKeyStore;
use crate::comms::ApplicationCommand;
use crate::log::Logger;
use crate::metrics;
use crate::targets::central_command::{self, CentralCommandTarget};
use crate::targets::Target;
use crate::units::http_server::HttpServer;
use crate::units::key_manager::KeyManagerUnit;
use crate::units::zone_loader::ZoneLoader;
use crate::units::zone_server::{self, ZoneServerUnit};
use crate::units::zone_signer::{KmipServerConnectionSettings, TomlDenialConfig, ZoneSignerUnit};
use crate::units::Unit;
use domain::zonetree::{StoredName, ZoneTree};

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
    /// The logger.
    _logger: &'static Logger,

    /// Senders for the channels to send application commands to units
    cmd_txs: HashMap<Box<str>, mpsc::UnboundedSender<ApplicationCommand>>,

    center_tx: Option<mpsc::UnboundedSender<TargetCommand>>,

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

impl Manager {
    /// Creates a new manager.
    pub fn new(logger: &'static Logger) -> Self {
        let (app_cmd_tx, app_cmd_rx) = tokio::sync::mpsc::channel(10);

        let tsig_key_store = Default::default();
        let unsigned_zones = Default::default();
        let signed_zones = Default::default();
        let published_zones = Default::default();

        #[allow(clippy::let_and_return, clippy::default_constructed_unit_structs)]
        let manager = Manager {
            _logger: logger,
            center_tx: None,
            cmd_txs: HashMap::new(),
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
            if let Some(tx) = self.cmd_txs.get(&*unit_name) {
                debug!("Forwarding application command to unit '{unit_name}'");
                tx.send(data).unwrap();
            } else {
                debug!("Unrecognized unit: {unit_name}");
            }
        }
    }

    /// Spawns all units and targets
    pub fn spawn(&mut self) {
        let (update_tx, update_rx) = mpsc::unbounded_channel();

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
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            Self::spawn_target(component, new_target, cmd_rx);
            self.center_tx = Some(cmd_tx);
        }

        let mut kmip_server_conn_settings = HashMap::new();

        let hsm_relay_host = std::env::var("KMIP2PKCS11_HOST").ok();
        let hsm_relay_port = std::env::var("KMIP2PKCS11_PORT")
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
                        server_username: std::env::var("KMIP2PKCS11_USERNAME").ok(),
                        server_password: std::env::var("KMIP2PKCS11_PASSWORD").ok(),
                        ..Default::default()
                    },
                );
            }
        }

        let zone_name = StoredName::from_str(
            &std::env::var("ZL_IN_ZONE").unwrap_or("example.com.".to_string()),
        )
        .unwrap();
        let xfr_in = std::env::var("ZL_XFR_IN").unwrap_or("127.0.0.1:8055 KEY sec1-key".into());
        let xfr_out = std::env::var("PS_XFR_OUT").unwrap_or("127.0.0.1:8055 KEY sec1-key".into());
        let tsig_key_name = std::env::var("ZL_TSIG_KEY_NAME").unwrap_or("sec1-key".into());
        let tsig_key = std::env::var("ZL_TSIG_KEY")
            .unwrap_or("hmac-sha256:zlCZbVJPIhobIs1gJNQfrsS3xCxxsR9pMUrGwG8OgG8=".into());

        self.spawn_unit(
            "ZL",
            Unit::ZoneLoader(ZoneLoader {
                zones: Default::default(),
                xfr_in: Arc::new(HashMap::from([(zone_name.clone(), xfr_in)])),
                xfr_out: Arc::new(HashMap::from([(zone_name.clone(), xfr_out.clone())])),
                tsig_keys: HashMap::from([(tsig_key_name, tsig_key)]),
                update_tx: update_tx.clone(),
            }),
        );

        self.spawn_unit(
            "RS",
            Unit::ZoneServer(ZoneServerUnit {
                listen: vec![
                    "tcp:127.0.0.1:8056".parse().unwrap(),
                    "udp:127.0.0.1:8056".parse().unwrap(),
                ],
                xfr_out: HashMap::from([(zone_name.clone(), xfr_out)]),
                hooks: vec![String::from("/tmp/approve_or_deny.sh")],
                mode: zone_server::Mode::Prepublish,
                source: zone_server::Source::UnsignedZones,
                update_tx: update_tx.clone(),
                http_api_path: Arc::new(String::from("/_unit/rs/")),
            }),
        );

        self.spawn_unit(
            "KM",
            Unit::KeyManager(KeyManagerUnit {
                dnst_keyset_bin_path: "/tmp/dnst".into(),
                dnst_keyset_data_dir: "/tmp".into(),
                update_tx: update_tx.clone(),
            }),
        );

        self.spawn_unit(
            "ZS",
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
            }),
        );

        self.spawn_unit(
            "RS2",
            Unit::ZoneServer(ZoneServerUnit {
                http_api_path: Arc::new(String::from("/_unit/rs2/")),
                listen: vec![
                    "tcp:127.0.0.1:8057".parse().unwrap(),
                    "udp:127.0.0.1:8057".parse().unwrap(),
                ],
                xfr_out: HashMap::from([(zone_name.clone(), "127.0.0.1:8055 KEY sec1-key".into())]),
                hooks: vec![String::from("/tmp/approve_or_deny_signed.sh")],
                mode: zone_server::Mode::Prepublish,
                source: zone_server::Source::SignedZones,
                update_tx: update_tx.clone(),
            }),
        );

        self.spawn_unit(
            "PS",
            Unit::ZoneServer(ZoneServerUnit {
                http_api_path: Arc::new(String::from("/_unit/ps/")),
                listen: vec![
                    "tcp:127.0.0.1:8058".parse().unwrap(),
                    "udp:127.0.0.1:8058".parse().unwrap(),
                ],
                xfr_out: HashMap::from([(zone_name.into(), "127.0.0.1:8055".into())]),
                hooks: vec![],
                mode: zone_server::Mode::Publish,
                source: zone_server::Source::PublishedZones,
                update_tx: update_tx.clone(),
            }),
        );

        self.spawn_unit(
            "HS",
            Unit::HttpServer(HttpServer {
                // TODO: config/argument option
                listen_addr: "127.0.0.1:8950".parse().unwrap(),
            }),
        );
    }

    pub async fn terminate(&mut self) {
        for (name, tx) in self.cmd_txs.drain() {
            info!("Stopping unit '{name}'");
            let _ = tx.send(ApplicationCommand::Terminate);
            tx.closed().await;
        }

        {
            info!("Stopping target 'CC'");
            let tx = self.center_tx.take().unwrap();
            let _ = tx.send(TargetCommand::Terminate);
            tx.closed().await;
        }
    }

    fn spawn_unit(&mut self, name: &str, new_unit: Unit) {
        let component = Component::new(
            self.metrics.clone(),
            self.unsigned_zones.clone(),
            self.signed_zones.clone(),
            self.published_zones.clone(),
            self.tsig_key_store.clone(),
            self.app_cmd_tx.clone(),
        );

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        self.cmd_txs.insert(name.into(), cmd_tx);

        info!("Starting unit '{name}'");
        tokio::spawn(new_unit.run(cmd_rx, component));
    }

    fn spawn_target(
        component: Component,
        new_target: Target,
        cmd_rx: mpsc::UnboundedReceiver<TargetCommand>,
    ) {
        tokio::spawn(new_target.run(component, cmd_rx));
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
