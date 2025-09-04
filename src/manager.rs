//! Controlling the entire operation.

use log::debug;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc::{self};

use crate::center::Center;
use crate::comms::ApplicationCommand;
use crate::payload::Update;
use crate::targets::central_command::CentralCommand;
use crate::units::http_server::HttpServer;
use crate::units::key_manager::KeyManagerUnit;
use crate::units::zone_loader::ZoneLoader;
use crate::units::zone_server::{self, ZoneServerUnit};
use crate::units::zone_signer::{KmipServerConnectionSettings, ZoneSignerUnit};
use domain::zonetree::StoredName;

/// Spawn all targets.
pub fn spawn(
    center: &Arc<Center>,
    update_rx: mpsc::UnboundedReceiver<Update>,
    center_tx_slot: &mut Option<mpsc::UnboundedSender<TargetCommand>>,
    unit_tx_slots: &mut foldhash::HashMap<String, mpsc::UnboundedSender<ApplicationCommand>>,
) {
    // Spawn the central command.
    log::info!("Starting target 'CC'");
    let target = CentralCommand {
        center: center.clone(),
    };
    let (center_tx, center_rx) = mpsc::unbounded_channel();
    tokio::spawn(target.run(center_rx, update_rx));
    *center_tx_slot = Some(center_tx);

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

    let zone_name =
        StoredName::from_str(&std::env::var("ZL_IN_ZONE").unwrap_or("example.com.".to_string()))
            .unwrap();
    let xfr_in = std::env::var("ZL_XFR_IN").unwrap_or("127.0.0.1:8055 KEY sec1-key".into());
    let xfr_out = std::env::var("PS_XFR_OUT").unwrap_or("127.0.0.1:8055 KEY sec1-key".into());
    let tsig_key_name = std::env::var("ZL_TSIG_KEY_NAME").unwrap_or("sec1-key".into());
    let tsig_key = std::env::var("ZL_TSIG_KEY")
        .unwrap_or("hmac-sha256:zlCZbVJPIhobIs1gJNQfrsS3xCxxsR9pMUrGwG8OgG8=".into());

    // Global settings for dnst keyset
    // TODO: Should probably be configurable
    let dnst_keyset_bin_path: PathBuf = "dnst".into();
    let dnst_keyset_data_dir: PathBuf = "/tmp/keyset/".into();

    // Spawn the zone loader.
    log::info!("Starting unit 'ZL'");
    let unit = ZoneLoader {
        center: center.clone(),
        zones: Default::default(),
        xfr_in: Arc::new(HashMap::from([(zone_name.clone(), xfr_in)])),
        xfr_out: Arc::new(HashMap::from([(zone_name.clone(), xfr_out.clone())])),
        tsig_keys: HashMap::from([(tsig_key_name, tsig_key)]),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("ZL".into(), cmd_tx);

    // Spawn the unsigned zone review server.
    log::info!("Starting unit 'RS'");
    let unit = ZoneServerUnit {
        center: center.clone(),
        _xfr_out: HashMap::from([(zone_name.clone(), xfr_out)]),
        hooks: vec![String::from("/tmp/approve_or_deny.sh")],
        mode: zone_server::Mode::Prepublish,
        source: zone_server::Source::UnsignedZones,
        http_api_path: Arc::new(String::from("/_unit/rs/")),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("RS".into(), cmd_tx);

    // Spawn the key manager.
    log::info!("Starting unit 'KM'");
    let unit = KeyManagerUnit {
        center: center.clone(),
        dnst_keyset_bin_path: dnst_keyset_bin_path.clone(),
        dnst_keyset_data_dir: dnst_keyset_data_dir.clone(),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("KM".into(), cmd_tx);

    // Spawn the zone signer.
    log::info!("Starting unit 'ZS'");
    let unit = ZoneSignerUnit {
        center: center.clone(),
        treat_single_keys_as_csks: true,
        max_concurrent_operations: 1,
        max_concurrent_rrsig_generation_tasks: 32,
        use_lightweight_zone_tree: false,
        kmip_server_conn_settings,
        dnst_keyset_data_dir: dnst_keyset_data_dir.clone(),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("ZS".into(), cmd_tx);

    // Spawn the signed zone review server.
    log::info!("Starting unit 'RS2'");
    let unit = ZoneServerUnit {
        center: center.clone(),
        http_api_path: Arc::new(String::from("/_unit/rs2/")),
        _xfr_out: HashMap::from([(zone_name.clone(), "127.0.0.1:8055 KEY sec1-key".into())]),
        hooks: vec![String::from("/tmp/approve_or_deny_signed.sh")],
        mode: zone_server::Mode::Prepublish,
        source: zone_server::Source::SignedZones,
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("RS2".into(), cmd_tx);

    // Spawn the published zone server.
    log::info!("Starting unit 'PS'");
    let unit = ZoneServerUnit {
        center: center.clone(),
        http_api_path: Arc::new(String::from("/_unit/ps/")),
        _xfr_out: HashMap::from([(zone_name, "127.0.0.1:8055".into())]),
        hooks: vec![],
        mode: zone_server::Mode::Publish,
        source: zone_server::Source::PublishedZones,
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("PS".into(), cmd_tx);

    // Spawn the HTTP server.
    log::info!("Starting unit 'HS'");
    let unit = HttpServer {
        center: center.clone(),
        // TODO: config/argument option
        listen_addr: "127.0.0.1:8950".parse().unwrap(),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(unit.run(cmd_rx));
    unit_tx_slots.insert("HS".into(), cmd_tx);
}

/// Forward application commands.
//
// TODO: Eliminate this function entirely.
pub async fn forward_app_cmds(
    rx: &mut mpsc::UnboundedReceiver<(String, ApplicationCommand)>,
    unit_txs: &foldhash::HashMap<String, mpsc::UnboundedSender<ApplicationCommand>>,
) {
    while let Some((unit_name, data)) = rx.recv().await {
        if let Some(tx) = unit_txs.get(&*unit_name) {
            debug!("Forwarding application command to unit '{unit_name}'");
            tx.send(data).unwrap();
        } else {
            debug!("Unrecognized unit: {unit_name}");
        }
    }
}

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
