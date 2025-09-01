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
use std::io::Read;
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
use domain::crypto::kmip::KeyUrl;
use domain::crypto::kmip::{self, ClientCertificate, ConnectionSettings};
use domain::crypto::sign::{
    generate, FromBytesError, GenerateParams, KeyPair, SecretKeyBytes, SignError, SignRaw,
};
use domain::dep::kmip::client::pool::{ConnectionManager, SyncConnPool};
use domain::dnssec::common::parse_from_bind;
use domain::dnssec::sign::denial::config::DenialConfig;
use domain::dnssec::sign::denial::nsec::GenerateNsecConfig;
use domain::dnssec::sign::denial::nsec3::{GenerateNsec3Config, Nsec3ParamTtlMode};
use domain::dnssec::sign::error::SigningError;
use domain::dnssec::sign::keys::keyset::{KeySet, KeyType};
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
use domain::tsig::KeyStore;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::utils::base64;
use domain::zonefile::inplace::{self, Entry, Zonefile};
use domain::zonetree::types::{StoredRecordData, ZoneUpdate};
use domain::zonetree::update::ZoneUpdater;
use domain::zonetree::{ReadableZone, StoredName, StoredRecord, Zone, ZoneBuilder};
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
use tokio::task::spawn_blocking;
use tokio::time::{sleep, Instant};
#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;
use url::Url;

use crate::common::light_weight_zone::LightWeightZone;
use crate::common::net::{
    ListenAddr, StandardTcpListenerFactory, StandardTcpStream, TcpListener, TcpListenerFactory,
    TcpStreamWrapper,
};
use crate::common::tsig::{parse_key_strings, TsigKeyStore};
use crate::common::xfr::parse_xfr_acl;
use crate::comms::ApplicationCommand;
use crate::comms::{GraphStatus, Terminated};
use crate::manager::Component;
use crate::metrics::{self, util::append_per_router_metric, Metric, MetricType, MetricUnit};
use crate::payload::Update;
use crate::units::Unit;
use crate::zonemaintenance::maintainer::{Config, ZoneLookup};
use crate::zonemaintenance::maintainer::{DefaultConnFactory, TypedZone, ZoneMaintainer};
use crate::zonemaintenance::types::{
    serialize_duration_as_secs, serialize_instant_as_duration_secs, serialize_opt_duration_as_secs,
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore,
};
use core::sync::atomic::AtomicBool;

#[derive(Debug)]
pub struct ZoneSignerUnit {
    pub keys_path: PathBuf,

    pub rrsig_inception_offset_secs: u32,

    pub rrsig_expiration_offset_secs: u32,

    pub denial_config: TomlDenialConfig,

    pub treat_single_keys_as_csks: bool,

    pub use_lightweight_zone_tree: bool,

    pub max_concurrent_operations: usize,

    pub max_concurrent_rrsig_generation_tasks: usize,

    pub kmip_server_conn_settings: HashMap<String, KmipServerConnectionSettings>,

    pub update_tx: mpsc::UnboundedSender<Update>,

    pub cmd_rx: mpsc::UnboundedReceiver<ApplicationCommand>,
}

#[allow(dead_code)]
impl ZoneSignerUnit {
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
    pub async fn run(mut self, component: Component) -> Result<(), Terminated> {
        // TODO: metrics and status reporting

        // Create KMIP server connection pools.
        // Warning: This will block until the pools have established their
        // minimum number of connections or timed out.
        let expected_kmip_server_conn_pools = self.kmip_server_conn_settings.len();

        let kmip_servers: HashMap<String, SyncConnPool> = self.kmip_server_conn_settings.drain().filter_map(|(server_id, conn_settings)| {
            let _host_and_port = (conn_settings.server_addr.clone(), conn_settings.server_port);

            match ConnectionManager::create_connection_pool(
                server_id.clone(),
                Arc::new(conn_settings.clone().into()),
                10,
                Some(Duration::from_secs(60)),
                Some(Duration::from_secs(60)),
            ) {
                Ok(kmip_conn_pool) => {
                    match kmip_conn_pool.get() {
                        Ok(conn) => {
                            match conn.query() {
                                Ok(q) => {
                                    // TODO: Check if the server meets our
                                    // needs. We can't assume domain will do
                                    // that for us because domain doesn't know
                                    // which functions we need.
                                    info!("Established connection pool for KMIP server '{server_id}' reporting as '{}'", q.vendor_identification.unwrap_or_default());
                                    Some((server_id, kmip_conn_pool))
                                }
                                Err(err) => {
                                    error!("Failed to create usable connection pool for KMIP server '{server_id}': {err}");
                                    None
                                }
                            }
                        }
                        Err(err) => {
                            error!("Failed to create usable connection pool for KMIP server '{server_id}': {err}");
                            None
                        }
                    }
                }

                Err(err) => {
                    error!("Failed to create connection pool for KMIP server '{server_id}': {err}");
                    None
                }
            }
        }).collect();

        if kmip_servers.len() != expected_kmip_server_conn_pools {
            // TODO: This is a bit severe. There should be a cleaner way to abort.
            std::process::exit(1);
        }

        ZoneSigner::new(
            component,
            self.rrsig_inception_offset_secs,
            self.rrsig_expiration_offset_secs,
            self.denial_config,
            self.use_lightweight_zone_tree,
            self.max_concurrent_operations,
            self.max_concurrent_rrsig_generation_tasks,
            self.treat_single_keys_as_csks,
            self.update_tx,
            self.keys_path,
            kmip_servers,
        )
        .run(self.cmd_rx)
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
        let public_data = std::fs::read_to_string(key_path).map_err(|_| {
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
    inception_offset_secs: u32,
    expiration_offset: u32,
    denial_config: TomlDenialConfig,
    use_lightweight_zone_tree: bool,
    concurrent_operation_permits: Semaphore,
    max_concurrent_rrsig_generation_tasks: usize,
    signer_status: Arc<RwLock<ZoneSignerStatus>>,
    treat_single_keys_as_csks: bool,
    update_tx: mpsc::UnboundedSender<Update>,
    _keys_path: PathBuf,
    kmip_servers: HashMap<String, SyncConnPool>,
}

impl ZoneSigner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        component: Component,
        inception_offset_secs: u32,
        expiration_offset: u32,
        denial_config: TomlDenialConfig,
        use_lightweight_zone_tree: bool,
        max_concurrent_operations: usize,
        max_concurrent_rrsig_generation_tasks: usize,
        treat_single_keys_as_csks: bool,
        update_tx: mpsc::UnboundedSender<Update>,
        keys_path: PathBuf,
        kmip_servers: HashMap<String, SyncConnPool>,
    ) -> Self {
        Self {
            component,
            inception_offset_secs,
            expiration_offset,
            denial_config,
            use_lightweight_zone_tree,
            concurrent_operation_permits: Semaphore::new(max_concurrent_operations),
            max_concurrent_rrsig_generation_tasks,
            signer_status: Default::default(),
            treat_single_keys_as_csks,
            update_tx,
            _keys_path: keys_path,
            kmip_servers,
        }
    }

    async fn run(
        self,
        mut cmd_rx: mpsc::UnboundedReceiver<ApplicationCommand>,
    ) -> Result<(), crate::comms::Terminated> {
        while let Some(cmd) = cmd_rx.recv().await {
            info!("[ZS]: Received command: {cmd:?}");
            match &cmd {
                ApplicationCommand::Terminate => {
                    // self.status_reporter.terminated();
                    return Ok(());
                }

                ApplicationCommand::SignZone {
                    zone_name,
                    zone_serial, // TODO: the serial number is ignored, but is that okay?
                } => {
                    if let Err(err) = self.sign_zone(zone_name, zone_serial.is_none()).await {
                        error!("[ZS]: Signing of zone '{zone_name}' failed: {err}");
                    }
                }

                _ => { /* Not for us */ }
            }
        }

        Ok(())
    }

    /// Signs zone_name from the Manager::unsigned_zones zone collection,
    /// unless `resign_last_signed_zone_content` is true in which case
    /// it resigns the copy of the zone from the Manager::published_zones
    /// collection instead. An alternative way to do this would be to only
    /// read the right version of the unsigned zone, but that would only
    /// be possible if the unsigned zone were definitely a ZoneApex zone
    /// rather than a LightWeightZone (and XFR-in zones are LightWeightZone
    /// instances).
    async fn sign_zone(
        &self,
        zone_name: &StoredName,
        resign_last_signed_zone_content: bool,
    ) -> Result<(), String> {
        // TODO: Implement serial bumping (per policy, e.g. ODS 'keep', 'counter', etc.?)

        info!("[ZS]: Waiting to start signing operation for zone '{zone_name}'.");
        self.signer_status.write().await.enqueue(zone_name.clone());

        let _permit = self.concurrent_operation_permits.acquire().await.unwrap();
        info!("[ZS]: Starting signing operation for zone '{zone_name}'");

        //
        // Lookup the zone to sign.
        //
        let zone_to_sign = match resign_last_signed_zone_content {
            false => {
                let unsigned_zones = self.component.unsigned_zones().load();
                unsigned_zones.get_zone(&zone_name, Class::IN).cloned()
            }
            true => {
                let published_zones = self.component.published_zones().load();
                published_zones.get_zone(&zone_name, Class::IN).cloned()
            }
        };
        let Some(unsigned_zone) = zone_to_sign else {
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
        trace!("[ZS]: Collecting records to sign for zone '{zone_name}'.");
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
                        debug!("Attempting to load private key '{priv_key_path}'.");

                        let private_key = ZoneSignerUnit::load_private_key(Path::new(priv_key_path))
                            .map_err(|_| format!("Failed to load private key from '{priv_key_path}'"))?;

                        let pub_key_path = pub_url.path();
                        debug!("Attempting to load public key '{pub_key_path}'.");

                        let public_key = ZoneSignerUnit::load_public_key(Path::new(pub_key_path))
                            .map_err(|_| format!("Failed to load public key from '{pub_key_path}'"))?;

                        let key_pair = KeyPair::from_bytes(&private_key, public_key.data())
                            .map_err(|err| format!("Failed to create key pair for zone {zone_name} using key files {pub_key_path} and {priv_key_path}: {err}"))?;
                        let signing_key =
                            SigningKey::new(zone_name.clone(), public_key.data().flags(), key_pair);

                        signing_keys.push(signing_key);
                    }

                    ("kmip", "kmip") => {
                        let priv_key_url = KeyUrl::try_from(priv_url).map_err(|err| format!("Invalid KMIP URL for private key: {err}"))?;
                        let pub_key_url = KeyUrl::try_from(pub_url).map_err(|err| format!("Invalid KMIP URL for public key: {err}"))?;

                        let kmip_conn_pool = self.kmip_servers
                            .get(priv_key_url.server_id())
                            .ok_or(format!("No connection pool available for KMIP server '{}'", priv_key_url.server_id()))?;

                        let _flags = priv_key_url.flags();

                        let key_pair = KeyPair::Kmip(kmip::sign::KeyPair::from_urls(
                            priv_key_url,
                            pub_key_url,
                            kmip_conn_pool.clone(),
                        ).map_err(|err| format!("Failed to create keypair for KMIP key IDs: {err}"))?);

                        let signing_key = SigningKey::new(zone_name.clone(), key_pair.flags(), key_pair);

                        signing_keys.push(signing_key);
                    }

                    (other1, other2) => return Err(format!("Using different key URI schemes ({other1} vs {other2}) for a public/private key pair is not supported.")),
                }

                debug!("Loaded key pair for zone {zone_name} from key pair");
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
        trace!("[ZS]: Sorting collected records for zone '{zone_name}'.");
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
        trace!("[ZS]: Generating denial records for zone '{zone_name}'.");
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
        trace!("[ZS]: Generating RRSIG records.");
        let rrsig_start = Instant::now();

        // Work out how many RRs have to be signed and how many concurrent
        // threads to sign with and how big each chunk to be signed should be.
        let rr_count = RecordsIter::new(&unsigned_records).count();
        let (parallelism, chunk_size) = self.determine_signing_concurrency(rr_count);
        info!("SIGNER: Using {parallelism} threads to sign {rr_count} owners in chunks of {chunk_size}.",);

        self.signer_status.write().await.update(zone_name, |s| {
            s.threads_used = Some(parallelism);
        });

        // Create a zone updater which will be used to add RRs resulting
        // from RRSIG generation to the signed zone. We set the create_diff
        // argument to false because we sign the zone by deleting all records
        // so from the point of view of the automatic diff creation logic all
        // records added to the zone appear to be new. Once we add support for
        // incremental signing (i.e. only regenerate, add and remove RRSIGs,
        // and update the NSEC(3) chain as needed, we can capture a diff of
        // the changes we make).
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
        let signing_ok = Arc::new(AtomicBool::new(true));
        let signing_ok_inner = signing_ok.clone();
        let rrsig_generation_complete = spawn_blocking(move || {
            let records_ref = &unsigned_records;

            rayon::scope(|scope| {
                for thread_idx in 0..parallelism {
                    let is_last_chunk = thread_idx == parallelism - 1;
                    let rrsig_cfg = &rrsig_cfg;
                    let tx = tx.clone();
                    let keys = keys.clone();
                    let zone_name = passed_zone_name.clone();
                    let signing_ok_inner = signing_ok_inner.clone();

                    scope.spawn(move |_| {
                        if let Err(err) = sign_rr_chunk(
                            is_last_chunk,
                            chunk_size,
                            records_ref,
                            thread_idx,
                            rrsig_cfg,
                            tx,
                            keys,
                            &zone_name,
                            treat_single_keys_as_csks,
                        ) {
                            error!(
                                "Signing of zone {zone_name} failed in thread {thread_idx}: {err}"
                            );
                            signing_ok_inner.store(false, std::sync::atomic::Ordering::SeqCst);
                        };
                    })
                }

                drop(tx);
            });

            unsigned_records
        });

        // Wait for RRSIG generation to complete.
        let unsigned_records = rrsig_generation_complete.await.unwrap();

        if !signing_ok.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("RRSIG generation error".to_string());
        }

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
            // TODO: Make this a floating point division?
            // f64::try_from(usize) isn't possible.
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
        self.update_tx
            .send(Update::ZoneSignedEvent {
                zone_name: zone_name.clone(),
                zone_serial,
            })
            .unwrap();

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
                _toml_nsec3_config,
                _toml_nsec_to_nsec3_transition_state,
            ) => todo!(),
            TomlDenialConfig::TransitioningFromNsec3(
                _toml_nsec3_config,
                _toml_nsec3_to_nsec_transition_state,
            ) => todo!(),
        };

        let _add_used_dnskeys = true;
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
    keys: Arc<Vec<SigningKey<Bytes, KeyPair>>>,
    zone_name: &StoredName,
    treat_single_keys_as_csks: bool,
) -> Result<(), String> {
    let mut iter = RecordsIter::new(records);
    let mut n = 0;
    let mut m = 0;
    for _ in 0..thread_idx * chunk_size {
        let Some(owner_rrs) = iter.next() else {
            trace!("SIGNER: Thread {thread_idx} ran out of data after skipping {n} owners covering {m} RRs!");
            return Ok(());
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

        let rrsigs = sign_sorted_zone_records(zone_name, RecordsIter::new(slice), &zsks, rrsig_cfg)
            .map_err(|err| err.to_string())?;
        duration = duration.saturating_add(before.elapsed());

        if !rrsigs.is_empty() {
            // trace!("SIGNER: Thread {i}: sending {} DNSKEY RRs and {} RRSIG RRs to be stored", res.dnskeys.len(), res.rrsigs.len());
            if let Err(err) = tx.blocking_send((rrsigs, duration)) {
                return Err(format!("Unable to send RRs for storage, aborting: {err}"));
            }
        } else {
            // trace!("SIGNER: Thread {i}: no DNSKEY RRs or RRSIG RRs to be stored");
        }
    }

    trace!("SIGNER: Thread {thread_idx} finished processing {n} owners covering {m} RRs.");
    Ok(())
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
    let start = Instant::now();

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
        if rrsig_count % 100 == 0 {
            let elapsed = start.elapsed().as_secs();
            let rate = if elapsed > 0 {
                rrsig_count / (elapsed as usize)
            } else {
                rrsig_count
            }; // TODO: Use floating point arithmetic?
            info!("Inserted {rrsig_count} RRSIGs in {elapsed} seconds at a rate of {rate} RRSIGs/second");
        }
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
        .map_err(|_| format!("SOA not found for zone '{zone_name}'"))?;
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

            // SKIP DNSSEC records that should be generated by the signing
            // process (these will be present if re-signing a published signed
            // zone rather than signing an unsigned zone)
            if matches!(
                rrset.rtype(),
                Rtype::DNSKEY | Rtype::RRSIG | Rtype::NSEC | Rtype::NSEC3
            ) {
                return;
            }

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
    #[allow(dead_code)]
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

//------------ DenialConfig --------------------------------------------------

// See: domain::sign::denial::config::DenialConfig
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TomlDenialConfig {
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
pub struct TomlNsec3Config {
    pub opt_out: TomlNsec3OptOut,
    pub nsec3_param_ttl_mode: TomlNsec3ParamTtlMode,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TomlNsec3OptOut {
    #[default]
    NoOptOut,
    OptOut,
    OptOutFlagOnly,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TomlNsec3ParamTtlMode {
    Fixed(Ttl),
    #[default]
    Soa,
    SoaMinimum,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TomlNsecToNsec3TransitionState {
    #[default]
    TransitioningDnsKeys,
    AddingNsec3Records,
    RemovingNsecRecords,
    Transitioned,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TomlNsec3ToNsecTransitionState {
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

#[derive(Clone, Debug)]
pub struct KmipServerConnectionSettings {
    /// Path to the client certificate file in PEM format
    pub client_cert_path: Option<PathBuf>,

    /// Path to the client certificate key file in PEM format
    pub client_key_path: Option<PathBuf>,

    /// Path to the client certificate and key file in PKCS#12 format
    pub client_pkcs12_path: Option<PathBuf>,

    /// Disable secure checks (e.g. verification of the server certificate)
    pub server_insecure: bool,

    /// Path to the server certificate file in PEM format
    pub server_cert_path: Option<PathBuf>,

    /// Path to the server CA certificate file in PEM format
    pub ca_cert_path: Option<PathBuf>,

    /// IP address, hostname or FQDN of the KMIP server
    pub server_addr: String,

    /// The TCP port number on which the KMIP server listens
    pub server_port: u16,

    /// The user name to authenticate with the KMIP server
    pub server_username: Option<String>,

    /// The password to authenticate with the KMIP server
    pub server_password: Option<String>,
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
        let client_cert = load_client_cert(&cfg);
        let _server_cert = cfg.server_cert_path.map(|p| load_binary_file(&p));
        let _ca_cert = cfg.ca_cert_path.map(|p| load_binary_file(&p));
        ConnectionSettings {
            host: cfg.server_addr,
            port: cfg.server_port,
            username: cfg.server_username,
            password: cfg.server_password,
            insecure: cfg.server_insecure,
            client_cert,
            server_cert: None,                             // TOOD
            ca_cert: None,                                 // TODO
            connect_timeout: Some(Duration::from_secs(5)), // TODO
            read_timeout: None,                            // TODO
            write_timeout: None,                           // TODO
            max_response_bytes: None,                      // TODO
        }
    }
}

fn load_client_cert(opt: &KmipServerConnectionSettings) -> Option<ClientCertificate> {
    match (
        &opt.client_cert_path,
        &opt.client_key_path,
        &opt.client_pkcs12_path,
    ) {
        (None, None, None) => None,
        (None, None, Some(path)) => Some(ClientCertificate::CombinedPkcs12 {
            cert_bytes: load_binary_file(path),
        }),
        (Some(_), None, None) | (None, Some(_), None) => {
            panic!("Client certificate authentication requires both a certificate and a key");
        }
        (_, Some(_), Some(_)) | (Some(_), _, Some(_)) => {
            panic!("Use either but not both of: client certificate and key PEM file paths, or a PCKS#12 certficate file path");
        }
        (Some(cert_path), Some(key_path), None) => Some(ClientCertificate::SeparatePem {
            cert_bytes: load_binary_file(cert_path),
            key_bytes: load_binary_file(key_path),
        }),
    }
}

pub fn load_binary_file(path: &Path) -> Vec<u8> {
    use std::{fs::File, io::Read};

    let mut bytes = Vec::new();
    File::open(path).unwrap().read_to_end(&mut bytes).unwrap();

    bytes
}
