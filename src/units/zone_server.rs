use core::future::ready;

use std::collections::HashMap;
use std::marker::Sync;
use std::net::IpAddr;
use std::pin::Pin;
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use bytes::Bytes;
use domain::base::iana::{Class, Rcode};
use domain::base::Name;
use domain::base::{Serial, ToName};
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::{self, DgramServer};
use domain::net::server::message::Request;
use domain::net::server::middleware::cookies::CookiesMiddlewareSvc;
use domain::net::server::middleware::edns::EdnsMiddlewareSvc;
use domain::net::server::middleware::mandatory::MandatoryMiddlewareSvc;
use domain::net::server::middleware::notify::{Notifiable, NotifyError, NotifyMiddlewareSvc};
use domain::net::server::middleware::tsig::TsigMiddlewareSvc;
use domain::net::server::middleware::xfr::XfrMiddlewareSvc;
use domain::net::server::middleware::xfr::{XfrData, XfrDataProvider, XfrDataProviderError};
use domain::net::server::service::{CallResult, Service, ServiceResult};
use domain::net::server::stream::{self, StreamServer};
use domain::net::server::util::mk_builder_for_target;
use domain::net::server::util::service_fn;
use domain::net::server::ConnectionConfig;
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key};
use domain::zonetree::types::EmptyZoneDiff;
use domain::zonetree::Answer;
use domain::zonetree::{StoredName, ZoneTree};
use futures::Future;
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::RwLock;
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;
use uuid::Uuid;

use crate::center::Center;
use crate::common::net::ListenAddr;
use crate::common::tsig::TsigKeyStore;
use crate::comms::ApplicationCommand;
use crate::comms::Terminated;
use crate::payload::Update;

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
    pub center: Arc<Center>,

    /// The relative path at which we should listen for HTTP query API requests
    pub http_api_path: Arc<String>,

    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// XFR out per zone: Allow XFR to, and when with a port also send NOTIFY to.
    pub _xfr_out: HashMap<StoredName, String>,

    pub hooks: Vec<String>,

    pub mode: Mode,

    pub source: Source,
}

impl ZoneServerUnit {
    pub async fn run(
        mut self,
        cmd_rx: mpsc::UnboundedReceiver<ApplicationCommand>,
    ) -> Result<(), Terminated> {
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
            Source::UnsignedZones => self.center.unsigned_zones.clone(),
            Source::SignedZones => self.center.signed_zones.clone(),
            Source::PublishedZones => self.center.published_zones.clone(),
        };

        let max_concurrency = std::thread::available_parallelism().unwrap().get() / 2;

        // TODO: Pass xfr_out to XfrDataProvidingZonesWrapper for enforcement.
        let zones = XfrDataProvidingZonesWrapper {
            zones,
            key_store: self.center.old_tsig_key_store.clone(),
        };

        // Propagate NOTIFY messages if this is the publication server.
        let notifier = LoaderNotifier {
            enabled: self.source == Source::PublishedZones,
            update_tx: self.center.update_tx.clone(),
        };

        // let svc = ZoneServerService::new(zones.clone());
        let svc = service_fn(zone_server_service, zones.clone());
        let svc = XfrMiddlewareSvc::new(svc, zones.clone(), max_concurrency);
        let svc = NotifyMiddlewareSvc::new(svc, notifier);
        let svc = TsigMiddlewareSvc::new(svc, self.center.old_tsig_key_store.clone());
        let svc = CookiesMiddlewareSvc::with_random_secret(svc);
        let svc = EdnsMiddlewareSvc::new(svc);
        let svc = MandatoryMiddlewareSvc::<_, _, ()>::new(svc);
        let svc = Arc::new(svc);

        let listen_strings = self
            .listen
            .iter()
            .filter_map(|addr| match addr {
                ListenAddr::Udp(socket_addr) => Some(socket_addr.to_string()),
                ListenAddr::Tcp(socket_addr) => Some(socket_addr.to_string()),
                ListenAddr::UdpSocket(_) | ListenAddr::TcpListener(_) => None,
            })
            .collect();

        for addr in self.listen.drain(..) {
            info!("[{unit_name}]: Binding on {addr:?}");
            let svc = svc.clone();
            let unit_name: Box<str> = unit_name.into();
            tokio::spawn(async move {
                if let Err(err) = Self::server(addr, svc).await {
                    error!("[{unit_name}]: {err}");
                }
            });
        }

        let update_tx = self.center.update_tx.clone();
        ZoneServer::new(
            self.center,
            self.http_api_path,
            self.mode,
            self.source,
            self.hooks,
            listen_strings,
            zones,
        )
        .run(unit_name, update_tx, cmd_rx)
        .await?;

        Ok(())
    }

    async fn server<Svc>(addr: ListenAddr, svc: Svc) -> Result<(), std::io::Error>
    where
        Svc: Service<Vec<u8>, ()> + Clone,
    {
        let buf = VecBufSource;
        match addr {
            ListenAddr::Udp(addr) => {
                let sock = UdpSocket::bind(addr).await?;
                serve_on_udp(svc, buf, sock).await;
            }
            ListenAddr::UdpSocket(sock) => {
                let sock = UdpSocket::from_std(sock)?;
                serve_on_udp(svc, buf, sock).await;
            }
            ListenAddr::Tcp(addr) => {
                let sock = tokio::net::TcpListener::bind(addr).await?;
                serve_on_tcp(svc, buf, sock).await;
            }
            ListenAddr::TcpListener(listener) => {
                listener.set_nonblocking(true)?;
                let listener = tokio::net::TcpListener::from_std(listener)?;
                serve_on_tcp(svc, buf, listener).await;
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

async fn serve_on_udp<Svc>(svc: Svc, buf: VecBufSource, sock: UdpSocket)
where
    Svc: Service<Vec<u8>, ()> + Clone,
{
    let config = dgram::Config::new();
    let srv = DgramServer::<_, _, _>::with_config(sock, buf, svc, config);
    let srv = Arc::new(srv);
    srv.run().await;
}

async fn serve_on_tcp<Svc>(svc: Svc, buf: VecBufSource, sock: tokio::net::TcpListener)
where
    Svc: Service<Vec<u8>, ()> + Clone,
{
    let mut conn_config = ConnectionConfig::new();
    conn_config.set_max_queued_responses(10000);
    let mut config = stream::Config::new();
    config.set_connection_config(conn_config);
    let srv = StreamServer::with_config(sock, buf, svc, config);
    let srv = Arc::new(srv);
    srv.run().await;
}

//------------ ZoneServer ----------------------------------------------------

struct ZoneServer {
    http_api_path: Arc<String>,
    zone_review_api: Option<ZoneReviewApi>,
    center: Arc<Center>,
    mode: Mode,
    source: Source,
    hooks: Vec<String>,
    listen: Vec<String>,
    #[allow(clippy::type_complexity)]
    pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
    #[allow(clippy::type_complexity)]
    last_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Instant>>>,
    zones: XfrDataProvidingZonesWrapper,
}

impl ZoneServer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        center: Arc<Center>,
        http_api_path: Arc<String>,
        mode: Mode,
        source: Source,
        hooks: Vec<String>,
        listen: Vec<String>,
        zones: XfrDataProvidingZonesWrapper,
    ) -> Self {
        Self {
            zone_review_api: Default::default(),
            http_api_path,
            center,
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
        mut self,
        unit_name: &str,
        update_tx: mpsc::UnboundedSender<Update>,
        mut cmd_rx: mpsc::UnboundedReceiver<ApplicationCommand>,
    ) -> Result<(), crate::comms::Terminated> {
        // let status_reporter = self.status_reporter.clone();

        // Setup approval API endpoint
        self.zone_review_api = Some(ZoneReviewApi::new(
            update_tx.clone(),
            self.pending_approvals.clone(),
            self.last_approvals.clone(),
            self.zones.clone(),
            self.mode,
            self.source,
            self.listen.clone(),
        ));

        // status_reporter.listener_listening(&listen_addr.to_string());

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        // arc_self.status_reporter.terminated();
                        return Err(Terminated);
                    };

                    self.handle_command(cmd, unit_name, update_tx.clone()).await?;
                }
            }
        }
    }

    async fn handle_command(
        &self,
        cmd: ApplicationCommand,
        unit_name: &str,
        update_tx: mpsc::UnboundedSender<Update>,
    ) -> Result<(), Terminated> {
        info!("[{unit_name}] Received command: {cmd:?}",);
        match cmd {
            ApplicationCommand::Terminate => {
                // arc_self.status_reporter.terminated();
                return Err(Terminated);
            }

            ApplicationCommand::HandleZoneReviewApi {
                zone_name,
                zone_serial,
                approval_token,
                operation,
                http_tx,
            } => {
                self.on_zone_review_api_cmd(
                    zone_name,
                    zone_serial,
                    &approval_token,
                    &operation,
                    http_tx,
                )
                .await;
            }

            ApplicationCommand::HandleZoneReviewApiStatus { http_tx } => {
                self.on_zone_review_api_status_cmd(http_tx).await;
            }

            ApplicationCommand::SeekApprovalForUnsignedZone { .. }
            | ApplicationCommand::SeekApprovalForSignedZone { .. } => {
                self.on_seek_approval_for_zone_cmd(cmd, unit_name, update_tx)
                    .await;
            }

            ApplicationCommand::PublishSignedZone {
                zone_name,
                zone_serial,
            } => {
                self.on_publish_signed_zone_cmd(unit_name, zone_name, zone_serial)
                    .await;
            }

            _ => { /* Not for us */ }
        }

        Ok(())
    }

    async fn on_publish_signed_zone_cmd(
        &self,
        unit_name: &str,
        zone_name: Name<Bytes>,
        zone_serial: Serial,
    ) {
        info!("[{unit_name}]: Publishing signed zone '{zone_name}' at serial {zone_serial}.");
        // Move the zone from the signed collection to the published collection.
        // TODO: Bump the zone serial?
        let signed_zones = self.center.signed_zones.load();
        if let Some(zone) = signed_zones.get_zone(&zone_name, Class::IN) {
            let published_zones = self.center.published_zones.load();

            // Create a deep copy of the set of
            // published zones. We will add the
            // new zone to that copied set and
            // then replace the original set with
            // the new set.
            info!("[{unit_name}]: Adding '{zone_name}' to the set of published zones.");
            let mut new_published_zones = Arc::unwrap_or_clone(published_zones.clone());
            let _ = new_published_zones.remove_zone(zone.apex_name(), zone.class());
            new_published_zones.insert_zone(zone.clone()).unwrap();
            self.center
                .published_zones
                .store(Arc::new(new_published_zones));

            // Create a deep copy of the set of
            // signed zones. We will remove the
            // zone from the copied set and then
            // replace the original set with the
            // new set.
            info!("[{unit_name}]: Removing '{zone_name}' from the set of signed zones.");
            let mut new_signed_zones = Arc::unwrap_or_clone(signed_zones.clone());
            new_signed_zones.remove_zone(&zone_name, Class::IN).unwrap();
            self.center.signed_zones.store(Arc::new(new_signed_zones));
        }
    }

    async fn on_seek_approval_for_zone_cmd(
        &self,
        cmd: ApplicationCommand,
        unit_name: &str,
        update_tx: mpsc::UnboundedSender<Update>,
    ) -> Option<Result<(), Terminated>> {
        let (zone_name, zone_serial, zone_type) = match cmd {
            ApplicationCommand::SeekApprovalForUnsignedZone {
                zone_name,
                zone_serial,
            } => (zone_name, zone_serial, "unsigned"),
            ApplicationCommand::SeekApprovalForSignedZone {
                zone_name,
                zone_serial,
            } => (zone_name, zone_serial, "signed"),
            _ => unreachable!(),
        };

        if self
            .last_approvals
            .read()
            .await
            .contains_key(&(zone_name.clone(), zone_serial))
        {
            trace!("Skipping approval request for already approved {zone_type} zone '{zone_name}' at serial {zone_serial}.");
            return Some(Ok(()));
        }
        if self.hooks.is_empty() {
            // Approve immediately.
            match self.source {
                Source::UnsignedZones => {
                    update_tx
                        .send(Update::UnsignedZoneApprovedEvent {
                            zone_name: zone_name.clone(),
                            zone_serial,
                        })
                        .unwrap();
                }
                Source::SignedZones => {
                    update_tx
                        .send(Update::SignedZoneApprovedEvent {
                            zone_name: zone_name.clone(),
                            zone_serial,
                        })
                        .unwrap();
                }
                Source::PublishedZones => unreachable!(),
            }
        }
        info!("[{unit_name}]: Seeking approval for {zone_type} zone '{zone_name}' at serial {zone_serial}.");

        // Only if not already approved...

        for hook in &self.hooks {
            let approval_token = Uuid::new_v4();
            info!("[{unit_name}]: Generated approval token '{approval_token}' for {zone_type} zone '{zone_name}' at serial {zone_serial}.");

            self.pending_approvals
                .write()
                .await
                .entry((zone_name.clone(), zone_serial))
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
                    info!("[{unit_name}]: Confirm with HTTP GET {}approve/{approval_token}?zone={zone_name}&serial={zone_serial}", self.http_api_path);
                    info!("[{unit_name}]: Reject with HTTP GET {}reject/{approval_token}?zone={zone_name}&serial={zone_serial}", self.http_api_path);
                }
                Err(err) => {
                    error!(
                                    "[{unit_name}]: Failed to execute hook '{hook}' for {zone_type} zone '{zone_name}' at serial {zone_serial}: {err}",
                                );
                    self.pending_approvals
                        .write()
                        .await
                        .remove(&(zone_name.clone(), zone_serial));
                }
            }
        }
        None
    }

    async fn on_zone_review_api_status_cmd(&self, http_tx: Sender<String>) {
        http_tx
            .send(
                self.zone_review_api
                    .as_ref()
                    .expect("This should have been setup on startup.")
                    .build_status_response()
                    .await,
            )
            .await
            .expect("TODO: Should this always succeed?");
    }

    async fn on_zone_review_api_cmd(
        &self,
        zone_name: Name<Bytes>,
        zone_serial: Serial,
        approval_token: &str,
        operation: &str,
        http_tx: Sender<Result<(), ()>>,
    ) {
        http_tx
            .send(
                self.zone_review_api
                    .as_ref()
                    .expect("This should have been setup on startup.")
                    .process_request(zone_name.clone(), zone_serial, approval_token, operation)
                    .await,
            )
            .await
            .expect("TODO: Should this always succeed?");
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

//----------- LoaderNotifier ---------------------------------------------------

/// A forwarder of NOTIFY messages to the zone loader.
#[derive(Clone, Debug)]
pub struct LoaderNotifier {
    /// Whether the forwarder is enabled.
    enabled: bool,

    /// A channel to propagate updates to Cascade.
    update_tx: mpsc::UnboundedSender<Update>,
}

impl Notifiable for LoaderNotifier {
    // TODO: Get the SOA serial in the NOTIFY message.
    fn notify_zone_changed(
        &self,
        class: Class,
        apex_name: &Name<Bytes>,
        serial: Option<Serial>,
        source: IpAddr,
    ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Sync + Send + '_>> {
        // Don't do anything if the notifier is disabled.
        if self.enabled && class == Class::IN {
            // Propagate a request for the zone refresh.
            let _ = self.update_tx.send(Update::RefreshZone {
                zone_name: apex_name.clone(),
                source: Some(source),
                serial,
            });
        }

        Box::pin(std::future::ready(Ok(())))
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

// TODO: Should we expire old pending approvals, e.g. a hook script failed and
// they never got approved or rejected?
impl ZoneReviewApi {
    async fn process_request(
        &self,
        zone_name: Name<Bytes>,
        zone_serial: Serial,
        given_approval_token: &str,
        operation: &str,
    ) -> Result<(), ()> {
        let mut status = Err(());
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
                            status = Ok(());
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
                                    .and_modify(|instant| *instant = approved_at)
                                    .or_insert(approved_at);
                                self.update_tx.send(event).unwrap();
                                remove_approvals = true;
                            }
                        }
                        "reject" => {
                            info!("Pending zone '{zone_name}' rejected at serial {zone_serial}.");
                            status = Ok(());
                            remove_approvals = true;
                        }
                        _ => unreachable!(),
                    }
                } else {
                    warn!("No pending approval found for zone name '{zone_name}' at serial {zone_serial} with approval token '{given_uuid}'.");
                }
            } else {
                warn!("Invalid approval token '{given_approval_token}' in request.");
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

        status
    }
}

//------------ ZoneReviewApi -------------------------------------------------

struct ZoneReviewApi {
    update_tx: mpsc::UnboundedSender<Update>,
    #[allow(clippy::type_complexity)]
    pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
    #[allow(clippy::type_complexity)]
    last_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Instant>>>,
    zones: XfrDataProvidingZonesWrapper,
    mode: Mode,
    source: Source,
    listen: Vec<String>,
}

impl ZoneReviewApi {
    #[allow(clippy::type_complexity)]
    fn new(
        update_tx: mpsc::UnboundedSender<Update>,
        pending_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Vec<Uuid>>>>,
        last_approvals: Arc<RwLock<HashMap<(Name<Bytes>, Serial), Instant>>>,
        zones: XfrDataProvidingZonesWrapper,
        mode: Mode,
        source: Source,
        listen: Vec<String>,
    ) -> Self {
        Self {
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

impl ZoneReviewApi {
    pub async fn build_status_response(&self) -> String {
        let mut response_body = self.build_response_header().await;

        self.build_status_response_body(&mut response_body).await;

        self.build_response_footer(&mut response_body);

        response_body
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
                for ((_zone_name, zone_serial), pending_approvals) in self
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
