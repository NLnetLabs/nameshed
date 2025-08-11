use core::fmt;
use core::future::{ready, Ready};

use std::any::Any;
use std::collections::HashMap;
use std::convert::Infallible;
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
use std::time::{Duration, Instant};

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
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;
use uuid::Uuid;

use crate::common::net::{
    ListenAddr, StandardTcpListenerFactory, StandardTcpStream, TcpListener, TcpListenerFactory,
    TcpStreamWrapper,
};
use crate::common::tsig::{parse_key_strings, TsigKeyStore};
use crate::common::xfr::parse_xfr_acl;
use crate::comms::ApplicationCommand;
use crate::comms::{GraphStatus, Terminated};
use crate::http::{PercentDecodedPath, ProcessRequest};
use crate::log::ExitError;
use crate::manager::Component;
use crate::metrics::{self, util::append_per_router_metric, Metric, MetricType, MetricUnit};
use crate::payload::Update;
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

#[derive(Debug)]
pub struct ZoneServerUnit {
    /// The relative path at which we should listen for HTTP query API requests
    pub http_api_path: Arc<String>,

    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// XFR out per zone: Allow XFR to, and when with a port also send NOTIFY to.
    pub xfr_out: HashMap<String, String>,

    pub hooks: Vec<String>,

    pub mode: Mode,

    pub source: Source,

    pub update_tx: mpsc::Sender<Update>,

    pub cmd_rx: mpsc::Receiver<ApplicationCommand>,
}

impl ZoneServerUnit {
    pub async fn run(self, component: Component) -> Result<(), Terminated> {
        let unit_name = match (self.mode, self.source) {
            (Mode::Prepublish, Source::UnsignedZones) => "RS",
            (Mode::Prepublish, Source::SignedZones) => "RS2",
            (Mode::Publish, Source::PublishedZones) => "PS",
            _ => unreachable!(),
        };

        // TODO: metrics and status reporting

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
            info!("[{unit_name}]: Binding on {:?}", addr);
            let svc = svc.clone();
            let unit_name: Box<str> = unit_name.into();
            tokio::spawn(async move {
                if let Err(err) = Self::server(addr, svc).await {
                    error!("[{unit_name}]: {}", err);
                }
            });
        }

        let component = Arc::new(RwLock::new(component));

        ZoneServer::new(
            component,
            self.http_api_path,
            self.mode,
            self.source,
            self.hooks,
            self.listen,
            zones,
        )
        .run(unit_name, self.update_tx, self.cmd_rx)
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
    #[allow(dead_code)]
    mode: Mode,
    source: Source,
    hooks: Vec<String>,
    listen: Vec<ListenAddr>,
    #[allow(clippy::type_complexity)]
    pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
    last_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Instant>>>,
    zones: XfrDataProvidingZonesWrapper,
}

impl ZoneServer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Arc<RwLock<Component>>,
        http_api_path: Arc<String>,
        mode: Mode,
        source: Source,
        hooks: Vec<String>,
        listen: Vec<ListenAddr>,
        zones: XfrDataProvidingZonesWrapper,
    ) -> Self {
        Self {
            component,
            http_api_path,
            mode,
            source,
            hooks,
            pending_approvals: Default::default(),
            last_approvals: Default::default(),
            listen,
            zones,
        }
    }

    async fn run(
        self,
        unit_name: &str,
        update_tx: mpsc::Sender<Update>,
        mut cmd_rx: mpsc::Receiver<ApplicationCommand>,
    ) -> Result<(), crate::comms::Terminated> {
        // let status_reporter = self.status_reporter.clone();

        // Setup REST API endpoint
        let http_processor = Arc::new(ZoneReviewApi::new(
            self.http_api_path.clone(),
            update_tx.clone(),
            self.pending_approvals.clone(),
            self.last_approvals.clone(),
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

        // status_reporter.listener_listening(&listen_addr.to_string());

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        // arc_self.status_reporter.terminated();
                        return Err(Terminated);
                    };

                    info!("[{unit_name}] Received command: {cmd:?}",);
                    match &cmd {
                        ApplicationCommand::Terminate => {
                            // arc_self.status_reporter.terminated();
                            return Err(Terminated);
                        }

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

                            // Only if not already approved...
                            if arc_self.last_approvals.read().await.contains_key(&(zone_name.clone(), *zone_serial)) {
                                trace!("Skipping approval request for already approved {zone_type} zone '{zone_name}' at serial {zone_serial}.");
                                continue;
                            }

                            info!("[{unit_name}]: Seeking approval for {zone_type} zone '{zone_name}' at serial {zone_serial}.");
                            for hook in &arc_self.hooks {
                                let approval_token = Uuid::new_v4();
                                info!("[{unit_name}]: Generated approval token '{approval_token}' for {zone_type} zone '{zone_name}' at serial {zone_serial}.");

                                arc_self.pending_approvals
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
                                        info!("[{unit_name}]: Executed hook '{hook}' for {zone_type} zone '{zone_name}' at serial {zone_serial}");
                                        info!("[{unit_name}]: Confirm with HTTP GET {}approve/{approval_token}?zone={zone_name}&serial={zone_serial}", arc_self.http_api_path);
                                        info!("[{unit_name}]: Reject with HTTP GET {}reject/{approval_token}?zone={zone_name}&serial={zone_serial}", arc_self.http_api_path);
                                    }
                                    Err(err) => {
                                        error!(
                                            "[{unit_name}]: Failed to execute hook '{hook}' for {zone_type} zone '{zone_name}' at serial {zone_serial}: {err}",
                                        );
                                        arc_self.pending_approvals
                                            .write()
                                            .await
                                            .remove(&(zone_name.clone(), *zone_serial));
                                    }
                                }
                            }
                        }

                        ApplicationCommand::PublishSignedZone {
                            zone_name,
                            zone_serial,
                        } => {
                            info!(
                                "[{unit_name}]: Publishing signed zone '{zone_name}' at serial {zone_serial}."
                            );
                            // Move the zone from the signed collection to the published collection.
                            // TODO: Bump the zone serial?
                            let component = arc_self.component.write().await;
                            let signed_zones = component.signed_zones().load();
                            if let Some(zone) = signed_zones.get_zone(zone_name, Class::IN)
                            {
                                let published_zones = component.published_zones().load();

                                // Create a deep copy of the set of
                                // published zones. We will add the
                                // new zone to that copied set and
                                // then replace the original set with
                                // the new set.
                                info!("[{unit_name}]: Adding '{zone_name}' to the set of published zones.");
                                let mut new_published_zones =
                                    Arc::unwrap_or_clone(published_zones.clone());
                                let _ = new_published_zones
                                    .remove_zone(zone.apex_name(), zone.class());
                                new_published_zones.insert_zone(zone.clone()).unwrap();
                                component
                                    .published_zones()
                                    .store(Arc::new(new_published_zones));

                                // Create a deep copy of the set of
                                // signed zones. We will remove the
                                // zone from the copied set and then
                                // replace the original set with the
                                // new set.
                                info!("[{unit_name}]: Removing '{zone_name}' from the set of signed zones.");
                                let mut new_signed_zones =
                                    Arc::unwrap_or_clone(signed_zones.clone());
                                new_signed_zones.remove_zone(zone_name, Class::IN).unwrap();
                                component.signed_zones().store(Arc::new(new_signed_zones));
                            }
                        }

                        _ => { /* Not for us */ }
                    }
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
    update_tx: mpsc::Sender<Update>,
    #[allow(clippy::type_complexity)]
    pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
    last_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Instant>>>,
    zones: XfrDataProvidingZonesWrapper,
    mode: Mode,
    source: Source,
    listen: Vec<ListenAddr>,
}

impl ZoneReviewApi {
    #[allow(clippy::type_complexity)]
    fn new(
        http_api_path: Arc<String>,
        update_tx: mpsc::Sender<Update>,
        pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
        last_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Instant>>>,
        zones: XfrDataProvidingZonesWrapper,
        mode: Mode,
        source: Source,
        listen: Vec<ListenAddr>,
    ) -> Self {
        Self {
            http_api_path,
            update_tx,
            pending_approvals,
            last_approvals,
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
        request: &hyper::Request<hyper::Body>,
    ) -> Option<hyper::Response<hyper::Body>> {
        let req_path = request.uri().decoded_path();
        if request.method() == hyper::Method::GET && req_path == *self.http_api_path {
            return Some(self.build_status_response().await);
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
                                                    let approved_at = Instant::now();
                                                    self.last_approvals
                                                        .write()
                                                        .await
                                                        .entry((zone_name.clone(), zone_serial))
                                                        .and_modify(|instant| {
                                                            *instant = approved_at
                                                        })
                                                        .or_insert(approved_at);
                                                    self.update_tx.send(event).await.unwrap();
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
                }
            }

            if status == StatusCode::BAD_REQUEST {
                debug!("Invalid request: {}", request.uri());
            }

            Some(
                hyper::Response::builder()
                    .status(status)
                    .body(hyper::Body::empty())
                    .unwrap(),
            )
        } else {
            None
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
