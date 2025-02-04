use core::fmt;
use core::future::ready;

use std::any::Any;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::ops::ControlFlow;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use domain::base::iana::{Class, Rcode};
use domain::base::wire::Composer;
use domain::base::{Name, Rtype, Serial};
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
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, WritableZone, WritableZoneNode, Zone, ZoneBuilder,
    ZoneStore, ZoneTree,
};
use futures::{
    future::{select, Either},
    pin_mut, Future,
};
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use non_empty_vec::NonEmpty;
use serde::Deserialize;
use serde_with::{serde_as, DeserializeFromStr, DisplayFromStr};
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{Receiver, Sender},
        Mutex, RwLock,
    },
    time::sleep,
};
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;

use crate::common::xfr::parse_xfr_acl;
use crate::http::PercentDecodedPath;
use crate::zonemaintenance::maintainer::{Config, ZoneLookup};
use crate::{
    common::tsig::{parse_key_strings, TsigKeyStore},
    http::ProcessRequest,
};
use crate::{
    common::{
        frim::FrimMap,
        net::{
            ListenAddr, StandardTcpListenerFactory, StandardTcpStream, TcpListener,
            TcpListenerFactory, TcpStreamWrapper,
        },
        status_reporter::{sr_log, AnyStatusReporter, Chainable, Named, UnitStatusReporter},
        unit::UnitActivity,
    },
    comms::{
        AnyDirectUpdate, DirectLink, DirectUpdate, Gate, GateMetrics, GateStatus, GraphStatus,
        Terminated,
    },
    log::ExitError,
    manager::{Component, WaitPoint},
    metrics::{self, util::append_per_router_metric, Metric, MetricType, MetricUnit},
    payload::Update,
    tokio::TokioTaskMetrics,
    tracing::Tracer,
    units::Unit,
    zonemaintenance::{
        maintainer::{DefaultConnFactory, TypedZone, ZoneMaintainer},
        types::{
            CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
            ZoneMaintainerKeyStore,
        },
    },
};
use domain::rdata::ZoneRecordData;
use domain::tsig::KeyStore;

#[serde_as]
#[derive(Clone, Debug, Deserialize)]
pub struct ZoneLoaderUnit {
    /// The relative path at which we should listen for HTTP query API requests
    #[serde(default = "ZoneLoaderUnit::default_http_api_path")]
    http_api_path: Arc<String>,

    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// The zone names and (if primary) corresponding zone file paths to load.
    pub zones: Arc<HashMap<String, String>>,

    /// XFR in per zone: Allow NOTIFY from, and when with a port also request XFR from.
    #[serde(default)]
    pub xfr_in: Arc<HashMap<String, String>>,

    /// TSIG keys.
    #[serde(default)]
    pub tsig_keys: HashMap<String, String>,
}

impl ZoneLoaderUnit {
    pub async fn run(
        self,
        mut component: Component,
        gate: Gate,
        mut waitpoint: WaitPoint,
    ) -> Result<(), Terminated> {
        let unit_name = component.name().clone();

        // Setup our metrics
        let metrics = Arc::new(ZoneLoaderMetrics::new(&gate));
        component.register_metrics(metrics.clone());

        // Setup our status reporting
        let status_reporter = Arc::new(ZoneLoaderStatusReporter::new(&unit_name, metrics.clone()));

        let (zone_updated_tx, zone_updated_rx) = tokio::sync::mpsc::channel(10);

        let mut notify_cfg = NotifyConfig { tsig_key: None };

        let mut xfr_cfg = XfrConfig {
            strategy: XfrStrategy::IxfrWithAxfrFallback,
            ixfr_transport: TransportStrategy::Tcp,
            compatibility_mode: CompatibilityMode::Default,
            tsig_key: None,
        };

        for (key_name, opt_alg_and_hex_bytes) in self.tsig_keys.iter() {
            let key = parse_key_strings(key_name, opt_alg_and_hex_bytes).map_err(|err| {
                error!(
                    "[{}]: Failed to parse TSIG key '{key_name}': {err}",
                    component.name()
                );
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

        for (zone_name, zone_path) in self.zones.iter() {
            let mut zone = if !zone_path.is_empty() {
                // Load the specified zone file.
                info!(
                    "[{}]: Loading primary zone '{zone_name}' from '{zone_path}'..",
                    component.name()
                );
                let mut zone_bytes = File::open(zone_path)
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Failed to open zone file at '{zone_path}': {err}",
                            component.name()
                        )
                    })
                    .map_err(|_| Terminated)?;
                let reader = inplace::Zonefile::load(&mut zone_bytes)
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Failed to load zone file from '{zone_path}': {err}",
                            component.name()
                        )
                    })
                    .map_err(|_| Terminated)?;
                let res = Zone::try_from(reader);
                let Ok(zone) = res else {
                    let errors = res.unwrap_err();
                    let mut msg = format!("Failed to parse zone: {} errors", errors.len());
                    for (name, err) in errors.into_iter() {
                        msg.push_str(&format!("  {name}: {err}\n"));
                    }
                    error!(
                        "[{}]: Error parsing zone '{zone_name}': {}",
                        component.name(),
                        msg
                    );
                    return Err(Terminated);
                };
                zone
            } else {
                let apex_name = Name::from_str(zone_name)
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Invalid zone name '{zone_name}': {err}",
                            component.name()
                        )
                    })
                    .map_err(|_| Terminated)?;
                info!(
                    "[{}]: Adding secondary zone '{zone_name}'",
                    component.name()
                );
                let builder = ZoneBuilder::new(apex_name, Class::IN);
                builder.build()
            };

            let mut zone_cfg = ZoneConfig::new();

            if let Some(xfr_in) = self.xfr_in.get(zone_name) {
                let src = parse_xfr_acl(
                    xfr_in,
                    &mut xfr_cfg,
                    &mut notify_cfg,
                    component.tsig_key_store(),
                )
                .map_err(|_| {
                    error!("[{}]: Error parsing XFR ACL", component.name());
                    Terminated
                })?;

                info!(
                    "[{}]: Allowing NOTIFY from {} for zone '{zone_name}'",
                    component.name(),
                    src.ip()
                );
                zone_cfg
                    .allow_notify_from
                    .add_src(src.ip(), notify_cfg.clone());
                if src.port() != 0 {
                    info!(
                        "[{}]: Adding XFR primary {src} for zone '{zone_name}'",
                        component.name()
                    );
                    zone_cfg.request_xfr_from.add_dst(src, xfr_cfg.clone());
                }
            }

            let notify_on_write_zone = NotifyOnWriteZone::new(zone, zone_updated_tx.clone());
            zone = Zone::new(notify_on_write_zone);

            let zone = TypedZone::new(zone, zone_cfg);
            zone_maintainer.insert_zone(zone).await.unwrap();
        }

        // let max_concurrency = std::thread::available_parallelism().unwrap().get() / 2;

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
            info!("[{}]: Binding on {:?}", component.name(), addr);
            let svc = svc.clone();
            let component_name = component.name().clone();
            tokio::spawn(async move {
                if let Err(err) = Self::server(addr, svc).await {
                    error!("[{}]: {}", component_name, err);
                }
            });
        }

        // Setup REST API endpoint
        let http_processor = Arc::new(ZoneListApi::new(
            self.http_api_path.clone(),
            self.zones.clone(),
            self.xfr_in.clone(),
            zone_maintainer.clone(),
        ));
        component.register_http_resource(http_processor.clone(), &self.http_api_path);

        // Wait for other components to be, and signal to other components
        // that we are, ready to start. All units and targets start together,
        // otherwise data passed from one component to another may be lost if
        // the receiving component is not yet ready to accept it.
        gate.process_until(waitpoint.ready()).await?;

        let zone_maintainer_clone = zone_maintainer.clone();
        tokio::spawn(async move { zone_maintainer_clone.run().await });

        // Signal again once we are out of the process_until() so that anyone
        // waiting to send important gate status updates won't send them while
        // we are in process_until() which will just eat them without handling
        // them.
        waitpoint.running().await;

        let component = Arc::new(RwLock::new(component));

        ZoneLoader::new(
            component,
            self.http_api_path,
            gate,
            metrics,
            status_reporter,
            http_processor,
            zone_maintainer,
        )
        .run(zone_updated_rx)
        .await?;

        Ok(())
    }

    fn default_http_api_path() -> Arc<String> {
        Arc::new("/zone-loader/".to_string())
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
        let fut = self.writable_zone.open(create_diff);
        Box::pin(async move {
            let writable_zone_node = fut.await?;
            let writable_zone_node = FilteringWritableZoneNode { writable_zone_node };
            Ok(Box::new(writable_zone_node) as Box<dyn WritableZoneNode>)
        })
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
                    let soa_data = answer.content().first().map(|(ttl, data)| data);
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
                Err(err) => {
                    error!("Failed to query SOA of zone {zone_name} after commit: out of zone.")
                }
            }
            res
        })
    }
}

struct FilteringWritableZoneNode {
    writable_zone_node: Box<dyn WritableZoneNode>,
}

impl WritableZoneNode for FilteringWritableZoneNode {
    fn update_child(
        &self,
        label: &domain::base::name::Label,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn WritableZoneNode>, std::io::Error>> + Send + Sync>,
    > {
        self.writable_zone_node.update_child(label)
    }

    fn get_rrset(
        &self,
        rtype: Rtype,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<domain::zonetree::SharedRrset>, std::io::Error>>
                + Send
                + Sync,
        >,
    > {
        self.writable_zone_node.get_rrset(rtype)
    }

    fn update_rrset(
        &self,
        rrset: domain::zonetree::SharedRrset,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        // Filter out attempts to add or change DNSSEC records to/in this zone.
        match rrset.rtype() {
            Rtype::DNSKEY
            | Rtype::DS
            | Rtype::RRSIG
            | Rtype::NSEC
            | Rtype::NSEC3
            | Rtype::NSEC3PARAM => Box::pin(ready(Ok(()))),

            _ => self.writable_zone_node.update_rrset(rrset),
        }
    }

    fn remove_rrset(
        &self,
        rtype: Rtype,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        self.writable_zone_node.remove_rrset(rtype)
    }

    fn make_regular(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        self.writable_zone_node.make_regular()
    }

    fn make_zone_cut(
        &self,
        cut: domain::zonetree::types::ZoneCut,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        self.writable_zone_node.make_zone_cut(cut)
    }

    fn make_cname(
        &self,
        cname: domain::zonetree::SharedRr,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        self.writable_zone_node.make_cname(cname)
    }

    fn remove_all(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send + Sync>> {
        self.writable_zone_node.remove_all()
    }
}

//------------ ZoneLoader ----------------------------------------------------

struct ZoneLoader {
    component: Arc<RwLock<Component>>,
    #[allow(dead_code)]
    http_api_path: Arc<String>,
    gate: Gate,
    metrics: Arc<ZoneLoaderMetrics>,
    status_reporter: Arc<ZoneLoaderStatusReporter>,
    http_processor: Arc<ZoneListApi>,
    xfr_in: Arc<ZoneMaintainer<TsigKeyStore, DefaultConnFactory>>,
}

impl ZoneLoader {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Arc<RwLock<Component>>,
        http_api_path: Arc<String>,
        gate: Gate,
        metrics: Arc<ZoneLoaderMetrics>,
        status_reporter: Arc<ZoneLoaderStatusReporter>,
        http_processor: Arc<ZoneListApi>,
        xfr_in: Arc<ZoneMaintainer<TsigKeyStore, DefaultConnFactory>>,
    ) -> Self {
        Self {
            component,
            http_api_path,
            gate,
            metrics,
            status_reporter,
            http_processor,
            xfr_in,
        }
    }

    async fn run(
        self,
        mut zone_updated_rx: Receiver<(StoredName, Serial)>,
    ) -> Result<(), crate::comms::Terminated> {
        let status_reporter = self.status_reporter.clone();

        let arc_self = Arc::new(self);

        loop {
            //     status_reporter.listener_listening(&listen_addr.to_string());

            match arc_self.clone().process_until(zone_updated_rx.recv()).await {
                ControlFlow::Continue(Some((zone_name, zone_serial))) => {
                    // status_reporter
                    //     .listener_connection_accepted(client_addr);

                    info!(
                        "[{}]: Received a new copy of zone '{zone_name}' at serial {zone_serial}",
                        arc_self.component.read().await.name(),
                    );

                    arc_self
                        .gate
                        .update_data(Update::UnsignedZoneUpdatedEvent {
                            zone_name,
                            zone_serial,
                        })
                        .await;
                }
                ControlFlow::Continue(None) | ControlFlow::Break(Terminated) => {
                    return Err(Terminated)
                }
            }
        }
    }

    async fn process_until<T, U>(
        self: Arc<Self>,
        until_fut: T,
    ) -> ControlFlow<Terminated, Option<U>>
    where
        T: Future<Output = Option<U>>,
    {
        let mut until_fut = Box::pin(until_fut);

        loop {
            let process_fut = self.gate.process();
            pin_mut!(process_fut);

            match select(process_fut, until_fut).await {
                Either::Left((Err(Terminated), _)) => {
                    self.status_reporter.terminated();
                    return ControlFlow::Break(Terminated);
                }
                Either::Left((Ok(status), next_fut)) => {
                    self.status_reporter.gate_status_announced(&status);
                    match status {
                        GateStatus::Reconfiguring {
                            new_config:
                                Unit::ZoneLoader(ZoneLoaderUnit {
                                    http_api_path,
                                    listen,
                                    zones,
                                    xfr_in,
                                    tsig_keys,
                                }),
                        } => {
                            // Runtime reconfiguration of this unit has
                            // been requested. New connections will be
                            // handled using the new configuration,
                            // existing connections handled by
                            // router_handler() tasks will receive their
                            // own copy of this Reconfiguring status
                            // update and can react to it accordingly.
                            // let rebind = self.listen != new_listen;

                            // self.listen = new_listen;
                            // self.filter_name.store(new_filter_name.into());
                            // self.router_id_template
                            //     .store(new_router_id_template.into());
                            // self.tracing_mode.store(new_tracing_mode.into());

                            // if rebind {
                            //     // Trigger re-binding to the new listen port.
                            //     let err = std::io::ErrorKind::Other;
                            //     return ControlFlow::Continue(
                            //         Err(err.into()),
                            //     );
                            // }
                        }

                        GateStatus::ReportLinks { report } => {
                            report.declare_source();
                            report.set_graph_status(self.metrics.clone());
                        }

                        GateStatus::ApplicationCommand { cmd } => {
                            info!(
                                "[{}] Received command: {cmd:?}",
                                self.component.read().await.name()
                            );
                        }

                        _ => { /* Nothing to do */ }
                    }

                    until_fut = next_fut;
                }
                Either::Right((updated_zone_name, _)) => {
                    return ControlFlow::Continue(updated_zone_name);
                }
            }
        }
    }
}

impl std::fmt::Debug for ZoneLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoneLoader").finish()
    }
}

#[async_trait]
impl DirectUpdate for ZoneLoader {
    async fn direct_update(&self, event: Update) {
        info!(
            "[{}]: Received event: {event:?}",
            self.component.read().await.name()
        );
    }
}

impl AnyDirectUpdate for ZoneLoader {}

//------------ ZoneLoaderMetrics ---------------------------------------------

#[derive(Debug, Default)]
pub struct ZoneLoaderMetrics {
    gate: Option<Arc<GateMetrics>>, // optional to make testing easier
}

impl GraphStatus for ZoneLoaderMetrics {
    fn status_text(&self) -> String {
        "TODO".to_string()
    }

    fn okay(&self) -> Option<bool> {
        Some(false)
    }
}

impl ZoneLoaderMetrics {
    // const LISTENER_BOUND_COUNT_METRIC: Metric = Metric::new(
    //     "bmp_tcp_in_listener_bound_count",
    //     "the number of times the TCP listen port was bound to",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
}

impl ZoneLoaderMetrics {
    pub fn new(gate: &Gate) -> Self {
        Self {
            gate: Some(gate.metrics()),
        }
    }
}

impl metrics::Source for ZoneLoaderMetrics {
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

//------------ ZoneLoaderStatusReporter --------------------------------------

#[derive(Debug, Default)]
pub struct ZoneLoaderStatusReporter {
    name: String,
    metrics: Arc<ZoneLoaderMetrics>,
}

impl ZoneLoaderStatusReporter {
    pub fn new<T: Display>(name: T, metrics: Arc<ZoneLoaderMetrics>) -> Self {
        Self {
            name: format!("{}", name),
            metrics,
        }
    }

    pub fn _typed_metrics(&self) -> Arc<ZoneLoaderMetrics> {
        self.metrics.clone()
    }
}

impl UnitStatusReporter for ZoneLoaderStatusReporter {}

impl AnyStatusReporter for ZoneLoaderStatusReporter {
    fn metrics(&self) -> Option<Arc<dyn crate::metrics::Source>> {
        Some(self.metrics.clone())
    }
}

impl Chainable for ZoneLoaderStatusReporter {
    fn add_child<T: Display>(&self, child_name: T) -> Self {
        Self::new(self.link_names(child_name), self.metrics.clone())
    }
}

impl Named for ZoneLoaderStatusReporter {
    fn name(&self) -> &str {
        &self.name
    }
}

//------------ ZoneListApi ---------------------------------------------------

struct ZoneListApi {
    http_api_path: Arc<String>,
    zones: Arc<HashMap<String, String>>,
    xfr_in: Arc<HashMap<String, String>>,
    zone_maintainer: Arc<ZoneMaintainer<TsigKeyStore, DefaultConnFactory>>,
}

impl ZoneListApi {
    fn new(
        http_api_path: Arc<String>,
        zones: Arc<HashMap<String, String>>,
        xfr_in: Arc<HashMap<String, String>>,
        zone_maintainer: Arc<ZoneMaintainer<TsigKeyStore, DefaultConnFactory>>,
    ) -> Self {
        Self {
            http_api_path,
            zones,
            xfr_in,
            zone_maintainer,
        }
    }
}

#[async_trait]
impl ProcessRequest for ZoneListApi {
    async fn process_request(
        &self,
        request: &hyper::Request<hyper::Body>,
    ) -> Option<hyper::Response<hyper::Body>> {
        let req_path = request.uri().decoded_path();
        if request.method() == hyper::Method::GET && req_path == *self.http_api_path {
            Some(self.build_response().await)
        } else {
            None
        }
    }
}

impl ZoneListApi {
    pub async fn build_response(&self) -> hyper::Response<hyper::Body> {
        let mut response_body = self.build_response_header();

        self.build_response_body(&mut response_body).await;

        self.build_response_footer(&mut response_body);

        hyper::Response::builder()
            .header("Content-Type", "text/html")
            .body(hyper::Body::from(response_body))
            .unwrap()
    }

    fn build_response_header(&self) -> String {
        formatdoc! {
            r#"
            <!DOCTYPE html>
            <html lang="en">
                <head>
                  <meta charset="UTF-8">
                </head>
                <body>
                <pre>Showing {num_zones} monitored zones:
            "#,
            num_zones = self.zones.len()
        }
    }

    async fn build_response_body(&self, response_body: &mut String) {
        for zone_name in self.zones.keys() {
            if let Ok(zone_name) = Name::from_str(zone_name) {
                if let Ok(report) = self
                    .zone_maintainer
                    .zone_status(&zone_name, Class::IN)
                    .await
                {
                    response_body.push_str(&format!("\n{report}"));
                }
            }
            if let Some(xfr_in) = self.xfr_in.get(zone_name) {
                response_body.push_str(&format!("        source: {xfr_in}"));
            }
        }
    }

    fn build_response_footer(&self, response_body: &mut String) {
        response_body.push_str("    </pre>\n");
        response_body.push_str("  </body>\n");
        response_body.push_str("</html>\n");
    }
}
