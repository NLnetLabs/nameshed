use core::future::ready;

use std::any::Any;
use std::collections::HashMap;
use std::fs::File;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Weak};

use arc_swap::ArcSwap;
use bytes::BufMut;
use domain::base::iana::{Class, Rcode};
use domain::base::{Name, Rtype, Serial};
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::{self, DgramServer};
use domain::net::server::message::Request;
use domain::net::server::middleware::cookies::CookiesMiddlewareSvc;
use domain::net::server::middleware::edns::EdnsMiddlewareSvc;
use domain::net::server::middleware::mandatory::MandatoryMiddlewareSvc;
use domain::net::server::middleware::notify::NotifyMiddlewareSvc;
use domain::net::server::service::{Service, ServiceError, ServiceResult};
use domain::net::server::stream::{self, StreamServer};
use domain::net::server::util::service_fn;
use domain::net::server::ConnectionConfig;
use domain::rdata::ZoneRecordData;
use domain::zonefile::inplace;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, StoredName, WritableZone,
    WritableZoneNode, Zone, ZoneStore, ZoneTree,
};
use futures::Future;
use log::{debug, error, info};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::Sender;
use tokio::time::Instant;

#[cfg(feature = "tls")]
use tokio_rustls::rustls::ServerConfig;

use crate::common::light_weight_zone::LightWeightZone;
use crate::common::net::ListenAddr;
use crate::common::tsig::{parse_key_strings, TsigKeyStore};
use crate::common::xfr::parse_xfr_acl;
use crate::comms::Terminated;
use crate::new::{Center, CenterEvent};
use crate::zonemaintenance::maintainer::{
    Config, DefaultConnFactory, TypedZone, ZoneMaintainer,
};
use crate::zonemaintenance::types::{
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig,
    XfrStrategy, ZoneConfig,
};

#[derive(Clone, Debug)]
pub struct ZoneLoaderConfig {
    /// Addresses and protocols to listen on.
    pub listen: Vec<ListenAddr>,

    /// The zone names and (if primary) corresponding zone file paths to load.
    pub zones: Arc<HashMap<String, String>>,

    /// XFR in per zone: Allow NOTIFY from, and when with a port also request XFR from.
    pub xfr_in: Arc<HashMap<String, String>>,

    /// TSIG keys.
    pub tsig_keys: HashMap<String, String>,
}

pub struct ZoneLoader {
    center: Weak<Center>,
    config: ZoneLoaderConfig,
    tsig_key_store: TsigKeyStore,
    xfr_in: Arc<ZoneMaintainer<TsigKeyStore, DefaultConnFactory>>,
}

impl ZoneLoader {
    const UNIT_NAME: &str = "zone-loader";

    pub fn new(
        center: Weak<Center>,
        config: ZoneLoaderConfig,
        tsig_key_store: TsigKeyStore,
        unsigned_zones: Arc<ArcSwap<ZoneTree>>,
    ) -> Result<Self, ()> {
        for (key_name, opt_alg_and_hex_bytes) in config.tsig_keys.iter() {
            let key = parse_key_strings(key_name, opt_alg_and_hex_bytes)
                .map_err(|err| {
                    error!(
                        "[{}]: Failed to parse TSIG key '{key_name}': {err}",
                        Self::UNIT_NAME,
                    );
                    ()
                })?;
            tsig_key_store.insert(key);
        }

        let maintainer_config =
            Config::<_, DefaultConnFactory>::new(tsig_key_store.clone());
        let zone_maintainer = Arc::new(
            ZoneMaintainer::new_with_config(maintainer_config)
                .with_zone_tree(unsigned_zones),
        );

        Ok(Self {
            center,
            config,
            tsig_key_store,
            xfr_in: zone_maintainer,
        })
    }

    async fn run(self) -> Result<(), crate::comms::Terminated> {
        let (zone_updated_tx, mut zone_updated_rx) =
            tokio::sync::mpsc::channel(10);

        // Define a server to handle NOTIFY messages and notify the
        // ZoneMaintainer on receipt, thereby triggering it to fetch the
        // latest version of the updated zone.
        let svc = service_fn(my_noop_service, ());
        let svc = NotifyMiddlewareSvc::new(svc, self.xfr_in.clone());
        let svc = CookiesMiddlewareSvc::with_random_secret(svc);
        let svc = EdnsMiddlewareSvc::new(svc);
        let svc = MandatoryMiddlewareSvc::new(svc);
        let svc = Arc::new(svc);

        for addr in self.config.listen.iter().cloned() {
            info!("[{}]: Binding on {:?}", Self::UNIT_NAME, addr);
            let svc = svc.clone();
            tokio::spawn(async move {
                if let Err(err) = Self::server(addr, svc).await {
                    error!("[{}]: {}", Self::UNIT_NAME, err);
                }
            });
        }

        let zone_maintainer_clone = self.xfr_in.clone();
        tokio::spawn(async move { zone_maintainer_clone.run().await });

        let mut notify_cfg = NotifyConfig { tsig_key: None };

        let mut xfr_cfg = XfrConfig {
            strategy: XfrStrategy::IxfrWithAxfrFallback,
            ixfr_transport: TransportStrategy::Tcp,
            compatibility_mode: CompatibilityMode::Default,
            tsig_key: None,
        };

        // Load primary zones.
        // Create secondary zones.
        for (zone_name, zone_path) in self.config.zones.iter() {
            let mut zone = if !zone_path.is_empty() {
                let before = Instant::now();

                // Load the specified zone file.
                info!(
                    "[{}]: Loading primary zone '{zone_name}' from '{zone_path}'..",
                    Self::UNIT_NAME,
                );
                let mut zone_file = File::open(zone_path)
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Failed to open zone file '{zone_path}': {err}",
                            Self::UNIT_NAME,
                        )
                    })
                    .map_err(|_| Terminated)?;

                // Don't use Zonefile::load() as it knows nothing about the
                // size of the original file so uses default allocation which
                // allocates more bytes than are needed. Instead control the
                // allocation size based on our knowledge of the file size.
                let zone_file_len = zone_file
                    .metadata()
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Failed to read metadata for file '{zone_path}': {err}",
                            Self::UNIT_NAME
                        )
                    })
                    .map_err(|_| Terminated)?
                    .len();
                let mut buf =
                    inplace::Zonefile::with_capacity(zone_file_len as usize)
                        .writer();
                std::io::copy(&mut zone_file, &mut buf)
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Failed to read data from file '{zone_path}': {err}",
                            Self::UNIT_NAME,
                        )
                    })
                    .map_err(|_| Terminated)?;
                let reader = buf.into_inner();

                let res = Zone::try_from(reader);
                let Ok(zone) = res else {
                    let errors = res.unwrap_err();
                    let mut msg = format!(
                        "Failed to parse zone: {} errors",
                        errors.len()
                    );
                    for (name, err) in errors.into_iter() {
                        msg.push_str(&format!("  {name}: {err}\n"));
                    }
                    error!(
                        "[{}]: Error parsing zone '{zone_name}': {}",
                        Self::UNIT_NAME,
                        msg
                    );
                    return Err(Terminated);
                };

                info!(
                    "Loaded {zone_file_len} bytes from '{zone_path}' in {} secs",
                    before.elapsed().as_secs()
                );

                zone_updated_tx
                    .send((zone.apex_name().clone(), Serial::now()))
                    .await
                    .unwrap();

                zone
            } else {
                let apex_name = Name::from_str(zone_name)
                    .inspect_err(|err| {
                        error!(
                            "[{}]: Error: Invalid zone name '{zone_name}': {err}",
                            Self::UNIT_NAME,
                        )
                    })
                    .map_err(|_| Terminated)?;
                info!(
                    "[{}]: Adding secondary zone '{zone_name}'",
                    Self::UNIT_NAME
                );
                Zone::new(LightWeightZone::new(apex_name, true))
            };

            let mut zone_cfg = ZoneConfig::new();

            if let Some(xfr_in) = self.config.xfr_in.get(zone_name) {
                let src = parse_xfr_acl(
                    xfr_in,
                    &mut xfr_cfg,
                    &mut notify_cfg,
                    &self.tsig_key_store,
                )
                .map_err(|_| {
                    error!("[{}]: Error parsing XFR ACL", Self::UNIT_NAME);
                    Terminated
                })?;

                info!(
                    "[{}]: Allowing NOTIFY from {} for zone '{zone_name}'",
                    Self::UNIT_NAME,
                    src.ip()
                );
                zone_cfg
                    .allow_notify_from
                    .add_src(src.ip(), notify_cfg.clone());
                if src.port() != 0 {
                    info!(
                        "[{}]: Adding XFR primary {src} for zone '{zone_name}'",
                        Self::UNIT_NAME,
                    );
                    zone_cfg.request_xfr_from.add_dst(src, xfr_cfg.clone());
                }
            }

            let notify_on_write_zone =
                NotifyOnWriteZone::new(zone, zone_updated_tx.clone());
            zone = Zone::new(notify_on_write_zone);

            let zone = TypedZone::new(zone, zone_cfg);
            self.xfr_in.insert_zone(zone).await;
        }

        while let Some((zone_name, zone_serial)) =
            zone_updated_rx.recv().await
        {
            info!(
                "[{}]: Received a new copy of zone '{zone_name}' at serial {zone_serial}",
                Self::UNIT_NAME,
            );
            let Some(center) = self.center.upgrade() else {
                break;
            };
            center.process(CenterEvent::ZoneLoaded {
                zone_name,
                zone_serial,
            })
        }

        Err(Terminated)
    }

    async fn server<Svc>(
        addr: ListenAddr,
        svc: Svc,
    ) -> Result<(), std::io::Error>
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

fn my_noop_service(
    _request: Request<Vec<u8>, ()>,
    _meta: (),
) -> ServiceResult<Vec<u8>> {
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
    ) -> Pin<Box<dyn Future<Output = Box<dyn WritableZone>> + Send + Sync>>
    {
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
        Box<
            dyn Future<
                    Output = Result<
                        Box<dyn WritableZoneNode>,
                        std::io::Error,
                    >,
                > + Send
                + Sync,
        >,
    > {
        self.writable_zone.open(create_diff)
    }

    fn commit(
        &mut self,
        bump_soa_serial: bool,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<InMemoryZoneDiff>, std::io::Error>,
                > + Send
                + Sync,
        >,
    > {
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
