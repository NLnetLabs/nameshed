use core::fmt;
use core::future::pending;
use core::ops::{Add, ControlFlow};

use std::io::Read;
use std::any::Any;
use std::cmp::Ordering;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::ops::Sub;
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
use domain::base::iana::{Class, SecurityAlgorithm};
use domain::base::name::FlattenInto;
use domain::base::record::ComposeRecord;
use domain::base::wire::Composer;
use domain::base::{
    CanonicalOrd, Name, ParsedName, ParsedRecord, Record, Rtype, Serial, ToName, Ttl,
};
use domain::crypto::sign::{generate, FromBytesError, GenerateParams, SecretKeyBytes, SignRaw};
use domain::dnssec::common::parse_from_bind;
use domain::dnssec::sign::keys::SigningKey;
use domain::dnssec::sign::records::{DefaultSorter, RecordsIter, Rrset, Sorter};
use domain::dnssec::sign::signatures::rrsigs::{
    sign_rrset, sign_sorted_zone_records, GenerateRrsigConfig,
};
use domain::dnssec::sign::traits::SignableZoneInPlace;
use domain::dnssec::sign::SigningConfig;
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
use domain::rdata::nsec3::Nsec3Salt;
use domain::rdata::{Dnskey, Nsec3param, Rrsig, Soa, ZoneRecordData};
// Use openssl::KeyPair because ring::KeyPair is not Send.
use domain::crypto::openssl::sign::KeyPair;
use domain::dnssec::sign::denial::config::DenialConfig;
use domain::dnssec::sign::denial::nsec::GenerateNsecConfig;
use domain::dnssec::sign::denial::nsec3::{GenerateNsec3Config, Nsec3ParamTtlMode};
use domain::dnssec::sign::error::SigningError;
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace::{self, Entry, Zonefile};
use domain::zonetree::types::{StoredRecordData, ZoneUpdate};
use domain::zonetree::update::ZoneUpdater;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, StoredRecord, WritableZone, WritableZoneNode, Zone,
    ZoneBuilder, ZoneStore, ZoneTree,
};
use domain::dnssec::sign::keys::keyset::{KeySet, KeyType};
use domain::crypto::kmip::{self, ConnectionSettings};
use domain::crypto::kmip_pool::{ConnectionManager, KmipConnPool};
use futures::future::{select, Either};
use futures::{pin_mut, Future, SinkExt};
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use non_empty_vec::NonEmpty;
use octseq::{EmptyBuilder, OctetsInto, Parser};
use rayon::iter::{IntoParallelIterator, IntoParallelRefIterator, ParallelExtend};
use rayon::slice::ParallelSliceMut;
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{serde_as, DeserializeFromStr, DisplayFromStr};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::time::{sleep, Instant};
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;
use tokio::task::spawn_blocking;
use url::Url;

use crate::common::frim::FrimMap;
use crate::common::light_weight_zone::LightWeightZone;
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
    serialize_duration_as_secs, serialize_instant_as_duration_secs, serialize_opt_duration_as_secs,
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore,
};


#[serde_as]
#[derive(Clone, Debug, Deserialize)]
pub struct ZoneSignerUnit {
    /// The relative path at which we should listen for HTTP query API requests
    #[serde(default = "ZoneSignerUnit::default_http_api_path")]
    http_api_path: Arc<String>,

    keys_path: PathBuf,

    #[serde(default = "ZoneSignerUnit::default_rrsig_inception_offset_secs")]
    rrsig_inception_offset_secs: u32,

    #[serde(default = "ZoneSignerUnit::default_rrsig_expiration_offset_secs")]
    rrsig_expiration_offset_secs: u32,

    #[serde(default)]
    denial_config: TomlDenialConfig,

    #[serde(default)]
    treat_single_keys_as_csks: bool,

    #[serde(default)]
    use_lightweight_zone_tree: bool,

    #[serde(default = "ZoneSignerUnit::default_max_concurrent_operations")]
    max_concurrent_operations: usize,

    #[serde(default = "ZoneSignerUnit::default_max_concurrent_rrsig_generation_tasks")]
    max_concurrent_rrsig_generation_tasks: usize,

    #[serde(rename = "kmip_servers", default)]
    kmip_server_conn_settings: Vec<KmipServerConnectionSettings>,
}

impl ZoneSignerUnit {
    fn default_http_api_path() -> Arc<String> {
        Arc::new("/zone-signer/".to_string())
    }

    fn default_rrsig_inception_offset_secs() -> u32 {
        60 * 90 // 90 minutes ala Knot
    }

    fn default_rrsig_expiration_offset_secs() -> u32 {
        60 * 60 * 24 * 14 // 14 days ala Knot
    }

    fn default_max_concurrent_operations() -> usize {
        1
    }

    fn default_max_concurrent_rrsig_generation_tasks() -> usize {
        std::thread::available_parallelism().unwrap().get() - 1
    }
}

impl ZoneSignerUnit {
    pub async fn run(
        mut self,
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

        // Create KMIP server connection pools.
        // Warning: This will block until the pools have established their
        // minimum number of connections or timed out.
        let kmip_servers = self.kmip_server_conn_settings.drain(..).filter_map(|conn_settings| {
            let host_and_port = (conn_settings.server_addr.clone(), conn_settings.server_port);

            match ConnectionManager::create_connection_pool(
                Arc::new(conn_settings.clone().into()),
                10,
                Duration::from_secs(60),
                Duration::from_secs(60),
            ) {
                Ok(kmip_conn_pool) => {
                    Some((host_and_port, kmip_conn_pool))
                }

                Err(()) => {
                    error!("Failed to create connection pool for KMIP server '{}:{}', disabling use of this KMIP server", conn_settings.server_addr, conn_settings.server_port);
                    None
                }
            }
        }).collect();

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
            self.rrsig_inception_offset_secs,
            self.rrsig_expiration_offset_secs,
            self.denial_config,
            self.use_lightweight_zone_tree,
            self.max_concurrent_operations,
            self.max_concurrent_rrsig_generation_tasks,
            self.treat_single_keys_as_csks,
            self.keys_path,
            kmip_servers
        )
        .run()
        .await?;

        Ok(())
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

    fn load_public_key(key_path: &Path) -> Result<Record<StoredName, Dnskey<Bytes>>, Terminated> {
        let public_data = std::fs::read_to_string(key_path).map_err(|err| {
            error!("loading public key from file '{}'", key_path.display(),);
            Terminated
        })?;

        // Note: Compared to the original ldns-signzone there is a minor
        // regression here because at the time of writing the error returned
        // from parsing indicates broadly the type of parsing failure but does
        // note indicate the line number at which parsing failed.
        let public_key_info = parse_from_bind(&public_data).map_err(|err| {
            error!(
                "Unable to parse BIND formatted public key file '{}': {}",
                key_path.display(),
                err
            );
            Terminated
        })?;

        Ok(public_key_info)
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
    inception_offset_secs: u32,
    expiration_offset: u32,
    denial_config: TomlDenialConfig,
    use_lightweight_zone_tree: bool,
    concurrent_operation_permits: Semaphore,
    max_concurrent_rrsig_generation_tasks: usize,
    signer_status: Arc<RwLock<ZoneSignerStatus>>,
    treat_single_keys_as_csks: bool,
    keys_path: PathBuf,
    kmip_servers: HashMap<(String, u16), KmipConnPool>,
}

impl ZoneSigner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Component,
        http_api_path: Arc<String>,
        gate: Gate,
        metrics: Arc<ZoneSignerMetrics>,
        status_reporter: Arc<ZoneSignerStatusReporter>,
        inception_offset_secs: u32,
        expiration_offset: u32,
        denial_config: TomlDenialConfig,
        use_lightweight_zone_tree: bool,
        max_concurrent_operations: usize,
        max_concurrent_rrsig_generation_tasks: usize,
        treat_single_keys_as_csks: bool,
        keys_path: PathBuf,
        kmip_servers: HashMap<(String, u16), KmipConnPool>,
    ) -> Self {
        Self {
            component,
            http_api_path,
            gate,
            metrics,
            status_reporter,
            inception_offset_secs,
            expiration_offset,
            denial_config,
            use_lightweight_zone_tree,
            concurrent_operation_permits: Semaphore::new(max_concurrent_operations),
            max_concurrent_rrsig_generation_tasks,
            signer_status: Default::default(),
            treat_single_keys_as_csks,
            keys_path,
            kmip_servers,
        }
    }

    async fn run(mut self) -> Result<(), crate::comms::Terminated> {
        let component_name = self.component.name().clone();

        // Setup REST API endpoint
        let http_processor = Arc::new(SigningHistoryApi::new(
            self.http_api_path.clone(),
            self.signer_status.clone(),
        ));
        self.component
            .register_http_resource(http_processor.clone(), &self.http_api_path);

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
                                    rrsig_inception_offset_secs: inception_offset_secs,
                                    rrsig_expiration_offset_secs: expiration_offset_secs,
                                    denial_config,
                                    use_lightweight_zone_tree,
                                    max_concurrent_operations,
                                    max_concurrent_rrsig_generation_tasks,
                                    kmip_server_conn_settings,
                                }),
                        } => {
                            // Runtime reconfiguration of this unit has been
                            // requested.

                            // TODO

                            // Report that we have finished handling the reconfigure command
                            self.status_reporter.reconfigured();
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
                                    if let Err(err) =
                                        self.sign_zone(component_name.clone(), zone_name).await
                                    {
                                        error!("[{component_name}]: Signing of zone '{zone_name}' failed: {err}");
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

    async fn sign_zone(
        &self,
        component_name: Arc<str>,
        zone_name: &StoredName,
    ) -> Result<(), String> {
        // TODO: Implement serial bumping (per policy, e.g. ODS 'keep', 'counter', etc.?)

        info!("[{component_name}]: Waiting to start signing operation for zone '{zone_name}'.");
        self.signer_status.write().await.enqueue(zone_name.clone());

        let permit = self.concurrent_operation_permits.acquire().await.unwrap();
        info!("[{component_name}]: Starting signing operation for zone '{zone_name}'");

        //
        // Lookup the unsigned zone.
        //
        let unsigned_zone = {
            let unsigned_zones = self.component.unsigned_zones().load();
            unsigned_zones.get_zone(&zone_name, Class::IN).cloned()
        };
        let Some(unsigned_zone) = unsigned_zone else {
            return Err(format!("Unknown zone '{zone_name}'"));
        };
        let soa_rr = get_zone_soa(unsigned_zone.clone(), zone_name.clone())?;
        let ZoneRecordData::Soa(soa) = soa_rr.data() else {
            return Err(format!("SOA not found for zone '{zone_name}'"));
        };

        self.signer_status
            .write()
            .await
            .start(zone_name, soa.serial());

        //
        // Lookup the signed zone to update, or create a new empty zone to
        // sign into.
        //
        let zone = self.get_or_insert_signed_zone(zone_name);

        //
        // Create a signing configuration.
        //
        let signing_config = self.signing_config();
        let rrsig_cfg =
            GenerateRrsigConfig::new(signing_config.inception, signing_config.expiration);

        //
        // Convert zone records into a form we can sign.
        //
        trace!("[{component_name}]: Collecting records to sign for zone '{zone_name}'.");
        let walk_start = Instant::now();
        let passed_zone = unsigned_zone.clone();
        let mut records = spawn_blocking(|| collect_zone(passed_zone)).await.unwrap();
        let walk_time = walk_start.elapsed().as_secs();
        let unsigned_rr_count = records.len();

        self.signer_status.write().await.update(zone_name, |s| {
            s.unsigned_rr_count = Some(unsigned_rr_count);
            s.walk_time = Some(Duration::from_secs(walk_time));
        });

        /// Persistent state for the keyset command.
        /// Copied frmo the keyset branch of dnst.
        #[derive(Deserialize, Serialize)]
        struct KeySetState {
            /// Domain KeySet state.
            keyset: KeySet,

            dnskey_rrset: Vec<String>,
            ds_rrset: Vec<String>,
            cds_rrset: Vec<String>,
            ns_rrset: Vec<String>,
        }

        trace!("Reading dnst keyset DNSKEY RRs and RRSIG RRs");
        // Read the DNSKEY RRs and DNSKEY RRSIG RR from the keyset state.
        let apex_name = zone.apex_name().to_string();
        let state_path = Path::new("/tmp/").join(format!("{apex_name}.state"));
        let state = std::fs::read_to_string(&state_path).map_err(|err| format!("Unable to read `dnst keyset` state file '{}' while signing zone {zone_name}: {err}", state_path.display()))?;
        let state: KeySetState = serde_json::from_str(&state).unwrap();
        for dnskey_rr in state.dnskey_rrset {
            let mut zonefile = Zonefile::new();
            zonefile.extend_from_slice(dnskey_rr.as_bytes());
            zonefile.extend_from_slice(b"\n");
            if let Ok(Some(Entry::Record(rec))) = zonefile.next_entry() {
                eprintln!("Adding RR {dnskey_rr}");
                records.push(rec.flatten_into());
            }
        }

        trace!("Loading dnst keyset signing keys");
        // Load the signing keys indicated by the keyset state.
        let mut signing_keys = vec![];
        for (pub_key_name, key_info) in state.keyset.keys() {
            // Only use active ZSKs or CSKs to sign the records in the zone.
            if !matches!(key_info.keytype(),
                KeyType::Zsk(key_state)|KeyType::Csk(_, key_state) if key_state.signer())
            {
                continue;
            }

            if let Some(priv_key_name) = key_info.privref() {
                let priv_url = Url::parse(priv_key_name).expect("valid URL expected");
                let pub_url = Url::parse(pub_key_name).expect("valid URL expected");

                match (priv_url.scheme(), pub_url.scheme()) {
                    ("file", "file") => {
                        let priv_key_path = priv_url.path();
                        debug!("Attempting to load private key '{}'.", priv_key_path);
        
                        let private_key = ZoneSignerUnit::load_private_key(Path::new(priv_key_path))
                            .map_err(|_| format!("Failed to load private key from '{}'", priv_key_path))?;
        
                        let pub_key_path = pub_url.path();
                        debug!("Attempting to load public key '{}'.", pub_key_path);
        
                        let public_key = ZoneSignerUnit::load_public_key(Path::new(pub_key_path))
                            .map_err(|_| format!("Failed to load public key from '{}'", pub_key_path))?;
        
                        let key_pair = KeyPair::from_bytes(&private_key, &public_key.data())
                            .map_err(|err| format!("Failed to create key pair for zone {zone_name} using key files {pub_key_path} and {priv_key_path}: {err}"))?;
                        // We use OpenSSL rather than Ring as the Ring KeyPair
                        // cannot be shared across threads which breaks the
                        // parallel usage below via Rayon.
                        let key_pair = ThreadSafeKeyPair::OpenSSL(key_pair);
                        let signing_key =
                            SigningKey::new(zone_name.clone(), public_key.data().flags(), key_pair);
                        signing_keys.push(signing_key);
                    }

                    ("kmip", "kmip") => {
                        let (priv_key_id, priv_algorithm, priv_flags, priv_host_and_port) = parse_kmip_key_url(&priv_url)
                            .map_err(|()| format!("Failed to parse KMIP key URL '{priv_url}'"))?;
                        let (pub_key_id, pub_algorithm, pub_flags, pub_host_and_port) = parse_kmip_key_url(&pub_url)
                            .map_err(|()| format!("Failed to parse KMIP key URL '{pub_url}'"))?;

                        if priv_algorithm != pub_algorithm {
                            return Err(format!("Private and public key URLs have different algorithms: {priv_algorithm} vs {pub_algorithm}"));
                        } else if priv_flags != pub_flags {
                            return Err(format!("Private and public key URLs have different flags: {priv_flags} vs {pub_flags}"));
                        } else if priv_host_and_port != pub_host_and_port {
                            return Err(format!("Private and public key URLs have different host and port: {}:{} vs {}:{}",
                            priv_host_and_port.0, priv_host_and_port.1, pub_host_and_port.0, pub_host_and_port.1));
                        }

                        let kmip_conn_pool = self.kmip_servers
                            .get(&priv_host_and_port)
                            .ok_or(format!("No connection pool available for KMIP server '{}:{}'", priv_host_and_port.0, priv_host_and_port.1))?;

                        let key_pair = ThreadSafeKeyPair::Kmip(kmip::sign::KeyPair::new(
                            priv_algorithm,
                            priv_flags,
                            &priv_key_id,
                            &pub_key_id,
                            kmip_conn_pool.clone(),
                        ));
                    
                        let signing_key = SigningKey::new(zone_name.clone(), priv_flags, key_pair);
                        signing_keys.push(signing_key);
                    }

                    (other1, other2) => return Err(format!("Using different key URI schemes ({other1} vs {other2}) for a public/private key pair is not supported.")),
                }
            }
        }

        trace!("{} signing keys loaded", signing_keys.len());

        // TODO: If signing is disabled for a zone should we then allow the
        // unsigned zone to propagate through the pipeline?
        if signing_keys.is_empty() {
            warn!("No signing keys found for zone {zone_name}, aborting");
            return Ok(());
        }

        //
        // Sort them into DNSSEC order ready for NSEC(3) generation.
        //
        trace!("[{component_name}]: Sorting collected records for zone '{zone_name}'.");
        let sort_start = Instant::now();
        let mut records = spawn_blocking(|| {
            DefaultSorter::sort_by(&mut records, CanonicalOrd::canonical_cmp);
            records.dedup();
            records
        })
        .await
        .unwrap();
        let sort_time = sort_start.elapsed().as_secs();

        self.signer_status.write().await.update(zone_name, |s| {
            s.sort_time = Some(Duration::from_secs(sort_time));
        });

        //
        // Generate NSEC(3) RRs.
        //
        trace!("[{component_name}]: Generating denial records for zone '{zone_name}'.");
        let denial_start = Instant::now();
        let apex_owner = zone_name.clone();
        let unsigned_records = spawn_blocking(move || {
            // By not passing any keys to sign_zone() will only add denial RRs,
            // not RRSIGs. We could invoke generate_nsecs() or generate_nsec3s()
            // directly here instead.
            let no_keys: [&SigningKey<Bytes, KeyPair>; 0] = Default::default();
            records.sign_zone(&apex_owner, &signing_config, &no_keys)?;
            Ok(records)
        })
        .await
        .unwrap()
        .map_err(|err: SigningError| {
            format!("Failed to generate denial RRs for zone '{zone_name}': {err}")
        })?;
        let denial_time = denial_start.elapsed().as_secs();
        let denial_rr_count = unsigned_records.len() - unsigned_rr_count;

        self.signer_status.write().await.update(zone_name, |s| {
            s.denial_rr_count = Some(denial_rr_count);
            s.denial_time = Some(Duration::from_secs(denial_time));
        });

        //
        // Generate RRSIG RRs concurrently.
        //
        // Use N concurrent Rayon scoped threads to do blocking RRSIG
        // generation without interfering with Tokio task scheduling, and an
        // async task which receives generated RRSIGs via a Tokio
        // mpsc::channel and accumulates them into the signed zone.
        //
        trace!("[{component_name}]: Generating RRSIG records.");
        let rrsig_start = Instant::now();

        // Work out how many RRs have to be signed and how many concurrent
        // threads to sign with and how big each chunk to be signed should be.
        let rr_count = RecordsIter::new(&unsigned_records).count();
        let (parallelism, chunk_size) = self.determine_signing_concurrency(rr_count);
        trace!("SIGNER: Using {parallelism} threads to sign {rr_count} owners in chunks of {chunk_size}.",);

        self.signer_status.write().await.update(zone_name, |s| {
            s.threads_used = Some(parallelism);
        });

        // Create a zone updater which will be used to add RRs resulting from
        // RRSIG generation to the signed zone.
        let mut updater = ZoneUpdater::new(zone.clone(), false).await.unwrap();

        // Clear out any RRs in the current version of the signed zone. If the zone
        // supports versioning this is a NO OP.
        trace!("SIGNER: Deleting records in existing (if any) copy of signed zone.");
        updater.apply(ZoneUpdate::DeleteAllRecords).await.unwrap();

        // Create a channel for passing RRs generated by RRSIG generation to
        // a task that will insert them into the signed zone.
        let (tx, rx) =
            mpsc::channel::<(Vec<Record<StoredName, Rrsig<Bytes, StoredName>>>, Duration)>(10000);

        // Start a background task that will insert RRs that it receives from
        // the RRSIG generator below.
        let inserts_complete = tokio::task::spawn(rrsig_inserter(updater, rx));

        // Generate RRSIGs concurrently.
        trace!("SIGNER: Generating RRSIGs concurrently..");
        let keys = Arc::new(signing_keys);
        let passed_zone_name = zone_name.clone();
        let treat_single_keys_as_csks = self.treat_single_keys_as_csks;

        // Use spawn_blocking() to prevent blocking the Tokio executor. Pass
        // the unsigned records in, we get them back out again at the end.
        let rrsig_generation_complete = spawn_blocking(move || {
            let records_ref = &unsigned_records;

            //
            rayon::scope(|scope| {
                for thread_idx in 0..parallelism {
                    let is_last_chunk = thread_idx == parallelism - 1;
                    let rrsig_cfg = &rrsig_cfg;
                    let tx = tx.clone();
                    let keys = keys.clone();
                    let zone_name = passed_zone_name.clone();

                    scope.spawn(move |_| {
                        sign_rr_chunk(
                            is_last_chunk,
                            chunk_size,
                            records_ref,
                            thread_idx,
                            rrsig_cfg,
                            tx,
                            keys,
                            zone_name,
                            treat_single_keys_as_csks,
                        );
                    })
                }

                drop(tx);
            });

            unsigned_records
        });

        // Wait for RRSIG generation to complete.
        let unsigned_records = rrsig_generation_complete.await.unwrap();

        // Wait for RRSIG insertion to complete.
        let (mut updater, rrsig_time, insertion_time, rrsig_count) =
            inserts_complete.await.unwrap();

        self.signer_status.write().await.update(zone_name, |s| {
            s.rrsig_count = Some(rrsig_count);
            s.rrsig_reused_count = Some(0); // Not implemented yet
            s.rrsig_time = Some(Duration::from_secs(rrsig_time));
        });

        // Insert the unsigned records into the signed zone as well.
        let insert_start = Instant::now();
        for rr in unsigned_records {
            updater
                .apply(ZoneUpdate::AddRecord(Record::from_record(rr)))
                .await
                .unwrap();
        }

        // Finalize the signed zone update.
        let ZoneRecordData::Soa(soa_data) = soa_rr.data() else {
            unreachable!();
        };
        let zone_serial = soa_data.serial();

        updater.apply(ZoneUpdate::Finished(soa_rr)).await.unwrap();
        let insertion_time = insertion_time
            .saturating_add(insert_start.elapsed())
            .as_secs();

        let total_time = rrsig_start.elapsed().as_secs();
        let rrsig_avg = if rrsig_time == 0 {
            rrsig_count
        } else {
            rrsig_count / rrsig_time as usize
        };

        self.signer_status.write().await.update(zone_name, |s| {
            s.insertion_time = Some(Duration::from_secs(insertion_time));
            s.total_time = Some(Duration::from_secs(total_time));
        });

        self.signer_status.write().await.finish(zone_name);

        // Log signing statistics.
        info!("[STATS] {zone_name} {zone_serial} RR[count={unsigned_rr_count} walk_time={walk_time}(sec) sort_time={sort_time}(sec)] DENIAL[count={denial_rr_count} time={denial_time}(sec)] RRSIG[new={rrsig_count} reused=0 time={rrsig_time}(sec) avg={rrsig_avg}(sig/sec)] INSERTION[time={insertion_time}(sec)] TOTAL[time={total_time}(sec)] with {parallelism} threads");

        // Notify Central Command that we have finished.
        self.gate
            .update_data(Update::ZoneSignedEvent {
                zone_name: zone_name.clone(),
                zone_serial,
            })
            .await;

        Ok(())
    }

    fn determine_signing_concurrency(&self, rr_count: usize) -> (usize, usize) {
        // TODO: Relevant user suggestion: "Misschien een tip voor NameShed:
        // Het aantal signerthreads dynamisch maken, zodat de signer zelf
        // extra threads kan opstarten als er geconstateerd wordt dat er veel
        // nieuwe sigs gemaakt moeten worden."
        let parallelism = if rr_count < 1024 {
            if rr_count >= 2 {
                2
            } else {
                1
            }
        } else {
            self.max_concurrent_rrsig_generation_tasks
        };
        let parallelism = std::cmp::min(parallelism, self.max_concurrent_rrsig_generation_tasks);
        let chunk_size = rr_count / parallelism;
        (parallelism, chunk_size)
    }

    fn get_or_insert_signed_zone(&self, zone_name: &StoredName) -> Zone {
        // Create an empty zone to sign into if no existing signed zone exists.
        let signed_zones = self.component.signed_zones().load();

        signed_zones
            .get_zone(zone_name, Class::IN)
            .cloned()
            .unwrap_or_else(move || {
                let mut new_zones = Arc::unwrap_or_clone(signed_zones.clone());

                let new_zone = if self.use_lightweight_zone_tree {
                    Zone::new(LightWeightZone::new(zone_name.clone(), false))
                } else {
                    ZoneBuilder::new(zone_name.clone(), Class::IN).build()
                };

                new_zones.insert_zone(new_zone.clone()).unwrap();
                self.component.signed_zones().store(Arc::new(new_zones));

                new_zone
            })
    }

    fn signing_config(&self) -> SigningConfig<Bytes, MultiThreadedSorter> {
        let denial = match &self.denial_config {
            TomlDenialConfig::Nsec => DenialConfig::Nsec(Default::default()),
            TomlDenialConfig::Nsec3(nsec3) => {
                assert_eq!(
                    1,
                    nsec3.len().get(),
                    "Multiple NSEC3 configurations per zone is not yet supported"
                );
                let first = parse_nsec3_config(&nsec3[0]);
                DenialConfig::Nsec3(first)
            }
            TomlDenialConfig::TransitioningToNsec3(
                toml_nsec3_config,
                toml_nsec_to_nsec3_transition_state,
            ) => todo!(),
            TomlDenialConfig::TransitioningFromNsec3(
                toml_nsec3_config,
                toml_nsec3_to_nsec_transition_state,
            ) => todo!(),
        };

        let add_used_dnskeys = true;
        let now = Timestamp::now().into_int();
        let inception = now.sub(self.inception_offset_secs).into();
        let expiration = now.add(self.expiration_offset).into();
        SigningConfig::new(denial, inception, expiration)
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
fn sign_rr_chunk(
    is_last_chunk: bool,
    chunk_size: usize,
    records: &[Record<StoredName, StoredRecordData>],
    thread_idx: usize,
    rrsig_cfg: &GenerateRrsigConfig,
    tx: Sender<(Vec<Record<StoredName, Rrsig<Bytes, StoredName>>>, Duration)>,
    keys: Arc<Vec<SigningKey<Bytes, ThreadSafeKeyPair>>>,
    zone_name: StoredName,
    treat_single_keys_as_csks: bool,
) {
    let mut iter = RecordsIter::new(records);
    let mut n = 0;
    let mut m = 0;
    for _ in 0..thread_idx * chunk_size {
        let Some(owner_rrs) = iter.next() else {
            trace!("SIGNER: Thread {thread_idx} ran out of data after skipping {n} owners covering {m} RRs!");
            return;
        };
        m += owner_rrs.into_inner().len();
        n += 1;
    }
    trace!("SIGNER: Thread {thread_idx} skipped {n} owners covering {m} RRs.");

    let mut duration = Duration::ZERO;
    n = 0;
    m = 0;
    loop {
        if !is_last_chunk && n == chunk_size {
            trace!("SIGNER: Thread {thread_idx} reached the end of the chunk.");
            break;
        }
        let Some(owner_rrs) = iter.next() else {
            trace!("SIGNER: Thread {thread_idx} reached the end of the data.");
            break;
        };
        let slice = owner_rrs.into_inner();
        m += slice.len();
        n += 1;
        // trace!("SIGNER: Thread {i}: processing owner_rrs slice of len {}.", slice.len());
        let before = Instant::now();
        // TODO: It's stupid to build this same ZSK set every time this fn is called.
        let mut zsks = keys.iter().collect::<Vec<_>>();

        // TODO: This will not be correct if there are some KSK/ZSK pairs and
        // some lone KSKs, the lone KSKs will not be treated as ZSKs by this
        // simplistic approach.
        if treat_single_keys_as_csks && zsks.is_empty() {
            for k in keys.iter() {
                zsks.push(k);
            }
        }

        let rrsigs =
            sign_sorted_zone_records(&zone_name, RecordsIter::new(slice), &zsks, rrsig_cfg)
                .unwrap();
        duration = duration.saturating_add(before.elapsed());

        if !rrsigs.is_empty() {
            // trace!("SIGNER: Thread {i}: sending {} DNSKEY RRs and {} RRSIG RRs to be stored", res.dnskeys.len(), res.rrsigs.len());
            if tx.blocking_send((rrsigs, duration)).is_err() {
                trace!("SIGNER: Thread {thread_idx}: unable to send RRs for storage, aborting.");
                break;
            }
        } else {
            // trace!("SIGNER: Thread {i}: no DNSKEY RRs or RRSIG RRs to be stored");
        }
    }
    trace!("SIGNER: Thread {thread_idx} finished processing {n} owners covering {m} RRs.");
}

#[allow(clippy::type_complexity)]
async fn rrsig_inserter(
    mut updater: ZoneUpdater<StoredName>,
    mut rx: Receiver<(Vec<Record<StoredName, Rrsig<Bytes, StoredName>>>, Duration)>,
) -> (ZoneUpdater<StoredName>, u64, Duration, usize) {
    trace!("SIGNER: Adding new signed records to new/existing copy of signed zone.");
    let mut rrsig_count = 0usize;
    let mut max_rrsig_generation_time = Duration::ZERO;
    let mut insertion_time = Duration::ZERO;

    while let Some((rrsig_records, duration)) = rx.recv().await {
        max_rrsig_generation_time = std::cmp::max(max_rrsig_generation_time, duration);

        let insert_start = Instant::now();

        for rr in rrsig_records {
            updater
                .apply(ZoneUpdate::AddRecord(Record::from_record(rr)))
                .await
                .unwrap();
            rrsig_count += 1;
        }

        insertion_time = insertion_time.saturating_add(insert_start.elapsed());
    }
    trace!("SIGNER: Added {rrsig_count} RRSIG RRs to new/existing copy of signed zone.");

    let rrsig_time = max_rrsig_generation_time.as_secs();

    (updater, rrsig_time, insertion_time, rrsig_count)
}

fn get_zone_soa(
    zone: Zone,
    zone_name: StoredName,
) -> Result<Record<StoredName, StoredRecordData>, String> {
    let answer = zone
        .read()
        .query(zone_name.clone(), Rtype::SOA)
        .map_err(|err| format!("SOA not found for zone '{zone_name}'"))?;
    let (soa_ttl, soa_data) = answer
        .content()
        .first()
        .ok_or_else(|| format!("SOA not found for zone '{zone_name}'"))?;
    if !matches!(soa_data, ZoneRecordData::Soa(_)) {
        return Err(format!("SOA not found for zone '{zone_name}'"));
    };
    Ok(Record::new(zone_name.clone(), Class::IN, soa_ttl, soa_data))
}

fn collect_zone(zone: Zone) -> Vec<StoredRecord> {
    // Temporary: Accumulate the zone into a vec as we can only sign over a
    // slice at the moment, not over an iterator yet (nor can we iterate over
    // a zone yet, only walk it ...).
    let records = Arc::new(std::sync::Mutex::new(vec![]));
    let passed_records = records.clone();

    trace!("SIGNER: Walking");
    zone.read()
        .walk(Box::new(move |owner, rrset, _at_zone_cut| {
            let mut unlocked_records = passed_records.lock().unwrap();
            unlocked_records.extend(
                rrset.data().iter().map(|rdata| {
                    Record::new(owner.clone(), Class::IN, rrset.ttl(), rdata.to_owned())
                }),
            );
        }));

    let records = Arc::into_inner(records).unwrap().into_inner().unwrap();

    trace!(
        "SIGNER: Walked: accumulated {} records for signing",
        records.len()
    );

    records
}

fn parse_nsec3_config(config: &TomlNsec3Config) -> GenerateNsec3Config<Bytes, MultiThreadedSorter> {
    let mut params = Nsec3param::default();
    if matches!(
        config.opt_out,
        TomlNsec3OptOut::OptOut | TomlNsec3OptOut::OptOutFlagOnly
    ) {
        params.set_opt_out_flag()
    }
    let ttl_mode = match config.nsec3_param_ttl_mode {
        TomlNsec3ParamTtlMode::Fixed(ttl) => Nsec3ParamTtlMode::Fixed(ttl),
        TomlNsec3ParamTtlMode::Soa => Nsec3ParamTtlMode::Soa,
        TomlNsec3ParamTtlMode::SoaMinimum => Nsec3ParamTtlMode::SoaMinimum,
    };
    let mut nsec3_config = GenerateNsec3Config::new(params).with_ttl_mode(ttl_mode);
    if matches!(config.opt_out, TomlNsec3OptOut::OptOutFlagOnly) {
        nsec3_config = nsec3_config.without_opt_out_excluding_owner_names_of_unsigned_delegations();
    }
    nsec3_config
}

impl std::fmt::Debug for ZoneSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoneSigner").finish()
    }
}

//------------ ZoneSigningStatus ---------------------------------------------

#[derive(Copy, Clone, Serialize)]
struct RequestedStatus {
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    requested_at: tokio::time::Instant,
}

impl RequestedStatus {
    fn new() -> Self {
        Self {
            requested_at: Instant::now(),
        }
    }
}

#[derive(Copy, Clone, Serialize)]
struct InProgressStatus {
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    requested_at: tokio::time::Instant,
    zone_serial: Serial,
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    started_at: tokio::time::Instant,
    unsigned_rr_count: Option<usize>,
    #[serde(serialize_with = "serialize_opt_duration_as_secs")]
    walk_time: Option<Duration>,
    #[serde(serialize_with = "serialize_opt_duration_as_secs")]
    sort_time: Option<Duration>,
    denial_rr_count: Option<usize>,
    #[serde(serialize_with = "serialize_opt_duration_as_secs")]
    denial_time: Option<Duration>,
    rrsig_count: Option<usize>,
    rrsig_reused_count: Option<usize>,
    #[serde(serialize_with = "serialize_opt_duration_as_secs")]
    rrsig_time: Option<Duration>,
    #[serde(serialize_with = "serialize_opt_duration_as_secs")]
    insertion_time: Option<Duration>,
    #[serde(serialize_with = "serialize_opt_duration_as_secs")]
    total_time: Option<Duration>,
    threads_used: Option<usize>,
}

impl InProgressStatus {
    fn new(requested_status: RequestedStatus, zone_serial: Serial) -> Self {
        Self {
            requested_at: requested_status.requested_at,
            zone_serial,
            started_at: Instant::now(),
            unsigned_rr_count: None,
            walk_time: None,
            sort_time: None,
            denial_rr_count: None,
            denial_time: None,
            rrsig_count: None,
            rrsig_reused_count: None,
            rrsig_time: None,
            insertion_time: None,
            total_time: None,
            threads_used: None,
        }
    }
}

#[derive(Copy, Clone, Serialize)]
struct FinishedStatus {
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    requested_at: tokio::time::Instant,
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    started_at: tokio::time::Instant,
    zone_serial: Serial,
    unsigned_rr_count: usize,
    #[serde(serialize_with = "serialize_duration_as_secs")]
    walk_time: Duration,
    #[serde(serialize_with = "serialize_duration_as_secs")]
    sort_time: Duration,
    denial_rr_count: usize,
    #[serde(serialize_with = "serialize_duration_as_secs")]
    denial_time: Duration,
    rrsig_count: usize,
    rrsig_reused_count: usize,
    #[serde(serialize_with = "serialize_duration_as_secs")]
    rrsig_time: Duration,
    #[serde(serialize_with = "serialize_duration_as_secs")]
    insertion_time: Duration,
    #[serde(serialize_with = "serialize_duration_as_secs")]
    total_time: Duration,
    threads_used: usize,
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    finished_at: tokio::time::Instant,
}

impl FinishedStatus {
    fn new(in_progress_status: InProgressStatus) -> Self {
        Self {
            requested_at: in_progress_status.requested_at,
            zone_serial: in_progress_status.zone_serial,
            started_at: Instant::now(),
            unsigned_rr_count: in_progress_status.unsigned_rr_count.unwrap(),
            walk_time: in_progress_status.walk_time.unwrap(),
            sort_time: in_progress_status.sort_time.unwrap(),
            denial_rr_count: in_progress_status.denial_rr_count.unwrap(),
            denial_time: in_progress_status.denial_time.unwrap(),
            rrsig_count: in_progress_status.rrsig_count.unwrap(),
            rrsig_reused_count: in_progress_status.rrsig_reused_count.unwrap(),
            rrsig_time: in_progress_status.rrsig_time.unwrap(),
            insertion_time: in_progress_status.insertion_time.unwrap(),
            total_time: in_progress_status.total_time.unwrap(),
            threads_used: in_progress_status.threads_used.unwrap(),
            finished_at: Instant::now(),
        }
    }
}

#[derive(Copy, Clone, Serialize)]
enum ZoneSigningStatus {
    Requested(RequestedStatus),

    InProgress(InProgressStatus),

    Finished(FinishedStatus),
}

impl ZoneSigningStatus {
    fn new() -> Self {
        Self::Requested(RequestedStatus::new())
    }

    fn start(self, zone_serial: Serial) -> Self {
        match self {
            ZoneSigningStatus::Requested(s) => {
                Self::InProgress(InProgressStatus::new(s, zone_serial))
            }
            ZoneSigningStatus::InProgress(_) => self,
            ZoneSigningStatus::Finished(_) => {
                panic!("Cannot start a signing operation that already finished")
            }
        }
    }

    fn finish(self) -> Self {
        match self {
            ZoneSigningStatus::Requested(_) => {
                panic!("Cannot finish a signing operation that never started")
            }
            ZoneSigningStatus::InProgress(s) => Self::Finished(FinishedStatus::new(s)),
            ZoneSigningStatus::Finished(_) => self,
        }
    }
}

//------------ ZoneSignerStatus ----------------------------------------------

const MAX_SIGNING_HISTORY: usize = 100;

#[derive(Serialize)]
struct NamedZoneSigningStatus {
    zone_name: StoredName,
    status: ZoneSigningStatus,
}

#[derive(Serialize)]
struct ZoneSignerStatus {
    // Maps zone names to signing status, keeping records of previous signing.
    // Use VecDeque for its ability to act as a ring buffer: check size, if
    // at max desired capacity pop_front(), then in both cases push_back().
    zones_being_signed: VecDeque<NamedZoneSigningStatus>,
}

impl ZoneSignerStatus {
    pub fn get(&self, wanted_zone_name: &StoredName) -> Option<&NamedZoneSigningStatus> {
        self.zones_being_signed
            .iter()
            .rfind(|v| v.zone_name == wanted_zone_name)
    }

    fn get_mut(&mut self, wanted_zone_name: &StoredName) -> Option<&mut NamedZoneSigningStatus> {
        self.zones_being_signed
            .iter_mut()
            .rfind(|v| v.zone_name == wanted_zone_name)
    }

    pub fn enqueue(&mut self, zone_name: StoredName) {
        if self.zones_being_signed.len() == self.zones_being_signed.capacity() {
            // Discard oldest.
            let _ = self.zones_being_signed.pop_front();
        }
        self.zones_being_signed.push_back(NamedZoneSigningStatus {
            zone_name,
            status: ZoneSigningStatus::new(),
        });
    }

    pub fn start(&mut self, zone_name: &StoredName, zone_serial: Serial) {
        let res = self.get_mut(zone_name);
        if matches!(
            res,
            Some(NamedZoneSigningStatus {
                status: ZoneSigningStatus::Requested(..),
                ..
            })
        ) {
            let named_status = res.unwrap();
            named_status.status = named_status.status.start(zone_serial);
        }
    }

    pub fn update<F: Fn(&mut InProgressStatus)>(&mut self, zone_name: &StoredName, cb: F) {
        if let Some(NamedZoneSigningStatus {
            status: ZoneSigningStatus::InProgress(in_progress_status),
            ..
        }) = self.get_mut(zone_name)
        {
            // Only an existing unfinished status can be updated.
            cb(in_progress_status)
        }
    }

    pub fn finish(&mut self, zone_name: &StoredName) {
        let res = self.get_mut(zone_name);
        if matches!(
            res,
            Some(NamedZoneSigningStatus {
                status: ZoneSigningStatus::InProgress(..),
                ..
            })
        ) {
            let named_status = res.unwrap();
            named_status.status = named_status.status.finish();
        }
    }
}

impl Default for ZoneSignerStatus {
    fn default() -> Self {
        Self {
            zones_being_signed: VecDeque::with_capacity(MAX_SIGNING_HISTORY),
        }
    }
}

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

struct SigningHistoryApi {
    http_api_path: Arc<String>,
    signing_status: Arc<RwLock<ZoneSignerStatus>>,
}

impl SigningHistoryApi {
    fn new(http_api_path: Arc<String>, signing_status: Arc<RwLock<ZoneSignerStatus>>) -> Self {
        Self {
            http_api_path,
            signing_status,
        }
    }
}

#[async_trait]
impl ProcessRequest for SigningHistoryApi {
    async fn process_request(
        &self,
        request: &hyper::Request<hyper::Body>,
    ) -> Option<hyper::Response<hyper::Body>> {
        let req_path = request.uri().decoded_path();
        #[allow(clippy::collapsible_if)]
        if request.method() == hyper::Method::GET {
            if req_path.starts_with(&*self.http_api_path) {
                if req_path == format!("{}status.json", *self.http_api_path) {
                    return Some(self.build_json_status_response().await);
                } else if req_path.ends_with("/status.json") {
                    let (_, parts) = req_path.split_at(self.http_api_path.len());
                    if let Some((zone_name, status_rel_url)) = parts.split_once('/') {
                        if status_rel_url == "status.json" {
                            return self.build_json_zone_status_response(zone_name).await;
                        }
                    }
                }
            }
        }
        None
    }
}

impl SigningHistoryApi {
    pub async fn build_json_status_response(&self) -> hyper::Response<hyper::Body> {
        let json = serde_json::to_string(&*self.signing_status.read().await).unwrap();

        hyper::Response::builder()
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(hyper::Body::from(json))
            .unwrap()
    }

    pub async fn build_json_zone_status_response(
        &self,
        zone_name: &str,
    ) -> Option<hyper::Response<hyper::Body>> {
        if let Ok(zone_name) = StoredName::from_str(zone_name) {
            if let Some(status) = self.signing_status.read().await.get(&zone_name) {
                let json = serde_json::to_string(status).unwrap();
                return Some(
                    hyper::Response::builder()
                        .header("Content-Type", "application/json")
                        .header("Access-Control-Allow-Origin", "*")
                        .body(hyper::Body::from(json))
                        .unwrap(),
                );
            }
        }
        None
    }
}

//------------ DenialConfig --------------------------------------------------

// See: domain::sign::denial::config::DenialConfig
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TomlDenialConfig {
    #[default]
    Nsec,

    Nsec3(NonEmpty<TomlNsec3Config>),

    TransitioningToNsec3(TomlNsec3Config, TomlNsecToNsec3TransitionState),

    TransitioningFromNsec3(TomlNsec3Config, TomlNsec3ToNsecTransitionState),
}

// See: domain::sign::denial::config::GenerateNsec3Config
// Note: We don't allow configuration of NSEC3 salt, iterations or algorithm
// as they are fixed to best practice values.
#[derive(Clone, Debug, Default, Deserialize)]
struct TomlNsec3Config {
    pub opt_out: TomlNsec3OptOut,
    pub nsec3_param_ttl_mode: TomlNsec3ParamTtlMode,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TomlNsec3OptOut {
    #[default]
    NoOptOut,
    OptOut,
    OptOutFlagOnly,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TomlNsec3ParamTtlMode {
    Fixed(Ttl),
    #[default]
    Soa,
    SoaMinimum,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TomlNsecToNsec3TransitionState {
    #[default]
    TransitioningDnsKeys,
    AddingNsec3Records,
    RemovingNsecRecords,
    Transitioned,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TomlNsec3ToNsecTransitionState {
    #[default]
    AddingNsecRecords,
    RemovingNsec3ParamdRecord,
    RemovingNsec3Records,
    TransitioningDnsKeys,
    Transitioned,
}

//------------ MultiThreadedSorter -------------------------------------------

/// A parallelized sort implementation for use with [`SortedRecords`].
///
/// TODO: Should we add a `-j` (jobs) command line argument to override the
/// default Rayon behaviour of using as many threads as their are CPU cores?
struct MultiThreadedSorter;

impl domain::dnssec::sign::records::Sorter for MultiThreadedSorter {
    fn sort_by<N, D, F>(records: &mut Vec<Record<N, D>>, compare: F)
    where
        F: Fn(&Record<N, D>, &Record<N, D>) -> Ordering + Sync,
        Record<N, D>: CanonicalOrd + Send,
    {
        records.par_sort_by(compare);
    }
}

//------------ KMIP related --------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KmipServerConnectionSettings {
    /// Path to the client certificate file in PEM format
    #[serde(skip_serializing_if = "Option::is_none", default)]
    client_cert_path: Option<PathBuf>,

    /// Path to the client certificate key file in PEM format
    #[serde(skip_serializing_if = "Option::is_none", default)]
    client_key_path: Option<PathBuf>,

    /// Path to the client certificate and key file in PKCS#12 format
    #[serde(skip_serializing_if = "Option::is_none", default)]
    client_pkcs12_path: Option<PathBuf>,

    /// Disable secure checks (e.g. verification of the server certificate)
    #[serde(default)]
    server_insecure: bool,

    /// Path to the server certificate file in PEM format
    #[serde(skip_serializing_if = "Option::is_none", default)]
    server_cert_path: Option<PathBuf>,

    /// Path to the server CA certificate file in PEM format
    #[serde(skip_serializing_if = "Option::is_none", default)]
    ca_cert_path: Option<PathBuf>,

    /// IP address, hostname or FQDN of the KMIP server
    server_addr: String,

    /// The TCP port number on which the KMIP server listens
    server_port: u16,

    /// The user name to authenticate with the KMIP server
    #[serde(skip_serializing_if = "Option::is_none", default)]
    server_username: Option<String>,

    /// The password to authenticate with the KMIP server
    #[serde(skip_serializing_if = "Option::is_none", default)]
    server_password: Option<String>,
}

impl Default for KmipServerConnectionSettings {
    fn default() -> Self {
        Self {
            server_addr: "localhost".into(),
            server_port: 5696,
            server_insecure: false,
            client_cert_path: None,
            client_key_path: None,
            client_pkcs12_path: None,
            server_cert_path: None,
            ca_cert_path: None,
            server_username: None,
            server_password: None,
        }
    }
}

impl From<KmipServerConnectionSettings> for ConnectionSettings {
    fn from(cfg: KmipServerConnectionSettings) -> Self {
        ConnectionSettings {
            host: cfg.server_addr,
            port: cfg.server_port,
            username: cfg.server_username,
            password: cfg.server_password,
            insecure: cfg.server_insecure,
            client_cert: None,        // TODO
            server_cert: None,        // TODO
            ca_cert: None,            // TODO
            connect_timeout: None,    // TODO
            read_timeout: None,       // TODO
            write_timeout: None,      // TODO
            max_response_bytes: None, // TODO
        }
    }
}

fn parse_kmip_key_url(
    kmip_key_url: &Url,
) -> Result<(String, SecurityAlgorithm, u16, (String, u16)), ()> {
    let host_and_port = (
        kmip_key_url.host_str().ok_or(())?.to_string(),
        kmip_key_url.port().unwrap_or(5696)
    );

    // TODO: We ignore the username and password as we have all of the
    // connection details. We need our own KMIP server configs as the URL
    // doesn't contain everything we need, e.g. certificates... but having
    // config both here AND in `dnst keyset` is annoying... can we get rid of
    // this duplication somehow?

    let url_path = kmip_key_url.path().to_string();
    let (keys, key_id) = url_path.strip_prefix('/').ok_or(())?.split_once('/').unwrap();
    if keys != "keys" {
        return Err(());
    }
    let key_id = key_id.to_string();
    let mut flags = None;
    let mut algorithm = None;
    for (k, v) in kmip_key_url.query_pairs() {
        match &*k {
            "flags" => flags = Some(v.parse::<u16>().map_err(|_| ())?),
            "algorithm" => algorithm = Some(SecurityAlgorithm::from_str(&*v).map_err(|_| ())?),
            _ => { /* ignore unknown URL query parameter */ }
        }
    }
    let flags = flags.ok_or(())?;
    let algorithm = algorithm.ok_or(())?;
    Ok((key_id, algorithm, flags, host_and_port))
}

// Like domain::crypto::sign::KeyPair but omits Ring which is currently not
// thread safe, though it is expected that can be resolved and thus this type
// would no longer be needed.
#[derive(Debug)]
enum ThreadSafeKeyPair {
    OpenSSL(domain::crypto::openssl::sign::KeyPair),

    Kmip(domain::crypto::kmip::sign::KeyPair),
}

impl SignRaw for ThreadSafeKeyPair {
    fn algorithm(&self) -> SecurityAlgorithm {
        match self {
            ThreadSafeKeyPair::OpenSSL(key_pair) => key_pair.algorithm(),
            ThreadSafeKeyPair::Kmip(key_pair) => key_pair.algorithm(),
        }
    }

    fn dnskey(&self) -> Dnskey<Vec<u8>> {
        match self {
            ThreadSafeKeyPair::OpenSSL(key_pair) => key_pair.dnskey(),
            ThreadSafeKeyPair::Kmip(key_pair) => key_pair.dnskey(),
        }
    }

    fn sign_raw(&self, data: &[u8]) -> Result<domain::crypto::sign::Signature, domain::crypto::sign::SignError> {
        match self {
            ThreadSafeKeyPair::OpenSSL(key_pair) => key_pair.sign_raw(data),
            ThreadSafeKeyPair::Kmip(key_pair) => key_pair.sign_raw(data),
        }
    }
}