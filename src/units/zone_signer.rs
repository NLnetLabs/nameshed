use core::fmt;
use core::future::pending;
use core::ops::Add;

use std::any::Any;
use std::cmp::Ordering;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::ops::{ControlFlow, Sub};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use domain::base::iana::{nsec3, Class, Rcode};
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
use domain::rdata::{Nsec3param, Soa, ZoneRecordData};
use domain::sign::crypto::common::{generate, GenerateParams};
// Use openssl::KeyPair because ring::KeyPair is not Send.
use domain::sign::crypto::openssl::KeyPair;
use domain::sign::denial::config::DenialConfig;
use domain::sign::denial::nsec::GenerateNsecConfig;
use domain::sign::denial::nsec3::{
    GenerateNsec3Config, Nsec3OptOut, Nsec3ParamTtlMode, OnDemandNsec3HashProvider,
};
use domain::sign::error::FromBytesError;
use domain::sign::keys::keymeta::IntendedKeyPurpose;
use domain::sign::keys::{DnssecSigningKey, SigningKey};
use domain::sign::records::{RecordsIter, RrsetIter, SortedRecords, Sorter};
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
use domain::zonetree::types::ZoneUpdate;
use domain::zonetree::update::ZoneUpdater;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, WritableZone, WritableZoneNode, Zone, ZoneBuilder,
    ZoneStore, ZoneTree,
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
use serde::{Deserialize, Deserializer};
use serde_with::{serde_as, DeserializeFromStr, DisplayFromStr};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
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
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore,
};
use std::thread;

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
        )
        .run()
        .await?;

        Ok(())
    }

    fn default_http_api_path() -> Arc<String> {
        Arc::new("/zone-signer/".to_string())
    }

    fn default_rrsig_inception_offset_secs() -> u32 {
        60 * 90 // 90 minutes ala Knot
    }

    fn default_rrsig_expiration_offset_secs() -> u32 {
        60 * 60 * 24 * 14 // 14 days ala Knot
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
                                    rrsig_inception_offset_secs: inception_offset_secs,
                                    rrsig_expiration_offset_secs: expiration_offset_secs,
                                    denial_config,
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
                                    let start = Instant::now();

                                    // Find a key that matches the zone being signed.
                                    // TODO: We should support multiple keys, not just one.
                                    let now = Timestamp::now().into_int();
                                    let inception = now.sub(self.inception_offset_secs);
                                    let expiration = now.add(self.expiration_offset);
                                    let validity = FixedRrsigValidityPeriodStrategy::from((
                                        inception, expiration,
                                    ));

                                    let unsigned_zone = {
                                        let unsigned_zones = self.component.unsigned_zones().load();
                                        unsigned_zones.get_zone(&zone_name, Class::IN).cloned()
                                    };

                                    // Sign the unsigned zone and store it as a signed zone.
                                    /*let (soa_rr, records) =*/
                                    if let Some(unsigned_zone) = unsigned_zone {
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

                                        // Create a signing configuration.
                                        // Store the zone in the signed zone tree.
                                        // First see if the zone already exists,
                                        // and ensure we don't hold a read lock.
                                        let signed_zones = self.component.signed_zones().load();
                                        let mut zone =
                                            signed_zones.get_zone(zone_name, Class::IN).cloned();

                                        if zone.is_none() {
                                            trace!(
                                                "SIGNER: Creating new zone to hold signed records"
                                            );
                                            let zones = signed_zones.clone();
                                            let mut new_zones = Arc::unwrap_or_clone(zones);
                                            let new_zone = ZoneBuilder::new(zone_name.clone(), Class::IN).build();
                                            new_zones.insert_zone(new_zone.clone()).unwrap();
                                            self.component
                                                .signed_zones()
                                                .store(Arc::new(new_zones));
                                            zone = Some(new_zone);
                                        };

                                        let zone = zone.unwrap();
                                        let zone_name = zone.apex_name().clone();
                                        let zone_serial =
                                            if let ZoneRecordData::Soa(soa_data) = soa_rr.data() {
                                                soa_data.serial()
                                            } else {
                                                unreachable!()
                                            };

                                        // Update the content of the zone.
                                        let mut updater = ZoneUpdater::new(zone).await.unwrap();
                                        trace!("SIGNER: Deleting records in existing (if any) copy of signed zone.");
                                        updater.apply(ZoneUpdate::DeleteAllRecords).await.unwrap();

                                        let mut signing_config = self.signing_config();
                                        signing_config.set_rrsig_validity_period_strategy(validity);

                                        let mut rrsig_config = GenerateRrsigConfig::<
                                            StoredName,
                                            DefaultSigningKeyUsageStrategy,
                                            FixedRrsigValidityPeriodStrategy,
                                            MultiThreadedSorter,
                                        >::new(
                                            signing_config.rrsig_validity_period_strategy,
                                        );
                                        rrsig_config.add_used_dnskeys =
                                            signing_config.add_used_dnskeys;

                                        // Collect the zones unsigned records in zone walking order into an
                                        // unsorted Vec then sort it after by converting it to SortedRecords.
                                        // This is faster than using a SortedRecords to start with as it will
                                        // sort on every insert which is slow, and we can't call
                                        // SortedRecords::extend() (which is faster because it basically does
                                        // pushes then sort) because we don't have access to all of the
                                        // records at once to extend the collection with in one go.
                                        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
                                        let passed_records = records.clone();

                                        let component_name = component_name.clone();
                                        let zone_name2 = zone_name.clone();
                                        let res = tokio::task::spawn_blocking(move || {
                                                let walk_start = Instant::now();

                                                trace!("SIGNER: Walking");
                                                read.walk(Box::new(
                                                    move |owner, rrset, _at_zone_cut| {
                                                        let mut unlocked_records =
                                                            passed_records.lock().unwrap();
                                                        unlocked_records.extend(
                                                            rrset.data().iter().map(|rdata| {
                                                                Record::new(
                                                                    owner.clone(),
                                                                    Class::IN,
                                                                    rrset.ttl(),
                                                                    rdata.to_owned(),
                                                                )
                                                            }),
                                                        );
                                                    },
                                                ));

                                                let walk_time = walk_start.elapsed().as_secs();

                                                let sort_start = Instant::now();
                                                let records = Arc::into_inner(records)
                                                    .unwrap()
                                                    .into_inner()
                                                    .unwrap();
                                                let mut records = SortedRecords::from(records);
                                                let sort_time = sort_start.elapsed().as_secs();

                                                let unsigned_rr_count = records.len();

                                                trace!("SIGNER: Walked: accumulated {} records for signing", records.len());

                                                // TODO: Try generating Vec<u8> output instead of Bytes as then each
                                                // NSEC3 and RRSIG generated record may take up less space?

                                                // TODO: Generate one RRSIG at a time and push it straight into the
                                                // final zone instead of collecting in an intermediate Vec, could we
                                                // even do that from the walker and thus avoid a temporary Vec for
                                                // signing?

                                                // Then sign the zone adding the generated records to the
                                                // signer_generated_rrs collection, as we don't want to keep two
                                                // copies of the unsigned records, we already have those in the
                                                // zone.
                                                let nsec3_start = Instant::now();
                                                if let Err(err) =
                                                    // records.sign_zone(&mut signing_config, keys)
                                                    // Don't pass in the keys, then it only does NSEC3, not RRSIGs.
                                                    records.sign_zone(
                                                            &mut signing_config,
                                                            &Vec::<
                                                                DnssecSigningKey<
                                                                    Bytes,
                                                                    KeyPair,
                                                                >,
                                                            >::new(),
                                                        )
                                                {
                                                    error!("[{component_name}]: Failed to sign zone '{zone_name2}': {err}");
                                                    return (soa_rr, 0, 0, 0, 0, None);
                                                }

                                                let nsec3_time = nsec3_start.elapsed().as_secs();
                                                (soa_rr, unsigned_rr_count, walk_time, sort_time, nsec3_time, Some(records.into_inner()))
                                            }).await;

                                        let Ok((
                                            soa_rr,
                                            unsigned_rr_count,
                                            walk_time,
                                            sort_time,
                                            nsec3_time,
                                            Some(records),
                                        )) = res
                                        else {
                                            continue;
                                        };

                                        let nsec3_rr_count = records.len() - unsigned_rr_count;

                                        let soa_owner = soa_rr.owner().clone();
                                        rrsig_config.zone_apex = Some(soa_owner);

                                        trace!(
                                            "SIGNER: Signed: Post NSEC3 contains {} records",
                                            records.len()
                                        );

                                        // Now do RRSIGs in parallel.
                                        let n = RecordsIter::new(&records).count();
                                        // let n = records.len();
                                        // Relevant user suggestion:
                                        // "Misschien een tip voor NameShed:
                                        // Het aantal signerthreads dynamisch
                                        // maken, zodat de signer zelf extra
                                        // threads kan opstarten als er
                                        // geconstateerd wordt dat er veel
                                        // nieuwe sigs gemaakt moeten worden."
                                        let parallelism = if n < 1024 {
                                            if n >= 2 {
                                                2
                                            } else {
                                                1
                                            }
                                        } else {
                                            thread::available_parallelism().unwrap().get() - 1
                                        };
                                        let chunk_size = n / parallelism;
                                        trace!(
                                            "SIGNER: Using {parallelism} threads to sign {n} owners in chunks of {chunk_size}.",
                                        );

                                        // let mut out = Vec::<
                                        //     Option<RrsigRecords<StoredName, Bytes>>,
                                        // >::with_capacity(
                                        //     parallelism
                                        // );
                                        // for _ in 0..parallelism {
                                        //     out.push(None);
                                        // }
                                        // let out = Arc::new(std::sync::Mutex::new(out));

                                        let (tx, mut rx) =
                                            tokio::sync::mpsc::channel::<(
                                                RrsigRecords<Name<Bytes>, Bytes>,
                                                Duration,
                                            )>(10000);

                                        let join_handle = tokio::task::spawn(async move {
                                            trace!("SIGNER: Adding new signed records to new/existing copy of signed zone.");
                                            let mut dnskeys_count = 0usize;
                                            let mut rrsig_count = 0usize;
                                            let mut max_rrsig_generation_time = Duration::ZERO;
                                            let mut insertion_time = Duration::ZERO;

                                            while let Some((rrsig_records, duration)) =
                                                rx.recv().await
                                            {
                                                max_rrsig_generation_time = std::cmp::max(
                                                    max_rrsig_generation_time,
                                                    duration,
                                                );
                                                // trace!(
                                                //     "Received {} DNSKEY RRs and {} RRSIG RRs.",
                                                //     rrsig_records.dnskeys.len(),
                                                //     rrsig_records.rrsigs.len()
                                                // );
                                                let insert_start = Instant::now();
                                                for rr in rrsig_records.dnskeys {
                                                    updater
                                                        .apply(ZoneUpdate::AddRecord(
                                                            Record::from_record(rr),
                                                        ))
                                                        .await
                                                        .unwrap();
                                                    dnskeys_count += 1;
                                                }

                                                for rr in rrsig_records.rrsigs {
                                                    updater
                                                        .apply(ZoneUpdate::AddRecord(
                                                            Record::from_record(rr),
                                                        ))
                                                        .await
                                                        .unwrap();
                                                    rrsig_count += 1;
                                                }

                                                insertion_time = insertion_time.saturating_add(insert_start.elapsed());
                                            }
                                            trace!("SIGNER: Added {dnskeys_count} DNSKEY RRs and {rrsig_count} RRSIG RRs to new/existing copy of signed zone.");

                                            // eprintln!(
                                            //     "Max RRSIG generation time: {:.2?}, # signatures: {}",
                                            //     max_rrsig_generation_time, rrsigs_count
                                            // );

                                            let rrsig_time = max_rrsig_generation_time.as_secs();

                                            (updater, rrsig_time, insertion_time, rrsig_count)
                                        });

                                        trace!("SIGNER: Signing concurrently..");
                                        let keys = self.signing_keys.clone();
                                        let zone_name2 = zone_name.clone();

                                        let join_handle2 = tokio::task::spawn_blocking(move || {
                                            let records_ref = &records;
                                            rayon::scope(|s| {
                                                for i in 0..parallelism {
                                                    // let j = i;
                                                    let rrsig_config = &rrsig_config;
                                                    let tx2 = tx.clone();
                                                    let keys2 = keys.clone();
                                                    let zone_name3 = zone_name2.clone();
                                                    // let out = out.clone();
                                                    s.spawn(move |_| {
                                                            let is_last_chunk = i == parallelism-1;
                                                            let keys = keys2.read().unwrap();
                                                            let Some(keys) = keys.get(&zone_name3) else {
                                                                error!("No key found for zone '{zone_name3}");
                                                                return;
                                                            };
                                                            let mut iter =
                                                                RecordsIter::new(records_ref);
                                                            let mut n = 0;
                                                            let mut m = 0;
                                                            for _ in 0..i*chunk_size {
                                                                let Some(owner_rrs) = iter.next() else {
                                                                    trace!("SIGNER: Thread {i} ran out of data after skipping {n} owners covering {m} RRs!");
                                                                    return;
                                                                };
                                                                m += owner_rrs.into_inner().len();
                                                                n += 1;
                                                            }
                                                            trace!("SIGNER: Thread {i} skipped {n} owners covering {m} RRs.");
                                                            n = 0;
                                                            m = 0;
                                                            let mut duration = Duration::ZERO;
                                                            loop {
                                                                if !is_last_chunk && n == chunk_size {
                                                                    trace!("SIGNER: Thread {i} reached the end of the chunk.");
                                                                    break;
                                                                }
                                                                let Some(owner_rrs) = iter.next() else {
                                                                    trace!("SIGNER: Thread {i} reached the end of the data.");
                                                                    break;
                                                                };
                                                                let slice = owner_rrs.into_inner();
                                                                m += slice.len();
                                                                n += 1;
                                                                // trace!("SIGNER: Thread {i}: processing owner_rrs slice of len {}.", slice.len());
                                                                let before = Instant::now();
                                                                let res = generate_rrsigs(
                                                                    RecordsIter::new(
                                                                        slice,
                                                                    ),
                                                                    keys,
                                                                    rrsig_config,
                                                                )
                                                                .unwrap();
                                                                duration = duration.saturating_add(before.elapsed());

                                                                if !res.dnskeys.is_empty() || !res.rrsigs.is_empty() {
                                                                    // trace!("SIGNER: Thread {i}: sending {} DNSKEY RRs and {} RRSIG RRs to be stored", res.dnskeys.len(), res.rrsigs.len());
                                                                    if tx2.blocking_send((res, duration)).is_err() {
                                                                        trace!("SIGNER: Thread {i}: unable to send RRs for storage, aborting.");
                                                                        break;
                                                                    }
                                                                } else {
                                                                    // trace!("SIGNER: Thread {i}: no DNSKEY RRs or RRSIG RRs to be stored");
                                                                }
                                                            }
                                                            // let mut_out =
                                                            //     &mut out.lock().unwrap()[j];
                                                            // *mut_out = Some(res);

                                                            trace!("SIGNER: Thread {i} finished processing {n} owners covering {m} RRs.");
                                                        })
                                                }

                                                drop(tx);
                                            });

                                            records
                                        });

                                        // trace!("SIGNER: Extending result collection with concurrent results");
                                        // let mut out = std::sync::Mutex::into_inner(
                                        //     Arc::into_inner(out).unwrap(),
                                        // )
                                        // .unwrap();

                                        // let res = tokio::task::spawn_blocking(move || {
                                        //     use rayon::prelude::*;
                                        //     for i in 0..parallelism {
                                        //         let other = out.pop().unwrap().unwrap();
                                        //         let par_iter = other.rrsigs.into_par_iter();
                                        //         records.par_extend(
                                        //             par_iter.map(Record::from_record),
                                        //         );
                                        //     }

                                        //     records
                                        // })
                                        // .await;

                                        // let Ok(records) = res else {
                                        //     continue;
                                        // };

                                        let records = join_handle2.await.unwrap();

                                        // eprintln!(
                                        //     "Elapsed time: {:.2?}, # signatures: {}",
                                        //     before_rrsigs.elapsed(),
                                        //     n_sigs.load(std::sync::atomic::Ordering::SeqCst)
                                        // );

                                        let (mut updater, rrsig_time, insertion_time, rrsig_count) =
                                            join_handle.await.unwrap();

                                        let insert_start = Instant::now();
                                        for rr in records {
                                            updater
                                                .apply(ZoneUpdate::AddRecord(Record::from_record(
                                                    rr,
                                                )))
                                                .await
                                                .unwrap();
                                        }

                                        updater.apply(ZoneUpdate::Finished(soa_rr)).await.unwrap();
                                        let insertion_time = insertion_time.saturating_add(insert_start.elapsed()).as_secs();

                                        let total_time = start.elapsed().as_secs();
                                        let rrsig_avg = if rrsig_time == 0 { rrsig_count } else { rrsig_count / rrsig_time as usize };
                                        info!("[STATS] {zone_name} {zone_serial} RR[count={unsigned_rr_count} walk_time={walk_time}(sec) sort_time={sort_time}(sec)] DENIAL[count={denial_rr_count} time={denial_time}(sec)] RRSIG[new={rrsig_count} reused=0 time={rrsig_time}(sec) avg={rrsig_avg}(sig/sec)] INSERTION[time={insertion_time}(sec)] TOTAL[time={total_time}(sec)]");

                                        self.gate
                                            .update_data(Update::ZoneSignedEvent {
                                                zone_name,
                                                zone_serial,
                                            })
                                            .await;

                                        // trace!("SIGNER: Sorting collected results");
                                        // MultiThreadedSorter::sort_by(&mut records, CanonicalOrd::canonical_cmp);

                                        // (soa_rr, records)
                                    } else {
                                        unreachable!();
                                    };

                                    // trace!("SIGNER: Adding new signed records to new/existing copy of signed zone.");
                                    // for rr in records {
                                    //     updater
                                    //         .apply(ZoneUpdate::AddRecord(rr))
                                    //         .await
                                    //         .unwrap();
                                    // }
                                    // trace!("SIGNER: Finishing adding new signed records to new/existing copy of signed zone.");
                                    // updater
                                    //     .apply(ZoneUpdate::Finished(soa_rr))
                                    //     .await
                                    //     .unwrap();

                                    // self.gate
                                    //     .update_data(Update::ZoneSignedEvent {
                                    //         zone_name,
                                    //         zone_serial,
                                    //     })
                                    //     .await;
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
        // Validity period will be overridden when actually signing.
        let rrsig_validity_period_strategy = FixedRrsigValidityPeriodStrategy::from((0, 0));
        SigningConfig::new(denial, add_used_dnskeys, rrsig_validity_period_strategy)
    }
}

fn parse_nsec3_config(
    config: &TomlNsec3Config,
) -> GenerateNsec3Config<StoredName, Bytes, OnDemandNsec3HashProvider<Bytes>, MultiThreadedSorter> {
    let params = Nsec3param::default();
    let opt_out = match config.opt_out {
        TomlNsec3OptOut::NoOptOut => Nsec3OptOut::NoOptOut,
        TomlNsec3OptOut::OptOut => Nsec3OptOut::OptOut,
        TomlNsec3OptOut::OptOutFlagsOnly => Nsec3OptOut::OptOutFlagsOnly,
    };
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
    GenerateNsec3Config::new(params, opt_out, hash_provider).with_ttl_mode(ttl_mode)
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
    OptOutFlagsOnly,
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
