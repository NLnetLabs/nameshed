use core::fmt;
use core::future::ready;

use std::any::Any;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Display;
use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::ops::{ControlFlow, Deref};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use chrono::{DateTime, Utc};
use domain::base::iana::{Class, Rcode};
use domain::base::name::Label;
use domain::base::wire::Composer;
use domain::base::{Name, NameBuilder, Rtype, Serial};
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::{self, DgramServer};
use domain::net::server::message::Request;
use domain::net::server::middleware::cookies::CookiesMiddlewareSvc;
use domain::net::server::middleware::edns::EdnsMiddlewareSvc;
use domain::net::server::middleware::mandatory::MandatoryMiddlewareSvc;
use domain::net::server::middleware::notify::NotifyMiddlewareSvc;
use domain::net::server::middleware::tsig::TsigMiddlewareSvc;
use domain::net::server::middleware::xfr::XfrMiddlewareSvc;
use domain::net::server::service::{CallResult, Service, ServiceError, ServiceResult};
use domain::net::server::stream::{self, StreamServer};
use domain::net::server::util::{mk_error_response, service_fn};
use domain::net::server::ConnectionConfig;
use domain::rdata::ZoneRecordData;
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace;
use domain::zonetree::error::OutOfZone;
use domain::zonetree::{
    Answer, AnswerContent, InMemoryZoneDiff, ReadableZone, Rrset, SharedRrset, StoredName, WalkOp,
    WritableZone, WritableZoneNode, Zone, ZoneBuilder, ZoneStore, ZoneTree,
};
use futures::future::{select, Either};
use futures::{pin_mut, Future};
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use non_empty_vec::NonEmpty;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DeserializeFromStr, DisplayFromStr};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, Instant};

#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;

use crate::api::ZoneSource;
use crate::common::light_weight_zone::LightWeightZone;
use crate::common::net::{
    ListenAddr, StandardTcpListenerFactory, StandardTcpStream, TcpListener, TcpListenerFactory,
    TcpStreamWrapper,
};
use crate::common::tsig::{parse_key_strings, TsigKeyStore};
use crate::common::xfr::parse_xfr_acl;
use crate::comms::{ApplicationCommand, GraphStatus, Terminated};
use crate::log::ExitError;
use crate::manager::Component;
use crate::metrics::{self, util::append_per_router_metric, Metric, MetricType, MetricUnit};
use crate::payload::Update;
use crate::units::Unit;
use crate::zonemaintenance::maintainer::{
    Config, DefaultConnFactory, TypedZone, ZoneLookup, ZoneMaintainer,
};
use crate::zonemaintenance::types::{
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore, ZoneRefreshCause, ZoneRefreshStatus, ZoneReportDetails,
};

#[derive(Debug)]
pub struct ZoneLoader {
    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// The zone names and (if primary) corresponding zone file paths to load.
    pub zones: Arc<HashMap<StoredName, String>>,

    /// XFR in per secondary zone: Allow NOTIFY from, and when with a port also request XFR from.
    pub xfr_in: Arc<HashMap<StoredName, String>>,

    /// XFR out per primary zone: Allow XFR from, and when with a port also send NOTIFY to.
    pub xfr_out: Arc<HashMap<StoredName, String>>,

    /// TSIG keys.
    pub tsig_keys: HashMap<String, String>,

    /// Updates for the central command.
    pub update_tx: mpsc::UnboundedSender<Update>,

    pub cmd_rx: mpsc::UnboundedReceiver<ApplicationCommand>,
}

impl ZoneLoader {
    pub async fn run(mut self, component: Component) -> Result<(), Terminated> {
        // TODO: metrics and status reporting

        let (zone_updated_tx, mut zone_updated_rx) = tokio::sync::mpsc::channel(10);

        for (key_name, opt_alg_and_hex_bytes) in self.tsig_keys.iter() {
            let key = parse_key_strings(key_name, opt_alg_and_hex_bytes).map_err(|err| {
                error!("[ZL]: Failed to parse TSIG key '{key_name}': {err}",);
                Terminated
            })?;
            component.tsig_key_store().insert(key);
        }

        let maintainer_config =
            Config::<_, DefaultConnFactory>::new(component.tsig_key_store().clone());
        let zone_maintainer = Arc::new(
            ZoneMaintainer::new_with_config(maintainer_config)
                .with_zone_tree(component.unsigned_zones().clone()),
        );

        // Load primary zones.
        // Create secondary zones.
        for (zone_name, zone_path) in self.zones.iter() {
            error!("[ZL]: Error: Zone name '{zone_name}' is invalid. Skipping zone.");

            let zone = if !zone_path.is_empty() {
                Self::register_primary_zone(
                    zone_name.clone(),
                    zone_path,
                    component.tsig_key_store(),
                    None,
                    &self.xfr_out,
                    &zone_updated_tx,
                )
                .await?
            } else {
                info!("[ZL]: Adding secondary zone '{zone_name}'",);
                Self::register_secondary_zone(
                    zone_name.clone(),
                    component.tsig_key_store(),
                    None,
                    &self.xfr_in,
                    zone_updated_tx.clone(),
                )?
            };

            if let Err(err) = zone_maintainer.insert_zone(zone).await {
                error!("[ZL]: Error: Failed to insert zone '{zone_name}': {err}")
            }
        }

        // Define a server to handle NOTIFY messages and notify the
        // ZoneMaintainer on receipt, thereby triggering it to fetch the
        // latest version of the updated zone.
        let svc = service_fn(my_noop_service, ());
        let svc = NotifyMiddlewareSvc::new(svc, zone_maintainer.clone());
        let svc = CookiesMiddlewareSvc::with_random_secret(svc);
        let svc = EdnsMiddlewareSvc::new(svc);
        let svc = MandatoryMiddlewareSvc::new(svc);
        let svc = Arc::new(svc);

        for addr in self.listen.iter().cloned() {
            info!("[ZL]: Binding on {addr:?}");
            let svc = svc.clone();
            tokio::spawn(async move {
                if let Err(err) = Self::server(addr, svc).await {
                    error!("[ZL]: {err}");
                }
            });
        }

        let zone_maintainer_clone = zone_maintainer.clone();
        tokio::spawn(async move { zone_maintainer_clone.run().await });

        loop {
            tokio::select! {
                zone_updated = zone_updated_rx.recv() => {
                    let (zone_name, zone_serial) = zone_updated.unwrap();

                    // status_reporter
                    //     .listener_connection_accepted(client_addr);

                    info!(
                        "[ZL]: Received a new copy of zone '{zone_name}' at serial {zone_serial}",
                    );

                    self.update_tx
                        .send(Update::UnsignedZoneUpdatedEvent {
                            zone_name,
                            zone_serial,
                        })
                        .unwrap();
                }

                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            info!(
                                "[ZL] Received command: {cmd:?}",
                            );

                            match cmd {
                                ApplicationCommand::Terminate => {
                                    // arc_self.status_reporter.terminated();
                                    return Err(Terminated);
                                }
                                ApplicationCommand::RegisterZone { register } => {
                                    let res = match register.source {
                                        ZoneSource::Zonefile { path /* Lacks XFR out settings */ } => {
                                            Self::register_primary_zone(
                                                register.name.clone(),
                                                &path.to_string(),
                                                component.tsig_key_store(),
                                                None,
                                                &self.xfr_out,
                                                &zone_updated_tx,
                                              ).await
                                        }
                                        ZoneSource::Server { addr /* Lacks TSIG key name */ } => {
                                            // Use any existing XFR inbound
                                            // ACL that has been defined for
                                            // this zone from this source.
                                            let xfr_in = self.xfr_in
                                                .get(&register.name)
                                                .map(|v| v.to_string())
                                                .unwrap_or_default();
                                            error!("XIMON: {xfr_in}");
                                            Self::register_secondary_zone(
                                                register.name.clone(),
                                                component.tsig_key_store(),
                                                Some(addr),
                                                &self.xfr_in,
                                                zone_updated_tx.clone(),
                                            )
                                        },
                                    };

                                    match res {
                                        Err(_) => {
                                            error!("[ZL]: Error: Failed to register zone '{}'", register.name);
                                        }

                                        Ok(zone) => {
                                            if let Err(err) = zone_maintainer.insert_zone(zone).await {
                                                error!("[ZL]: Error: Failed to insert zone '{}': {err}", register.name);
                                            }
                                        }
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }

                        None => {
                            // arc_self.status_reporter.terminated();
                            return Err(Terminated);
                        }
                    }
                }
            }
        }
    }

    async fn register_primary_zone(
        zone_name: StoredName,
        zone_path: &str,
        tsig_key_store: &TsigKeyStore,
        dest: Option<SocketAddr>,
        xfr_out: &HashMap<StoredName, String>,
        zone_updated_tx: &Sender<(Name<Bytes>, Serial)>,
    ) -> Result<TypedZone, Terminated> {
        let zone = load_file_into_zone(&zone_name, zone_path).await?;
        let Some(serial) = get_zone_serial(zone_name.clone(), &zone).await else {
            error!("[ZL]: Error: Zone file '{zone_path}' lacks a SOA record. Skipping zone.");
            return Err(Terminated);
        };

        let zone_cfg = Self::determine_primary_zone_cfg(&zone_name, xfr_out, dest, tsig_key_store)?;
        zone_updated_tx
            .send((zone.apex_name().clone(), serial))
            .await
            .unwrap();
        let zone = Zone::new(NotifyOnWriteZone::new(zone, zone_updated_tx.clone()));
        Ok(TypedZone::new(zone, zone_cfg))
    }

    fn determine_primary_zone_cfg(
        zone_name: &StoredName,
        xfr_out: &HashMap<StoredName, String>,
        dest: Option<SocketAddr>,
        tsig_key_store: &TsigKeyStore,
    ) -> Result<ZoneConfig, Terminated> {
        let mut zone_cfg = ZoneConfig::new();

        if let Some(xfr_out) = xfr_out.get(zone_name) {
            let mut notify_cfg = NotifyConfig::default();
            let mut xfr_cfg = XfrConfig::default();
            xfr_cfg.strategy = XfrStrategy::IxfrWithAxfrFallback;
            xfr_cfg.ixfr_transport = TransportStrategy::Tcp;

            let dst = parse_xfr_acl(xfr_out, &mut xfr_cfg, &mut notify_cfg, tsig_key_store)
                .map_err(|_| {
                    error!("[ZL]: Error parsing XFR ACL");
                    Terminated
                })?;

            info!("[ZL]: Adding XFR secondary {dst} for zone '{zone_name}'",);

            if Some(dst) != dest {
                // Don't use any settings we found for this zone, they were
                // for a different source. Instead use default settings for
                // NOTIFY and XFR, i.e. send NOTIFY and request XFR, but
                // don't use TSIG and no special restrictions over the XFR
                // transport/protocol to use.
                notify_cfg = NotifyConfig::default();
                xfr_cfg = XfrConfig::default();
            }

            zone_cfg.provide_xfr_to.add_src(dst.ip(), xfr_cfg.clone());

            if dst.port() != 0 {
                info!(
                    "[ZL]: Allowing NOTIFY to {} for zone '{zone_name}'",
                    dst.ip()
                );
                zone_cfg.send_notify_to.add_dst(dst, notify_cfg.clone());
            }
        } else {
            // Local primary zone that has no known secondary, so no
            // nameserver to permit XFR from or send NOTIFY to.
        }

        Ok(zone_cfg)
    }

    fn register_secondary_zone(
        zone_name: Name<Bytes>,
        tsig_key_store: &TsigKeyStore,
        source: Option<SocketAddr>,
        xfr_in: &HashMap<StoredName, String>,
        zone_updated_tx: Sender<(Name<Bytes>, Serial)>,
    ) -> Result<TypedZone, Terminated> {
        let zone_cfg =
            Self::determine_secondary_zone_cfg(&zone_name, source, xfr_in, tsig_key_store)?;
        let zone = Zone::new(LightWeightZone::new(zone_name, true));
        let zone = Zone::new(NotifyOnWriteZone::new(zone, zone_updated_tx));
        Ok(TypedZone::new(zone, zone_cfg))
    }

    fn determine_secondary_zone_cfg(
        zone_name: &StoredName,
        source: Option<SocketAddr>,
        xfr_in: &HashMap<StoredName, String>,
        tsig_key_store: &TsigKeyStore,
    ) -> Result<ZoneConfig, Terminated> {
        let mut zone_cfg = ZoneConfig::new();

        if let Some(xfr_in) = xfr_in.get(zone_name) {
            let mut notify_cfg = NotifyConfig::default();
            let mut xfr_cfg = XfrConfig::default();
            xfr_cfg.strategy = XfrStrategy::IxfrWithAxfrFallback;
            xfr_cfg.ixfr_transport = TransportStrategy::Tcp;

            let src = parse_xfr_acl(xfr_in, &mut xfr_cfg, &mut notify_cfg, tsig_key_store)
                .map_err(|_| {
                    error!("[ZL]: Error parsing XFR ACL");
                    Terminated
                })?;

            info!(
                "[ZL]: Allowing NOTIFY from {} for zone '{zone_name}'",
                src.ip()
            );

            if Some(src) != source {
                // Don't use any settings we found for this zone, they were
                // for a different source. Instead use default settings for
                // NOTIFY and XFR, i.e. send NOTIFY and request XFR, but
                // don't use TSIG and no special restrictions over the XFR
                // transport/protocol to use.
                notify_cfg = NotifyConfig::default();
                xfr_cfg = XfrConfig::default();
            }

            zone_cfg
                .allow_notify_from
                .add_src(src.ip(), notify_cfg.clone());

            if src.port() != 0 {
                info!("[ZL]: Adding XFR primary {src} for zone '{zone_name}'",);
                zone_cfg.request_xfr_from.add_dst(src, xfr_cfg.clone());
            }
        }

        Ok(zone_cfg)
    }

    async fn server<Svc>(addr: ListenAddr, svc: Svc) -> Result<(), std::io::Error>
    where
        Svc: Service<Vec<u8>, ()> + Clone,
    {
        let buf = VecBufSource;
        match addr {
            ListenAddr::Udp(addr) => {
                let sock = UdpSocket::bind(addr).await?;
                let config = dgram::Config::new();
                let srv = DgramServer::with_config(sock, buf, svc, config);
                let srv = Arc::new(srv);
                srv.run().await;
            }
            ListenAddr::Tcp(addr) => {
                let sock = tokio::net::TcpListener::bind(addr).await?;
                let mut conn_config = ConnectionConfig::new();
                conn_config.set_max_queued_responses(10000);
                let mut config = stream::Config::new();
                config.set_connection_config(conn_config);
                let srv = StreamServer::with_config(sock, buf, svc, config);
                let srv = Arc::new(srv);
                srv.run().await;
            } // #[cfg(feature = "tls")]
              // ListenAddr::Tls(addr, config) => {
              //     let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
              //     let sock = TcpListener::bind(addr).await?;
              //     let sock = tls::RustlsTcpListener::new(sock, acceptor);
              //     let mut conn_config = ConnectionConfig::new();
              //     conn_config.set_max_queued_responses(10000);
              //     let mut config = stream::Config::new();
              //     config.set_connection_config(conn_config);
              //     let srv = StreamServer::with_config(sock, buf, svc, config);
              //     let srv = Arc::new(srv);
              //     srv.run().await;
              // }
        }
        Ok(())
    }
}

async fn get_zone_serial(apex_name: Name<Bytes>, zone: &Zone) -> Option<Serial> {
    if let Ok(answer) = zone.read().query(apex_name, Rtype::SOA) {
        if let AnswerContent::Data(rrset) = answer.content() {
            if let Some(rr) = rrset.first() {
                if let ZoneRecordData::Soa(soa) = rr.data() {
                    return Some(soa.serial());
                }
            }
        }
    }
    None
}

async fn load_file_into_zone(zone_name: &StoredName, zone_path: &str) -> Result<Zone, Terminated> {
    let before = Instant::now();
    info!("[ZL]: Loading primary zone '{zone_name}' from '{zone_path}'..",);
    let mut zone_file = File::open(zone_path)
        .inspect_err(|err| error!("[ZL]: Error: Failed to open zone file '{zone_path}': {err}",))
        .map_err(|_| Terminated)?;
    let zone_file_len = zone_file
        .metadata()
        .inspect_err(|err| {
            error!("[ZL]: Error: Failed to read metadata for file '{zone_path}': {err}",)
        })
        .map_err(|_| Terminated)?
        .len();
    let mut buf = inplace::Zonefile::with_capacity(zone_file_len as usize).writer();
    std::io::copy(&mut zone_file, &mut buf)
        .inspect_err(|err| {
            error!("[ZL]: Error: Failed to read data from file '{zone_path}': {err}",)
        })
        .map_err(|_| Terminated)?;
    let reader = buf.into_inner();
    let res = Zone::try_from(reader);
    let Ok(zone) = res else {
        let errors = res.unwrap_err();
        let mut msg = format!("Failed to parse zone: {} errors", errors.len());
        for (name, err) in errors.into_iter() {
            msg.push_str(&format!("  {name}: {err}\n"));
        }
        error!("[ZL]: Error parsing zone '{zone_name}': {msg}");
        return Err(Terminated);
    };
    info!(
        "Loaded {zone_file_len} bytes from '{zone_path}' in {} secs",
        before.elapsed().as_secs()
    );
    Ok(zone)
}

fn my_noop_service(_request: Request<Vec<u8>, ()>, _meta: ()) -> ServiceResult<Vec<u8>> {
    Err(ServiceError::Refused)
}

//------------- NotifyOnWriteZone --------------------------------------------

#[derive(Debug)]
pub struct NotifyOnWriteZone {
    store: Arc<dyn ZoneStore>,
    sender: Sender<(StoredName, Serial)>,
}

impl NotifyOnWriteZone {
    pub fn new(zone: Zone, sender: Sender<(StoredName, Serial)>) -> Self {
        Self {
            store: zone.into_inner(),
            sender,
        }
    }
}

impl ZoneStore for NotifyOnWriteZone {
    fn class(&self) -> Class {
        self.store.class()
    }

    fn apex_name(&self) -> &StoredName {
        self.store.apex_name()
    }

    fn read(self: Arc<Self>) -> Box<dyn ReadableZone> {
        self.store.clone().read()
    }

    fn write(
        self: Arc<Self>,
    ) -> Pin<Box<dyn Future<Output = Box<dyn WritableZone>> + Send + Sync>> {
        let fut = self.store.clone().write();
        Box::pin(async move {
            let writable_zone = fut.await;
            let writable_zone = NotifyOnCommitZone {
                writable_zone,
                store: self.store.clone(),
                sender: self.sender.clone(),
            };
            Box::new(writable_zone) as Box<dyn WritableZone>
        })
    }

    fn as_any(&self) -> &dyn Any {
        self as &dyn Any
    }
}

struct NotifyOnCommitZone {
    writable_zone: Box<dyn WritableZone>,
    store: Arc<dyn ZoneStore>,
    sender: Sender<(StoredName, Serial)>,
}

impl WritableZone for NotifyOnCommitZone {
    fn open(
        &self,
        create_diff: bool,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn WritableZoneNode>, std::io::Error>> + Send + Sync>,
    > {
        self.writable_zone.open(create_diff)
    }

    fn commit(
        &mut self,
        bump_soa_serial: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<InMemoryZoneDiff>, std::io::Error>> + Send + Sync>>
    {
        let fut = self.writable_zone.commit(bump_soa_serial);
        let store = self.store.clone();
        let sender = self.sender.clone();

        Box::pin(async move {
            let res = fut.await;
            let zone_name = store.apex_name().clone();
            match store
                .read()
                .query_async(zone_name.clone(), Rtype::SOA)
                .await
            {
                Ok(answer) if answer.rcode() == Rcode::NOERROR => {
                    let soa_data = answer.content().first().map(|(_ttl, data)| data);
                    if let Some(ZoneRecordData::Soa(soa)) = soa_data {
                        let zone_serial = soa.serial();
                        debug!("Notifying that zone '{zone_name}' has been committed at serial {zone_serial}");
                        sender.send((zone_name.clone(), zone_serial)).await.unwrap();
                    } else {
                        error!("Failed to query SOA of zone {zone_name} after commit: invalid SOA found");
                    }
                }
                Ok(answer) => error!(
                    "Failed to query SOA of zone {zone_name} after commit: rcode {}",
                    answer.rcode()
                ),
                Err(_) => {
                    error!("Failed to query SOA of zone {zone_name} after commit: out of zone.")
                }
            }
            res
        })
    }
}
