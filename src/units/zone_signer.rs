use core::fmt;
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
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use domain::base::iana::{Class, Rcode};
use domain::base::wire::Composer;
use domain::base::{Name, ParsedName, ParsedRecord, Record, Rtype, Serial, ToName};
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
use crate::comms::ApplicationCommand;
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
use core::future::pending;
use domain::base::name::FlattenInto;
use domain::base::record::ComposeRecord;
use domain::rdata::dnssec::Timestamp;
use domain::rdata::{Soa, ZoneRecordData};
use domain::sign::crypto::common::{generate, GenerateParams, KeyPair};
use domain::sign::keys::{DnssecSigningKey, SigningKey};
use domain::sign::records::SortedRecords;
use domain::sign::traits::SignableZoneInPlace;
use domain::sign::SigningConfig;
use domain::tsig::KeyStore;
use domain::zonetree::types::ZoneUpdate;
use domain::zonetree::update::ZoneUpdater;
use octseq::{OctetsInto, Parser};

#[serde_as]
#[derive(Clone, Debug, Deserialize)]
pub struct ZoneSignerUnit {
    /// The relative path at which we should listen for HTTP query API requests
    #[serde(default = "ZoneSignerUnit::default_http_api_path")]
    http_api_path: Arc<String>,
}

impl ZoneSignerUnit {
    pub async fn run(
        self,
        mut component: Component,
        gate: Gate,
        mut waitpoint: WaitPoint,
    ) -> Result<(), Terminated> {
        let unit_name = component.name().clone();

        // Setup our metrics
        let metrics = Arc::new(ZoneSignerMetrics::new(&gate));
        component.register_metrics(metrics.clone());

        // Setup our status reporting
        let status_reporter = Arc::new(ZoneSignerStatusReporter::new(&unit_name, metrics.clone()));

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

        ZoneSigner::new(
            component,
            self.http_api_path,
            gate,
            metrics,
            status_reporter,
        )
        .run()
        .await?;

        Ok(())
    }

    fn default_http_api_path() -> Arc<String> {
        Arc::new("/zone-signer/".to_string())
    }
}

//------------ ZoneSigner ----------------------------------------------------

struct ZoneSigner {
    component: Arc<RwLock<Component>>,
    #[allow(dead_code)]
    http_api_path: Arc<String>,
    gate: Gate,
    metrics: Arc<ZoneSignerMetrics>,
    status_reporter: Arc<ZoneSignerStatusReporter>,
}

impl ZoneSigner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Arc<RwLock<Component>>,
        http_api_path: Arc<String>,
        gate: Gate,
        metrics: Arc<ZoneSignerMetrics>,
        status_reporter: Arc<ZoneSignerStatusReporter>,
    ) -> Self {
        Self {
            component,
            http_api_path,
            gate,
            metrics,
            status_reporter,
        }
    }

    async fn run(self) -> Result<(), crate::comms::Terminated> {
        let status_reporter = self.status_reporter.clone();

        let arc_self = Arc::new(self);

        loop {
            //     status_reporter.listener_listening(&listen_addr.to_string());

            match arc_self.clone().process_until(pending()).await {
                ControlFlow::Continue(Some((zone_name, zone_serial))) => {
                    // status_reporter
                    //     .listener_connection_accepted(client_addr);

                    info!(
                        "[{}]: Received a new copy of zone '{zone_name}' at serial {zone_serial}",
                        arc_self.component.read().await.name(),
                    );

                    arc_self
                        .gate
                        .update_data(Update::ZoneUpdatedEvent {
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
                            new_config: Unit::ZoneSigner(ZoneSignerUnit { http_api_path }),
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
                            match &cmd {
                                ApplicationCommand::SignZone {
                                    zone_name,
                                    zone_serial,
                                } => {
                                    let unsigned_zone = {
                                        // Sign the unsigned zone and store it as a signed zone.
                                        let component = self.component.read().await;
                                        let unsigned_zones = component.unsigned_zones().load();
                                        unsigned_zones.get_zone(&zone_name, Class::IN).cloned()
                                    };

                                    let (soa_rr, records) = if let Some(unsigned_zone) =
                                        unsigned_zone
                                    {
                                        // Sign the zone and store the resulting RRs.

                                        // Temporary: Accumulate the zone into a vec as we can only sign
                                        // over a slice at the moment, not over an iterator yet (nor can
                                        // we iterate over a zone yet, only walk it ...).

                                        let read = unsigned_zone.read();

                                        let answer =
                                            read.query(zone_name.clone(), Rtype::SOA).unwrap();

                                        let (soa_ttl, soa_data) = answer.content().first().unwrap();
                                        let soa_rr = Record::new(
                                            zone_name.clone(),
                                            Class::IN,
                                            soa_ttl,
                                            soa_data,
                                        );

                                        let records = Arc::new(std::sync::Mutex::new(
                                            SortedRecords::default(),
                                        ));
                                        let passed_records = records.clone();

                                        read.walk(Box::new(move |owner, rrset, _at_zone_cut| {
                                            for data in rrset.data() {
                                                // WARNING: insert() is slow for large zones,
                                                // use extend() instead.
                                                passed_records
                                                    .lock()
                                                    .unwrap()
                                                    .insert(Record::new(
                                                        owner.clone(),
                                                        Class::IN,
                                                        rrset.ttl(),
                                                        data.to_owned(),
                                                    ))
                                                    .unwrap();
                                            }
                                        }));

                                        // Generate signing key.
                                        // TODO: This should come from configuration.

                                        // Generate a new Ed25519 key.
                                        let params = GenerateParams::Ed25519;
                                        let (sec_bytes, pub_bytes) = generate(params).unwrap();

                                        // Parse the key into Ring or OpenSSL.
                                        let key_pair =
                                            KeyPair::from_bytes(&sec_bytes, &pub_bytes).unwrap();

                                        // Associate the key with important metadata.
                                        let owner: Name<Vec<u8>> =
                                            "www.example.org.".parse().unwrap();
                                        let owner: Name<Bytes> = owner.octets_into();
                                        let flags = 257; // key signing key
                                        let key = SigningKey::new(owner, flags, key_pair);

                                        // Assign signature validity period and operator intent to the keys.
                                        let key = key.with_validity(
                                            Timestamp::from_str("20240101000000").unwrap(),
                                            Timestamp::from_str("20260101000000").unwrap(),
                                        );
                                        let keys = [DnssecSigningKey::new_csk(key)];

                                        // Create a signing configuration.
                                        let mut signing_config = SigningConfig::default();

                                        // Then sign the zone adding the generated records to the
                                        // signer_generated_rrs collection, as we don't want to keep two
                                        // copies of the unsigned records, we already have those in the
                                        // zone.
                                        let mut records =
                                            Arc::into_inner(records).unwrap().into_inner().unwrap();
                                        info!("Pre-signing");
                                        records.sign_zone(&mut signing_config, &keys).unwrap();
                                        info!("Post-signing");
                                        (soa_rr, records)
                                    } else {
                                        info!("Unreachable");
                                        unreachable!();
                                    };
                                    info!("Post all-signing");

                                    // Store the zone in the signed zone tree.
                                    // First see if the zone already exists,
                                    // and ensure we don't hold a read lock.
                                    let mut zone = {
                                        let signed_zones =
                                            self.component.read().await.signed_zones().load();
                                        signed_zones.get_zone(zone_name, Class::IN).cloned()
                                    };

                                    if zone.is_none() {
                                        info!("Creating zone");
                                        let component = self.component.write().await;
                                        let zones = component.signed_zones().load().clone();
                                        let mut new_zones = Arc::unwrap_or_clone(zones);
                                        let new_zone =
                                            ZoneBuilder::new(zone_name.clone(), Class::IN).build();
                                        new_zones.insert_zone(new_zone.clone()).unwrap();
                                        component.signed_zones().store(Arc::new(new_zones));
                                        zone = Some(new_zone);
                                    };

                                    let zone = zone.unwrap();

                                    // Update the content of the zone.
                                    let mut updater = ZoneUpdater::new(zone).await.unwrap();
                                    updater.apply(ZoneUpdate::DeleteAllRecords).await.unwrap();
                                    for rr in records.into_inner() {
                                        updater.apply(ZoneUpdate::AddRecord(rr)).await.unwrap();
                                    }
                                    updater.apply(ZoneUpdate::Finished(soa_rr)).await.unwrap();
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

impl std::fmt::Debug for ZoneSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoneSigner").finish()
    }
}

#[async_trait]
impl DirectUpdate for ZoneSigner {
    async fn direct_update(&self, event: Update) {
        info!(
            "[{}]: Received event: {event:?}",
            self.component.read().await.name()
        );
    }
}

impl AnyDirectUpdate for ZoneSigner {}

//------------ ZoneSignerMetrics ---------------------------------------------

#[derive(Debug, Default)]
pub struct ZoneSignerMetrics {
    gate: Option<Arc<GateMetrics>>, // optional to make testing easier
}

impl GraphStatus for ZoneSignerMetrics {
    fn status_text(&self) -> String {
        "TODO".to_string()
    }

    fn okay(&self) -> Option<bool> {
        Some(false)
    }
}

impl ZoneSignerMetrics {
    // const LISTENER_BOUND_COUNT_METRIC: Metric = Metric::new(
    //     "bmp_tcp_in_listener_bound_count",
    //     "the number of times the TCP listen port was bound to",
    //     MetricType::Counter,
    //     MetricUnit::Total,
    // );
}

impl ZoneSignerMetrics {
    pub fn new(gate: &Gate) -> Self {
        Self {
            gate: Some(gate.metrics()),
        }
    }
}

impl metrics::Source for ZoneSignerMetrics {
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

//------------ ZoneSignerStatusReporter --------------------------------------

#[derive(Debug, Default)]
pub struct ZoneSignerStatusReporter {
    name: String,
    metrics: Arc<ZoneSignerMetrics>,
}

impl ZoneSignerStatusReporter {
    pub fn new<T: Display>(name: T, metrics: Arc<ZoneSignerMetrics>) -> Self {
        Self {
            name: format!("{}", name),
            metrics,
        }
    }

    pub fn _typed_metrics(&self) -> Arc<ZoneSignerMetrics> {
        self.metrics.clone()
    }
}

impl UnitStatusReporter for ZoneSignerStatusReporter {}

impl AnyStatusReporter for ZoneSignerStatusReporter {
    fn metrics(&self) -> Option<Arc<dyn crate::metrics::Source>> {
        Some(self.metrics.clone())
    }
}

impl Chainable for ZoneSignerStatusReporter {
    fn add_child<T: Display>(&self, child_name: T) -> Self {
        Self::new(self.link_names(child_name), self.metrics.clone())
    }
}

impl Named for ZoneSignerStatusReporter {
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
