use core::fmt;
use core::future::pending;
use core::ops::Add;

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
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
use domain::base::name::FlattenInto;
use domain::base::record::ComposeRecord;
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
use domain::rdata::dnssec::Timestamp;
use domain::rdata::{Soa, ZoneRecordData};
use domain::sign::crypto::common::{generate, GenerateParams};
// Use openssl::KeyPair because ring::KeyPair is not Send.
use domain::sign::crypto::openssl::KeyPair;
use domain::sign::error::FromBytesError;
use domain::sign::keys::{DnssecSigningKey, SigningKey};
use domain::sign::records::SortedRecords;
use domain::sign::signatures::strategy::FixedRrsigValidityPeriodStrategy;
use domain::sign::traits::SignableZoneInPlace;
use domain::sign::{SecretKeyBytes, SigningConfig};
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace;
use domain::zonetree::types::ZoneUpdate;
use domain::zonetree::update::ZoneUpdater;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, WritableZone, WritableZoneNode, Zone, ZoneBuilder,
    ZoneStore, ZoneTree,
};
use futures::future::{select, Either};
use futures::{pin_mut, Future};
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use non_empty_vec::NonEmpty;
use octseq::{OctetsInto, Parser};
use serde::Deserialize;
use serde_with::{serde_as, DeserializeFromStr, DisplayFromStr};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;

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
use crate::http::PercentDecodedPath;
use crate::http::ProcessRequest;
use crate::log::ExitError;
use crate::manager::{Component, WaitPoint};
use crate::metrics::{self, util::append_per_router_metric, Metric, MetricType, MetricUnit};
use crate::payload::Update;
use crate::tokio::TokioTaskMetrics;
use crate::tracing::Tracer;
use crate::units::Unit;
use crate::zonemaintenance::maintainer::{Config, ZoneLookup};
use crate::zonemaintenance::maintainer::{DefaultConnFactory, TypedZone, ZoneMaintainer};
use crate::zonemaintenance::types::{
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore,
};
use domain::sign::keys::keymeta::IntendedKeyPurpose;
use std::collections::hash_map::Entry::{Occupied, Vacant};

#[serde_as]
#[derive(Clone, Debug, Deserialize)]
pub struct ZoneSignerUnit {
    /// The relative path at which we should listen for HTTP query API requests
    #[serde(default = "ZoneSignerUnit::default_http_api_path")]
    http_api_path: Arc<String>,

    keys_path: PathBuf,

    #[serde(default)]
    treat_single_keys_as_csks: bool,
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

        let mut key_path_stems = HashSet::new();
        let mut keys = HashMap::<StoredName, Vec<DnssecSigningKey<Bytes, KeyPair>>>::new();

        info!("Loading key pairs from '{}'.", self.keys_path.display());
        for entry in std::fs::read_dir(&self.keys_path).map_err(|err| {
            error!(
                "Unable to load keys from '{}': {err}",
                self.keys_path.display()
            );
            Terminated
        })? {
            match entry {
                Ok(entry)
                    if entry
                        .file_type()
                        .map(|typ| typ.is_file())
                        .unwrap_or_default() =>
                {
                    let path = entry.path();
                    match (path.file_stem(), path.extension()) {
                        (Some(stem), Some(ext)) if ext == "key" || ext == "private" => {
                            key_path_stems.insert(stem.to_owned());
                        }
                        _ => { /* Skip */ }
                    }
                }
                _ => { /* Skip */ }
            }
        }

        for stem in key_path_stems {
            let key_path = self.keys_path.join(stem);
            debug!("Attempting to load key pair '{}'.", key_path.display());

            let priv_key_path = Self::mk_private_key_path(&key_path);
            let private_key = Self::load_private_key(&priv_key_path).inspect_err(|_| {
                error!(
                    "Failed to load private key from '{}'",
                    priv_key_path.display()
                );
            })?;

            let pub_key_path = Self::mk_public_key_path(&key_path);
            let public_key = Self::load_public_key(&pub_key_path).inspect_err(|_| {
                error!(
                    "Failed to load public key from '{}'",
                    pub_key_path.display()
                );
            })?;

            let key = Self::mk_signing_key(&private_key, public_key).map_err(|err| {
                error!(
                    "Failed to make key pair for '{}': {err}",
                    key_path.display()
                );
                Terminated
            })?;

            // TODO: Don't assume the key is a CSK.
            let key = DnssecSigningKey::from(key);
            info!(
                "Loaded key pair '{}' for zone '{}' as {}.",
                key_path.display(),
                key.owner(),
                key.purpose()
            );
            match keys.entry(key.owner().to_owned()) {
                Occupied(mut e) => {
                    e.get_mut().push(key);
                }
                Vacant(e) => {
                    e.insert(vec![key]);
                }
            }
        }

        if self.treat_single_keys_as_csks {
            for (owner, owner_keys) in keys.iter_mut().filter(|(_, keys)| keys.len() == 1) {
                info!(
                    "Lone key {} for zone '{owner}' will be used as a CSK",
                    owner_keys[0].purpose()
                );
                owner_keys[0].set_purpose(IntendedKeyPurpose::CSK);
            }
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

        ZoneSigner::new(
            component,
            self.http_api_path,
            gate,
            metrics,
            status_reporter,
            keys,
        )
        .run()
        .await?;

        Ok(())
    }

    fn default_http_api_path() -> Arc<String> {
        Arc::new("/zone-signer/".to_string())
    }

    fn load_private_key(key_path: &Path) -> Result<SecretKeyBytes, Terminated> {
        let private_data = std::fs::read_to_string(key_path).map_err(|err| {
            error!("Unable to read file '{}': {err}", key_path.display());
            Terminated
        })?;

        // Note: Compared to the original ldns-signzone there is a minor
        // regression here because at the time of writing the error returned
        // from parsing indicates broadly the type of parsing failure but does
        // note indicate the line number at which parsing failed.
        let secret_key = SecretKeyBytes::parse_from_bind(&private_data).map_err(|err| {
            error!(
                "Unable to parse BIND formatted private key file '{}': {err}",
                key_path.display(),
            );
            Terminated
        })?;

        Ok(secret_key)
    }

    fn load_public_key(key_path: &Path) -> Result<domain::validate::Key<Bytes>, Terminated> {
        let public_data = std::fs::read_to_string(key_path).map_err(|_| Terminated)?;

        // Note: Compared to the original ldns-signzone there is a minor
        // regression here because at the time of writing the error returned
        // from parsing indicates broadly the type of parsing failure but does
        // note indicate the line number at which parsing failed.
        let public_key_info =
            domain::validate::Key::parse_from_bind(&public_data).map_err(|err| {
                error!(
                    "Unable to parse BIND formatted public key file '{}': {err}",
                    key_path.display(),
                );
                Terminated
            })?;

        Ok(public_key_info)
    }

    fn mk_public_key_path(key_path: &Path) -> PathBuf {
        if key_path.extension().and_then(|ext| ext.to_str()) == Some("key") {
            key_path.to_path_buf()
        } else {
            PathBuf::from(format!("{}.key", key_path.display()))
        }
    }

    fn mk_private_key_path(key_path: &Path) -> PathBuf {
        if key_path.extension().and_then(|ext| ext.to_str()) == Some("private") {
            key_path.to_path_buf()
        } else {
            PathBuf::from(format!("{}.private", key_path.display()))
        }
    }

    fn mk_signing_key(
        private_key: &SecretKeyBytes,
        public_key: domain::validate::Key<Bytes>,
    ) -> Result<SigningKey<Bytes, KeyPair>, FromBytesError> {
        let key_pair = KeyPair::from_bytes(private_key, public_key.raw_public_key())?;
        let signing_key = SigningKey::new(public_key.owner().clone(), public_key.flags(), key_pair);
        Ok(signing_key)
    }
}

//------------ ZoneSigner ----------------------------------------------------

struct ZoneSigner {
    component: Component,
    #[allow(dead_code)]
    http_api_path: Arc<String>,
    gate: Gate,
    metrics: Arc<ZoneSignerMetrics>,
    status_reporter: Arc<ZoneSignerStatusReporter>,
    signing_keys: HashMap<StoredName, Vec<DnssecSigningKey<Bytes, KeyPair>>>,
}

impl ZoneSigner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Component,
        http_api_path: Arc<String>,
        gate: Gate,
        metrics: Arc<ZoneSignerMetrics>,
        status_reporter: Arc<ZoneSignerStatusReporter>,
        signing_keys: HashMap<StoredName, Vec<DnssecSigningKey<Bytes, KeyPair>>>,
    ) -> Self {
        Self {
            component,
            http_api_path,
            gate,
            metrics,
            status_reporter,
            signing_keys,
        }
    }

    async fn run(self) -> Result<(), crate::comms::Terminated> {
        let component_name = self.component.name().clone();

        loop {
            match self.gate.process().await {
                Err(Terminated) => {
                    self.status_reporter.terminated();
                    return Ok(());
                }

                Ok(status) => {
                    self.status_reporter.gate_status_announced(&status);
                    match status {
                        GateStatus::Reconfiguring {
                            new_config:
                                Unit::ZoneSigner(ZoneSignerUnit {
                                    http_api_path,
                                    keys_path,
                                    treat_single_keys_as_csks: treat_single_key_as_csk,
                                }),
                        } => {
                            // Runtime reconfiguration of this unit has been
                            // requested. New connections will be handled
                            // using the new configuration, existing
                            // connections handled by router_handler() tasks
                            // will receive their own copy of this
                            // Reconfiguring status update and can react to it
                            // accordingly. let rebind = self.listen !=
                            // new_listen;

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
                            info!("[{component_name}]: Received command: {cmd:?}");
                            match &cmd {
                                ApplicationCommand::SignZone {
                                    zone_name,
                                    zone_serial,
                                } => {
                                    // Find a key that matches the zone being signed.
                                    // TODO: We should support multiple keys, not just one.
                                    match self.signing_keys.get(zone_name) {
                                        None => {
                                            error!("[{component_name}]: No matching key found to sign zone '{zone_name}'");
                                        }

                                        Some(keys) => {
                                            // Generate signing key timestamps for demonstration purposes.
                                            // TODO: Get the signing validity period from somewhere else.
                                            // TODO: Well, actually this will likely change a lot as there is a
                                            // lot more to keeping a zone signed over time than just signing it
                                            // once.
                                            const ONE_YEAR_IN_SECS: u32 = 60 * 60 * 24 * 365;
                                            let inception = Timestamp::now();
                                            let expiration = Timestamp::from(
                                                inception.into_int().add(ONE_YEAR_IN_SECS),
                                            );
                                            let validity = FixedRrsigValidityPeriodStrategy::new(
                                                inception, expiration,
                                            );

                                            let unsigned_zone = {
                                                let unsigned_zones =
                                                    self.component.unsigned_zones().load();
                                                unsigned_zones
                                                    .get_zone(&zone_name, Class::IN)
                                                    .cloned()
                                            };

                                            // Sign the unsigned zone and store it as a signed zone.
                                            let (soa_rr, records) = if let Some(unsigned_zone) =
                                                unsigned_zone
                                            {
                                                // Sign the zone and store the resulting RRs.

                                                // Temporary: Accumulate the zone into a vec as we can only sign
                                                // over a slice at the moment, not over an iterator yet (nor can
                                                // we iterate over a zone yet, only walk it ...).

                                                let read = unsigned_zone.read();

                                                let answer = read
                                                    .query(zone_name.clone(), Rtype::SOA)
                                                    .unwrap();

                                                let (soa_ttl, soa_data) =
                                                    answer.content().first().unwrap();
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

                                                read.walk(Box::new(
                                                    move |owner, rrset, _at_zone_cut| {
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
                                                    },
                                                ));

                                                // Create a signing configuration.
                                                let mut signing_config =
                                                    SigningConfig::default(validity);

                                                // Then sign the zone adding the generated records to the
                                                // signer_generated_rrs collection, as we don't want to keep two
                                                // copies of the unsigned records, we already have those in the
                                                // zone.
                                                let mut records = Arc::into_inner(records)
                                                    .unwrap()
                                                    .into_inner()
                                                    .unwrap();
                                                if let Err(err) =
                                                    records.sign_zone(&mut signing_config, keys)
                                                {
                                                    error!("[{component_name}]: Failed to sign zone '{zone_name}': {err}");
                                                    continue;
                                                }
                                                (soa_rr, records)
                                            } else {
                                                unreachable!();
                                            };

                                            // Store the zone in the signed zone tree.
                                            // First see if the zone already exists,
                                            // and ensure we don't hold a read lock.
                                            let signed_zones = self.component.signed_zones().load();
                                            let mut zone = signed_zones
                                                .get_zone(zone_name, Class::IN)
                                                .cloned();

                                            if zone.is_none() {
                                                let zones = signed_zones.clone();
                                                let mut new_zones = Arc::unwrap_or_clone(zones);
                                                let new_zone =
                                                    ZoneBuilder::new(zone_name.clone(), Class::IN)
                                                        .build();
                                                new_zones.insert_zone(new_zone.clone()).unwrap();
                                                self.component
                                                    .signed_zones()
                                                    .store(Arc::new(new_zones));
                                                zone = Some(new_zone);
                                            };

                                            let zone = zone.unwrap();
                                            let zone_name = zone.apex_name().clone();
                                            let zone_serial = if let ZoneRecordData::Soa(soa_data) =
                                                soa_rr.data()
                                            {
                                                soa_data.serial()
                                            } else {
                                                unreachable!()
                                            };

                                            // Update the content of the zone.
                                            let mut updater = ZoneUpdater::new(zone).await.unwrap();
                                            updater
                                                .apply(ZoneUpdate::DeleteAllRecords)
                                                .await
                                                .unwrap();
                                            for rr in records.into_inner() {
                                                updater
                                                    .apply(ZoneUpdate::AddRecord(rr))
                                                    .await
                                                    .unwrap();
                                            }
                                            updater
                                                .apply(ZoneUpdate::Finished(soa_rr))
                                                .await
                                                .unwrap();

                                            self.gate
                                                .update_data(Update::ZoneSignedEvent {
                                                    zone_name,
                                                    zone_serial,
                                                })
                                                .await;
                                        }
                                    }
                                }

                                _ => { /* Not for us */ }
                            }
                        }

                        _ => { /* Nothing to do */ }
                    }
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

// #[async_trait]
// impl DirectUpdate for ZoneSigner {
//     async fn direct_update(&self, event: Update) {
//         info!(
//             "[{}]: Received event: {event:?}",
//             self.component.read().await.name()
//         );
//     }
// }

// impl AnyDirectUpdate for ZoneSigner {}

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
