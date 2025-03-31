use core::fmt;
use core::future::{ready, Ready};

use std::any::Any;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::marker::Sync;
use std::net::{IpAddr, SocketAddr};
use std::ops::{ControlFlow, Deref};
use std::pin::Pin;
use std::process::Command;
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
use domain::base::Name;
use domain::base::{Serial, ToName};
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::{self, DgramServer};
use domain::net::server::message::Request;
use domain::net::server::middleware::cookies::CookiesMiddlewareSvc;
use domain::net::server::middleware::edns::EdnsMiddlewareSvc;
use domain::net::server::middleware::mandatory::MandatoryMiddlewareSvc;
use domain::net::server::middleware::notify::{Notifiable, NotifyMiddlewareSvc};
use domain::net::server::middleware::tsig::TsigMiddlewareSvc;
use domain::net::server::middleware::xfr::XfrMiddlewareSvc;
use domain::net::server::middleware::xfr::{XfrData, XfrDataProvider, XfrDataProviderError};
use domain::net::server::service::{CallResult, Service, ServiceError, ServiceResult};
use domain::net::server::stream::{self, StreamServer};
use domain::net::server::util::mk_builder_for_target;
use domain::net::server::util::{mk_error_response, service_fn};
use domain::net::server::ConnectionConfig;
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace;
use domain::zonetree::types::EmptyZoneDiff;
use domain::zonetree::Answer;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, WritableZone, WritableZoneNode, Zone, ZoneBuilder,
    ZoneStore, ZoneTree,
};
use futures::future::{select, Either};
use futures::stream::{once, Once};
use futures::{pin_mut, Future};
use hyper::StatusCode;
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use non_empty_vec::NonEmpty;
use octseq::Octets;
use serde::Deserialize;
use serde_with::{serde_as, DeserializeFromStr, DisplayFromStr};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;
use uuid::Uuid;

use crate::common::frim::FrimMap;
use crate::common::net::{
    ListenAddr, StandardTcpListenerFactory, StandardTcpStream, TcpListener, TcpListenerFactory,
    TcpStreamWrapper,
};
use crate::common::status_reporter::{
    sr_log, AnyStatusReporter, Chainable, Named, UnitStatusReporter,
};
use crate::common::tsig::{parse_key_strings, TsigKeyStore};
use crate::common::unit::UnitActivity;
use crate::common::xfr::parse_xfr_acl;
use crate::comms::ApplicationCommand;
use crate::comms::{
    AnyDirectUpdate, DirectLink, DirectUpdate, Gate, GateMetrics, GateStatus, GraphStatus,
    Terminated,
};
use crate::http::{PercentDecodedPath, ProcessRequest};
use crate::log::ExitError;
use crate::manager::{Component, WaitPoint};
use crate::metrics::{self, util::append_per_router_metric, Metric, MetricType, MetricUnit};
use crate::payload::Update;
use crate::tokio::TokioTaskMetrics;
use crate::tracing::Tracer;
use crate::units::Unit;
use crate::zonemaintenance::maintainer::{DefaultConnFactory, TypedZone, ZoneMaintainer};
use crate::zonemaintenance::types::TsigKey;
use crate::zonemaintenance::types::{
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore,
};

#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub enum Mode {
    #[serde(alias = "prepublish")]
    #[serde(alias = "prepub")]
    Prepublish,

    #[serde(alias = "publish")]
    #[serde(alias = "pub")]
    Publish,
}

#[allow(clippy::enum_variant_names)]
#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub enum Source {
    #[serde(alias = "unsigned")]
    UnsignedZones,

    #[serde(alias = "signed")]
    SignedZones,

    #[serde(alias = "published")]
    PublishedZones,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ZoneServerUnit {
    /// The relative path at which we should listen for HTTP query API requests
    #[serde(default = "ZoneServerUnit::default_http_api_path")]
    http_api_path: Arc<String>,

    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// XFR out per zone: Allow XFR to, and when with a port also send NOTIFY to.
    pub xfr_out: HashMap<String, String>,

    pub hooks: Vec<String>,

    pub mode: Mode,

    pub source: Source,
}

impl ZoneServerUnit {
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

        // TODO: This will just choose all current zones to be served. For signed and published
        // zones this doesn't matter so much as they only exist while being and once approved.
        // But for unsigned zones the zone could be updated whilst being reviewed and we only
        // serve the latest version of the zone, not the specific serial being reviewed!
        let zones = match self.source {
            Source::UnsignedZones => component.unsigned_zones().clone(),
            Source::SignedZones => component.signed_zones().clone(),
            Source::PublishedZones => component.published_zones().clone(),
        };

        let max_concurrency = std::thread::available_parallelism().unwrap().get() / 2;

        // TODO: Pass xfr_out to XfrDataProvidingZonesWrapper for enforcement.
        let zones = XfrDataProvidingZonesWrapper {
            zones,
            key_store: component.tsig_key_store().clone(),
        };

        // let svc = ZoneServerService::new(zones.clone());
        let svc = service_fn(zone_server_service, zones.clone());
        let svc = XfrMiddlewareSvc::new(svc, zones.clone(), max_concurrency);
        let svc = NotifyMiddlewareSvc::new(svc, zones.clone());
        let svc = TsigMiddlewareSvc::new(svc, component.tsig_key_store().clone());
        let svc = CookiesMiddlewareSvc::with_random_secret(svc);
        let svc = EdnsMiddlewareSvc::new(svc);
        let svc = MandatoryMiddlewareSvc::<_, _, ()>::new(svc);
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

        let component = Arc::new(RwLock::new(component));

        ZoneServer::new(
            component,
            self.http_api_path,
            gate,
            metrics,
            status_reporter,
            self.mode,
            self.source,
            self.hooks,
            self.listen,
            zones,
        )
        .run()
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
                let srv = DgramServer::<_, _, _>::with_config(sock, buf, svc, config);
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

//------------ ZoneServer ----------------------------------------------------

struct ZoneServer {
    component: Arc<RwLock<Component>>,
    #[allow(dead_code)]
    http_api_path: Arc<String>,
    gate: Gate,
    metrics: Arc<ZoneLoaderMetrics>,
    status_reporter: Arc<ZoneLoaderStatusReporter>,
    #[allow(dead_code)]
    mode: Mode,
    source: Source,
    hooks: Vec<String>,
    listen: Vec<ListenAddr>,
    #[allow(clippy::type_complexity)]
    pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
    zones: XfrDataProvidingZonesWrapper,
}

impl ZoneServer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Arc<RwLock<Component>>,
        http_api_path: Arc<String>,
        gate: Gate,
        metrics: Arc<ZoneLoaderMetrics>,
        status_reporter: Arc<ZoneLoaderStatusReporter>,
        mode: Mode,
        source: Source,
        hooks: Vec<String>,
        listen: Vec<ListenAddr>,
        zones: XfrDataProvidingZonesWrapper,
    ) -> Self {
        Self {
            component,
            http_api_path,
            gate,
            metrics,
            status_reporter,
            mode,
            source,
            hooks,
            pending_approvals: Default::default(),
            listen,
            zones,
        }
    }

    async fn run(self) -> Result<(), crate::comms::Terminated> {
        let status_reporter = self.status_reporter.clone();

        // Setup REST API endpoint
        let http_processor = Arc::new(ZoneReviewApi::new(
            self.http_api_path.clone(),
            self.gate.clone(),
            self.pending_approvals.clone(),
            self.zones.clone(),
            self.mode,
            self.source,
            self.listen.clone(),
        ));
        self.component
            .write()
            .await
            .register_http_resource(http_processor.clone(), &self.http_api_path);

        let arc_self = Arc::new(self);

        loop {
            //     status_reporter.listener_listening(&listen_addr.to_string());

            match arc_self
                .clone()
                .process_until(core::future::pending::<()>())
                .await
            {
                ControlFlow::Continue(()) => {
                    todo!()
                }
                ControlFlow::Break(Terminated) => return Err(Terminated),
            }
        }
    }

    async fn process_until<T, U>(self: Arc<Self>, until_fut: T) -> ControlFlow<Terminated, U>
    where
        T: Future<Output = U>,
    {
        let mut until_fut = Box::pin(until_fut);
        let component_name = self.component.read().await.name().clone();

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
                                Unit::ZoneServer(ZoneServerUnit {
                                    http_api_path,
                                    listen,
                                    xfr_out,
                                    mode,
                                    source,
                                    hooks,
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
                            info!("[{component_name}] Received command: {cmd:?}",);
                            match &cmd {
                                ApplicationCommand::SeekApprovalForUnsignedZone {
                                    zone_name,
                                    zone_serial,
                                }
                                | ApplicationCommand::SeekApprovalForSignedZone {
                                    zone_name,
                                    zone_serial,
                                } => {
                                    let zone_type = match cmd {
                                        ApplicationCommand::SeekApprovalForUnsignedZone {
                                            ..
                                        } => "unsigned",
                                        ApplicationCommand::SeekApprovalForSignedZone {
                                            ..
                                        } => "signed",
                                        _ => unreachable!(),
                                    };
                                    info!("[{component_name}]: Seeking approval for {zone_type} zone '{zone_name}' at serial {zone_serial}.");
                                    for hook in &self.hooks {
                                        let approval_token = Uuid::new_v4();
                                        info!("[{component_name}]: Generated approval token '{approval_token}' for {zone_type} zone '{zone_name}' at serial {zone_serial}.");

                                        self.pending_approvals
                                            .write()
                                            .await
                                            .entry((zone_name.clone(), *zone_serial))
                                            .and_modify(|e| e.push(approval_token))
                                            .or_insert(vec![approval_token]);

                                        match Command::new(hook)
                                            .arg(format!("{zone_name}"))
                                            .arg(format!("{zone_serial}"))
                                            .arg(format!("{approval_token}"))
                                            .spawn()
                                        {
                                            Ok(_) => {
                                                info!("[{component_name}]: Executed hook '{hook}' for {zone_type} zone '{zone_name}' at serial {zone_serial}");
                                                info!("[{component_name}]: Confirm with HTTP GET {}approve/{approval_token}?zone={zone_name}&serial={zone_serial}", self.http_api_path);
                                                info!("[{component_name}]: Reject with HTTP GET {}reject/{approval_token}?zone={zone_name}&serial={zone_serial}", self.http_api_path);
                                            }
                                            Err(err) => {
                                                error!(
                                                    "[{component_name}]: Failed to execute hook '{hook}' for {zone_type} zone '{zone_name}' at serial {zone_serial}: {err}",
                                                );
                                            }
                                        }
                                    }
                                }

                                ApplicationCommand::PublishSignedZone {
                                    zone_name,
                                    zone_serial,
                                } => {
                                    info!(
                                        "[{component_name}]: Publishing signed zone '{zone_name}' at serial {zone_serial}."                                        
                                    );
                                    // Move the zone from the signed collection to the published collection.
                                    // TODO: Bump the zone serial?
                                    let component = self.component.write().await;
                                    let signed_zones = component.signed_zones().load();
                                    if let Some(zone) = signed_zones.get_zone(zone_name, Class::IN)
                                    {
                                        let published_zones = component.published_zones().load();

                                        // Create a deep copy of the set of
                                        // published zones. We will add the
                                        // new zone to that copied set and
                                        // then replace the original set with
                                        // the new set.
                                        info!("[{component_name}]: Adding '{zone_name}' to the set of published zones.");
                                        let mut new_published_zones =
                                            Arc::unwrap_or_clone(published_zones.clone());
                                        let _ = new_published_zones.remove_zone(zone.apex_name(), zone.class());
                                        new_published_zones.insert_zone(zone.clone()).unwrap();
                                        component
                                            .published_zones()
                                            .store(Arc::new(new_published_zones));

                                        // Create a deep copy of the set of
                                        // signed zones. We will remove the
                                        // zone from the copied set and then
                                        // replace the original set with the
                                        // new set.
                                        info!("[{component_name}]: Removing '{zone_name}' from the set of signed zones.");
                                        let mut new_signed_zones =
                                            Arc::unwrap_or_clone(signed_zones.clone());
                                        new_signed_zones.remove_zone(zone_name, Class::IN).unwrap();
                                        component.signed_zones().store(Arc::new(new_signed_zones));
                                    }
                                }

                                _ => { /* Not for us */ }
                            }
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

impl std::fmt::Debug for ZoneServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoneLoader").finish()
    }
}

#[async_trait]
impl DirectUpdate for ZoneServer {
    async fn direct_update(&self, event: Update) {
        info!(
            "[{}]: Received event: {event:?}",
            self.component.read().await.name()
        );
    }
}

impl AnyDirectUpdate for ZoneServer {}

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

#[derive(Clone, Default)]
struct XfrDataProvidingZonesWrapper {
    zones: Arc<ArcSwap<ZoneTree>>,
    key_store: TsigKeyStore,
}

impl Notifiable for XfrDataProvidingZonesWrapper {
    fn notify_zone_changed(
        &self,
        _class: Class,
        _apex_name: &Name<Bytes>,
        _source: IpAddr,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<(), domain::net::server::middleware::notify::NotifyError>>
                + Sync
                + Send
                + '_,
        >,
    > {
        // TODO: Should we check here if we have the zone?
        Box::pin(ready(Ok(())))
    }
}

impl XfrDataProvider<Option<<TsigKeyStore as KeyStore>::Key>> for XfrDataProvidingZonesWrapper {
    type Diff = EmptyZoneDiff;

    fn request<Octs>(
        &self,
        req: &Request<Octs, Option<<TsigKeyStore as KeyStore>::Key>>,
        _diff_from: Option<domain::base::Serial>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        domain::net::server::middleware::xfr::XfrData<Self::Diff>,
                        domain::net::server::middleware::xfr::XfrDataProviderError,
                    >,
                > + Sync
                + Send
                + '_,
        >,
    >
    where
        Octs: octseq::Octets + Send + Sync,
    {
        let res = req
            .message()
            .sole_question()
            .map_err(XfrDataProviderError::ParseError)
            .and_then(|q| {
                if let Some(zone) = self.zones.load().find_zone(q.qname(), q.qclass()) {
                    Ok(XfrData::new(zone.clone(), vec![], false))
                } else {
                    Err(XfrDataProviderError::UnknownZone)
                }
            });

        Box::pin(ready(res))
    }
}

impl KeyStore for XfrDataProvidingZonesWrapper {
    type Key = Key;

    fn get_key<N: ToName>(&self, name: &N, algorithm: Algorithm) -> Option<Self::Key> {
        self.key_store.get_key(name, algorithm)
    }
}

#[derive(Clone)]
struct ZoneServerService {
    #[allow(dead_code)]
    zones: XfrDataProvidingZonesWrapper,
}

impl ZoneServerService {
    #[allow(dead_code)]
    fn new(zones: XfrDataProvidingZonesWrapper) -> Self {
        Self { zones }
    }
}

fn zone_server_service(
    request: Request<Vec<u8>, Option<<TsigKeyStore as KeyStore>::Key>>,
    zones: XfrDataProvidingZonesWrapper,
) -> ServiceResult<Vec<u8>> {
    let question = request.message().sole_question().unwrap();
    let zone = zones
        .zones
        .load()
        .find_zone(question.qname(), question.qclass())
        .map(|zone| zone.read());
    let answer = match zone {
        Some(zone) => {
            let qname = question.qname().to_bytes();
            let qtype = question.qtype();
            zone.query(qname, qtype).unwrap()
        }
        None => Answer::new(Rcode::NXDOMAIN),
    };

    let builder = mk_builder_for_target();
    let additional = answer.to_message(request.message(), builder);
    Ok(CallResult::new(additional))
}

//------------ ZoneReviewApi -------------------------------------------------

struct ZoneReviewApi {
    http_api_path: Arc<String>,
    gate: Gate,
    #[allow(clippy::type_complexity)]
    pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
    zones: XfrDataProvidingZonesWrapper,
    mode: Mode,
    source: Source,
    listen: Vec<ListenAddr>,
}

impl ZoneReviewApi {
    #[allow(clippy::type_complexity)]
    fn new(
        http_api_path: Arc<String>,
        gate: Gate,
        pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
        zones: XfrDataProvidingZonesWrapper,
        mode: Mode,
        source: Source,
        listen: Vec<ListenAddr>,
    ) -> Self {
        Self {
            http_api_path,
            gate,
            pending_approvals,
            zones,
            mode,
            source,
            listen,
        }
    }
}

// API: GET /<http api path>/{approve,reject}/<approval token>?zone=<zone name>&serial=<zone serial>
//
// NOTE: We use query parameters for the zone details because dots that appear in zone names are
// decoded specially by HTTP standards compliant libraries, especially occurences of handling of /./
// are problematic as that gets collapsed to /.
//
// TODO: Should we expire old pending approvals, e.g. a hook script failed and
// they never got approved or rejected?
#[async_trait]
impl ProcessRequest for ZoneReviewApi {
    async fn process_request(
        &self,
        request: hyper::Request<hyper::Body>,
    ) -> ControlFlow<hyper::Request<hyper::Body>, hyper::Response<hyper::Body>> {
        let req_path = request.uri().decoded_path();
        if request.method() == hyper::Method::GET && req_path == *self.http_api_path {
            return ControlFlow::Continue(self.build_status_response().await);
        } else if self.mode == Mode::Prepublish && request.method() == hyper::Method::GET // should really be POST with POST body parameters.
            && req_path.starts_with(self.http_api_path.deref())
        {
            let mut status = StatusCode::BAD_REQUEST;

            let (_base_path, remainder) = req_path.split_at(self.http_api_path.len());

            let mut parts = remainder.split('/');
            let operation = parts.next();
            let given_approval_token = parts.next();

            // We don't use Url::parse() here because it doesn't like
            // base-less URIs which is what we receive.
            if let Some(query) = request.uri().query() {
                let query_pairs_iter = url::form_urlencoded::parse(query.as_bytes());
                let query_pairs: HashMap<_, _> = HashMap::from_iter(query_pairs_iter);
                let zone_name = query_pairs.get("zone");
                let zone_serial = query_pairs.get("serial");

                // Is this a valid request?
                if matches!(operation, Some("approve") | Some("reject"))
                    && zone_name.is_some()
                    && zone_serial.is_some()
                    && given_approval_token.is_some()
                    && parts.next().is_none()
                {
                    let operation = operation.unwrap();
                    let zone_name = zone_name.unwrap();
                    let zone_serial = zone_serial.unwrap();
                    let given_approval_token = given_approval_token.unwrap();

                    // Is the zone name valid?
                    if let Ok(zone_name) = Name::<Bytes>::from_str(zone_name) {
                        // Is the zone serial valid?
                        if let Ok(zone_serial) = Serial::from_str(zone_serial) {
                            let mut remove_approvals = false;

                            // Are approvals pending for this serial of this zone?
                            if let Some(pending_approvals) = self
                                .pending_approvals
                                .write()
                                .await
                                .get_mut(&(zone_name.clone(), zone_serial))
                            {
                                // Is this a valid approval token?
                                if let Ok(given_uuid) = Uuid::from_str(given_approval_token) {
                                    if let Some(idx) = pending_approvals
                                        .iter()
                                        .position(|&uuid| uuid == given_uuid)
                                    {
                                        // For a rejection remove all pending approvals for the zone.
                                        // For an approval remove only the specified approval.
                                        match operation {
                                            "approve" => {
                                                status = StatusCode::OK;
                                                pending_approvals.remove(idx);

                                                if pending_approvals.is_empty() {
                                                    let evt_zone_name = zone_name.clone();
                                                    let (zone_type, event) = match self.source {
                                                        Source::UnsignedZones => (
                                                            "unsigned",
                                                            Update::UnsignedZoneApprovedEvent {
                                                                zone_name: evt_zone_name,
                                                                zone_serial,
                                                            },
                                                        ),
                                                        Source::SignedZones => (
                                                            "signed",
                                                            Update::SignedZoneApprovedEvent {
                                                                zone_name: evt_zone_name,
                                                                zone_serial,
                                                            },
                                                        ),
                                                        Source::PublishedZones => unreachable!(),
                                                    };
                                                    info!("Pending {zone_type} zone '{zone_name}' approved at serial {zone_serial}.");
                                                    self.gate.update_data(event).await;
                                                    remove_approvals = true;
                                                }
                                            }
                                            "reject" => {
                                                info!("Pending zone '{zone_name}' rejected at serial {zone_serial}.");
                                                status = StatusCode::OK;
                                                remove_approvals = true;
                                            }
                                            _ => unreachable!(),
                                        }
                                    } else {
                                        warn!("No pending approval found for zone name '{zone_name}' at serial {zone_serial} with approval token '{given_uuid}'.");
                                    }
                                } else {
                                    warn!(
                                        "Invalid approval token '{given_approval_token}' in request."
                                    );
                                }
                            } else {
                                debug!("No pending approvals for zone name '{zone_name}' at serial {zone_serial}.");
                            }

                            if remove_approvals {
                                self.pending_approvals
                                    .write()
                                    .await
                                    .remove(&(zone_name, zone_serial));
                            }
                        } else {
                            warn!("Invalid zone serial '{zone_serial}' in request.");
                        }
                    } else {
                        warn!("Invalid zone name '{zone_name}' in request.");
                    }
                } else {
                }
            }

            if status == StatusCode::BAD_REQUEST {
                debug!("Invalid request: {}", request.uri());
            }

            ControlFlow::Continue(
                hyper::Response::builder()
                    .status(status)
                    .body(hyper::Body::empty())
                    .unwrap(),
            )
        } else {
            ControlFlow::Break(request)
        }
    }
}

impl ZoneReviewApi {
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
        let intro = match (self.mode, self.source) {
            (Mode::Prepublish, Source::UnsignedZones) => {
                "Pre-publication review server for unsigned zones"
            }
            (Mode::Prepublish, Source::SignedZones) => {
                "Pre-publication review server for signed zones"
            }
            (Mode::Prepublish, Source::PublishedZones) => {
                "Pre-publication review server for published zones"
            }
            (Mode::Publish, Source::UnsignedZones) => "Publication server for unsigned zones",
            (Mode::Publish, Source::SignedZones) => "Publication server for signed zones",
            (Mode::Publish, Source::PublishedZones) => "Publication server for published zones",
        };

        formatdoc! {
            r#"
            <!DOCTYPE html>
            <html lang="en">
                <head>
                  <meta charset="UTF-8">
                </head>
                <body>
                <pre>{intro}

            "#,
        }
    }

    async fn build_status_response_body(&self, response_body: &mut String) {
        let num_zones = self.zones.zones.load().iter_zones().count();
        response_body.push_str(&format!("Serving {num_zones} zones on:\n"));

        for addr in &self.listen {
            response_body.push_str(&format!("  - {addr}\n"));
        }
        response_body.push('\n');

        for zone in self.zones.zones.load().iter_zones() {
            response_body.push_str(&format!("zone:   {}\n", zone.apex_name()));
            if self.mode == Mode::Prepublish {
                for ((zone_name, zone_serial), pending_approvals) in self
                    .pending_approvals
                    .read()
                    .await
                    .iter()
                    .filter(|((zone_name, _), _)| zone_name == zone.apex_name())
                {
                    response_body.push_str(&format!(
                        "        pending approvals for serial {zone_serial}: {}\n",
                        pending_approvals.len()
                    ));
                }
            }
        }
    }

    fn build_response_footer(&self, response_body: &mut String) {
        response_body.push_str("    </pre>\n");
        response_body.push_str("  </body>\n");
        response_body.push_str("</html>\n");
    }
}
