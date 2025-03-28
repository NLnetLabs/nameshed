use core::fmt;
use core::future::pending;
use core::ops::{Add, ControlFlow};

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
use domain::base::iana::Class;
use domain::base::name::FlattenInto;
use domain::base::record::ComposeRecord;
use domain::base::wire::Composer;
use domain::base::{
    CanonicalOrd, Name, ParsedName, ParsedRecord, Record, Rtype, Serial, ToName, Ttl,
};
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
use domain::rdata::{Nsec3param, Soa, ZoneRecordData};
use domain::sign::crypto::common::{generate, GenerateParams};
// Use openssl::KeyPair because ring::KeyPair is not Send.
use domain::sign::crypto::openssl::KeyPair;
use domain::sign::denial::config::DenialConfig;
use domain::sign::denial::nsec::GenerateNsecConfig;
use domain::sign::denial::nsec3::{
    GenerateNsec3Config, Nsec3ParamTtlMode, OnDemandNsec3HashProvider,
};
use domain::sign::error::{FromBytesError, SigningError};
use domain::sign::keys::keymeta::IntendedKeyPurpose;
use domain::sign::keys::{DnssecSigningKey, SigningKey};
use domain::sign::records::{DefaultSorter, RecordsIter, RrsetIter, SortedRecords, Sorter};
use domain::sign::signatures::rrsigs::{generate_rrsigs, GenerateRrsigConfig, RrsigRecords};
use domain::sign::signatures::strategy::{
    DefaultSigningKeyUsageStrategy, FixedRrsigValidityPeriodStrategy,
};
use domain::sign::traits::SignableZoneInPlace;
use domain::sign::{SecretKeyBytes, SigningConfig};
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace;
use domain::zonetree::types::{StoredRecordData, ZoneUpdate};
use domain::zonetree::update::ZoneUpdater;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, StoredRecord, WritableZone, WritableZoneNode, Zone,
    ZoneBuilder, ZoneStore, ZoneTree,
};
use futures::future::{select, Either};
use futures::{pin_mut, Future, SinkExt};
use indoc::formatdoc;
use log::warn;
use log::{debug, error, info, trace};
use non_empty_vec::NonEmpty;
use octseq::{OctetsInto, Parser};
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
use tokio::task::spawn_blocking;

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
            self.rrsig_inception_offset_secs,
            self.rrsig_expiration_offset_secs,
            self.denial_config,
            self.use_lightweight_zone_tree,
            self.max_concurrent_operations,
            self.max_concurrent_rrsig_generation_tasks,
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
    signing_keys:
        Arc<std::sync::RwLock<HashMap<StoredName, Vec<DnssecSigningKey<Bytes, KeyPair>>>>>,
    inception_offset_secs: u32,
    expiration_offset: u32,
    denial_config: TomlDenialConfig,
    use_lightweight_zone_tree: bool,
    concurrent_operation_permits: Semaphore,
    max_concurrent_rrsig_generation_tasks: usize,
    signer_status: Arc<RwLock<ZoneSignerStatus>>,
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
        inception_offset_secs: u32,
        expiration_offset: u32,
        denial_config: TomlDenialConfig,
        use_lightweight_zone_tree: bool,
        max_concurrent_operations: usize,
        max_concurrent_rrsig_generation_tasks: usize,
    ) -> Self {
        Self {
            component,
            http_api_path,
            gate,
            metrics,
            status_reporter,
            signing_keys: Arc::new(std::sync::RwLock::new(signing_keys)),
            inception_offset_secs,
            expiration_offset,
            denial_config,
            use_lightweight_zone_tree,
            concurrent_operation_permits: Semaphore::new(max_concurrent_operations),
            max_concurrent_rrsig_generation_tasks,
            signer_status: Default::default(),
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
        let mut signing_config = self.signing_config();
        let mut rrsig_cfg = GenerateRrsigConfig::from(&signing_config);
        rrsig_cfg.zone_apex = Some(soa_rr.owner().clone());

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
        let unsigned_records = spawn_blocking(move || {
            // By not passing any keys to sign_zone() will only add denial RRs,
            // not RRSIGs. We could invoke generate_nsecs() or generate_nsec3s()
            // directly here instead.
            let no_keys: [DnssecSigningKey<Bytes, KeyPair>; 0] = Default::default();
            records.sign_zone(&mut signing_config, &no_keys)?;
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
        let (tx, rx) = mpsc::channel::<(RrsigRecords<StoredName, Bytes>, Duration)>(10000);

        // Start a background task that will insert RRs that it receives from
        // the RRSIG generator below.
        let inserts_complete = tokio::task::spawn(rrsig_inserter(updater, rx));

        // Generate RRSIGs concurrently.
        trace!("SIGNER: Generating RRSIGs concurrently..");
        let keys = self.signing_keys.clone();
        let passed_zone_name = zone_name.clone();

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

    fn signing_config(
        &self,
    ) -> SigningConfig<
        StoredName,
        Bytes,
        domain::sign::crypto::openssl::KeyPair,
        DefaultSigningKeyUsageStrategy,
        FixedRrsigValidityPeriodStrategy,
        MultiThreadedSorter,
        OnDemandNsec3HashProvider<Bytes>,
    > {
        let denial = match &self.denial_config {
            TomlDenialConfig::Nsec => DenialConfig::Nsec(Default::default()),
            TomlDenialConfig::Nsec3(nsec3) => {
                let first = parse_nsec3_config(&nsec3[0]);
                let rest = nsec3[1..].iter().map(parse_nsec3_config).collect();
                DenialConfig::Nsec3(first, rest)
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
        let inception = now.sub(self.inception_offset_secs);
        let expiration = now.add(self.expiration_offset);
        let rrsig_validity_period_strategy =
            FixedRrsigValidityPeriodStrategy::from((inception, expiration));
        SigningConfig::new(denial, add_used_dnskeys, rrsig_validity_period_strategy)
    }
}

fn sign_rr_chunk(
    is_last_chunk: bool,
    chunk_size: usize,
    records: &[Record<StoredName, StoredRecordData>],
    thread_idx: usize,
    rrsig_cfg: &GenerateRrsigConfig<
        StoredName,
        DefaultSigningKeyUsageStrategy,
        FixedRrsigValidityPeriodStrategy,
        MultiThreadedSorter,
    >,
    tx: Sender<(RrsigRecords<StoredName, Bytes>, Duration)>,
    keys: Arc<std::sync::RwLock<HashMap<StoredName, Vec<DnssecSigningKey<Bytes, KeyPair>>>>>,
    zone_name: StoredName,
) {
    let keys = keys.read().unwrap();
    let Some(keys) = keys.get(&zone_name) else {
        error!("No key found for zone '{zone_name}");
        return;
    };

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
        let res = generate_rrsigs(RecordsIter::new(slice), keys, rrsig_cfg).unwrap();
        duration = duration.saturating_add(before.elapsed());

        if !res.dnskeys.is_empty() || !res.rrsigs.is_empty() {
            // trace!("SIGNER: Thread {i}: sending {} DNSKEY RRs and {} RRSIG RRs to be stored", res.dnskeys.len(), res.rrsigs.len());
            if tx.blocking_send((res, duration)).is_err() {
                trace!("SIGNER: Thread {thread_idx}: unable to send RRs for storage, aborting.");
                break;
            }
        } else {
            // trace!("SIGNER: Thread {i}: no DNSKEY RRs or RRSIG RRs to be stored");
        }
    }
    trace!("SIGNER: Thread {thread_idx} finished processing {n} owners covering {m} RRs.");
}

async fn rrsig_inserter(
    mut updater: ZoneUpdater<StoredName>,
    mut rx: Receiver<(RrsigRecords<StoredName, Bytes>, Duration)>,
) -> (ZoneUpdater<StoredName>, u64, Duration, usize) {
    trace!("SIGNER: Adding new signed records to new/existing copy of signed zone.");
    let mut dnskeys_count = 0usize;
    let mut rrsig_count = 0usize;
    let mut max_rrsig_generation_time = Duration::ZERO;
    let mut insertion_time = Duration::ZERO;

    while let Some((rrsig_records, duration)) = rx.recv().await {
        max_rrsig_generation_time = std::cmp::max(max_rrsig_generation_time, duration);

        let insert_start = Instant::now();
        for rr in rrsig_records.dnskeys {
            updater
                .apply(ZoneUpdate::AddRecord(Record::from_record(rr)))
                .await
                .unwrap();
            dnskeys_count += 1;
        }

        for rr in rrsig_records.rrsigs {
            updater
                .apply(ZoneUpdate::AddRecord(Record::from_record(rr)))
                .await
                .unwrap();
            rrsig_count += 1;
        }

        insertion_time = insertion_time.saturating_add(insert_start.elapsed());
    }
    trace!("SIGNER: Added {dnskeys_count} DNSKEY RRs and {rrsig_count} RRSIG RRs to new/existing copy of signed zone.");

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

fn parse_nsec3_config(
    config: &TomlNsec3Config,
) -> GenerateNsec3Config<StoredName, Bytes, OnDemandNsec3HashProvider<Bytes>, MultiThreadedSorter> {
    let mut params = Nsec3param::default();
    if matches!(
        config.opt_out,
        TomlNsec3OptOut::OptOut | TomlNsec3OptOut::OptOutFlagOnly
    ) {
        params.set_opt_out_flag()
    }
    let hash_provider = OnDemandNsec3HashProvider::new(
        params.hash_algorithm(),
        params.iterations(),
        params.salt().clone(),
    );
    let ttl_mode = match config.nsec3_param_ttl_mode {
        TomlNsec3ParamTtlMode::Fixed(ttl) => Nsec3ParamTtlMode::Fixed(ttl),
        TomlNsec3ParamTtlMode::Soa => Nsec3ParamTtlMode::Soa,
        TomlNsec3ParamTtlMode::SoaMinimum => Nsec3ParamTtlMode::SoaMinimum,
    };
    let mut nsec3_config = GenerateNsec3Config::new(params, hash_provider).with_ttl_mode(ttl_mode);
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
        request: hyper::Request<hyper::Body>,
    ) -> ControlFlow<hyper::Request<hyper::Body>, hyper::Response<hyper::Body>> {
        let req_path = request.uri().decoded_path();
        if request.method() == hyper::Method::GET {
            if req_path.starts_with(&*self.http_api_path) {
                if req_path == format!("{}status.json", *self.http_api_path) {
                    return ControlFlow::Continue(self.build_json_status_response().await)
                } else if req_path.ends_with("/status.json") {
                    let (_, parts) = req_path.split_at(self.http_api_path.len());
                    if let Some((zone_name, status_rel_url)) = parts.split_once('/') {
                        if status_rel_url == "status.json" {
                            return match self.build_json_zone_status_response(zone_name).await {
                                Some(r) => ControlFlow::Continue(r),
                                None => ControlFlow::Break(request),
                            };
                        }
                    }
                }
            }
        }
        ControlFlow::Break(request)
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

impl domain::sign::records::Sorter for MultiThreadedSorter {
    fn sort_by<N, D, F>(records: &mut Vec<Record<N, D>>, compare: F)
    where
        F: Fn(&Record<N, D>, &Record<N, D>) -> Ordering + Sync,
        Record<N, D>: CanonicalOrd + Send,
    {
        records.par_sort_by(compare);
    }
}
