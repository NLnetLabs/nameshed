//! Running the daemon.
use core::future::pending;

use std::fs::File;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use tokio::net::{TcpListener, UdpSocket};
use tokio::runtime::Runtime;
use tracing_subscriber::EnvFilter;

use domain::utils::base64;
use domain::base::iana::{Class, Rcode};
use domain::base::name::Name;
use domain::base::wire::Composer;
use domain::base::ToName;
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::{self, DgramServer};
use domain::net::server::message::Request;
use domain::net::server::middleware::cookies::CookiesMiddlewareSvc;
use domain::net::server::middleware::edns::EdnsMiddlewareSvc;
use domain::net::server::middleware::mandatory::MandatoryMiddlewareSvc;
use domain::net::server::middleware::notify::NotifyMiddlewareSvc;
use domain::net::server::middleware::tsig::TsigMiddlewareSvc;
use domain::net::server::middleware::xfr::XfrMiddlewareSvc;
use domain::net::server::service::{CallResult, Service, ServiceResult};
use domain::net::server::stream::{self, StreamServer};
use domain::net::server::util::{mk_builder_for_target, service_fn};
use domain::net::server::ConnectionConfig;
use domain::tsig::{Algorithm, Key, KeyName};
use domain::zonefile::inplace;
use domain::zonetree::{Answer, Zone, ZoneBuilder};

use crate::archive::ArchiveZone;
use crate::config::{Config, ListenAddr};
use crate::error::ExitError;
use crate::process::Process;
use crate::zonemaintenance::maintainer::{
    DefaultConnFactory, TypedZone, ZoneLookup, ZoneMaintainer,
};
use crate::zonemaintenance::types::{
    CompatibilityMode, NotifyConfig, TransportStrategy, XfrConfig, XfrStrategy, ZoneConfig,
    ZoneMaintainerKeyStore,
};

pub fn prepare() -> Result<(), ExitError> {
    Process::init()?;
    Ok(())
}

#[allow(clippy::mutable_key_type)]
pub fn run(config: Config) -> Result<(), ExitError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_thread_ids(true)
        .try_init()
        .ok();

    let process = Process::new(config);
    process.switch_logging(false)?;

    if process.config().zones.is_empty() {
        eprintln!("Error: At least one zone must be specified in the config file");
        return Err(ExitError::Generic);
    }

    Runtime::new()
        .map_err(|_| ExitError::Generic)?
        .block_on(async move {
            let mut key_store = ZoneMaintainerKeyStore::new();

            let mut notify_cfg = NotifyConfig { tsig_key: None };

            let mut xfr_cfg = XfrConfig {
                strategy: XfrStrategy::IxfrWithAxfrFallback,
                ixfr_transport: TransportStrategy::Tcp,
                compatibility_mode: CompatibilityMode::Default,
                tsig_key: None,
            };

            for (key_name, opt_alg_and_hex_bytes) in process.config().tsig_keys.iter() {
                let key_parts: Vec<String> = opt_alg_and_hex_bytes
                    .split(':').map(ToString::to_string).collect();

                let (alg, base64) = match key_parts.len() {
                    1 => (Algorithm::Sha256, key_parts[1].clone()),
                    2 => {
                        let alg = Algorithm::from_str(&key_parts[0])
                        .inspect_err(|_| eprintln!("Error: '{}' is not a valid TSIG algorithm for key '{key_name}'", key_parts[0]))
                        .map_err(|_| ExitError::Generic)?;
                        (alg, key_parts[1].clone())
                    }
                    _ => {
                        eprintln!("Invalid TSIG key specification for key '{key_name}': Should be [<algorithm>]:<base64 bytes>");
                        return Err(ExitError::Generic);
                    }
                };

                let encoded_key_name =
                    KeyName::from_str(key_name).inspect_err(|err| eprintln!("Error: Invalid TSIG key name '{key_name}': {err}")).map_err(|_| ExitError::Generic)?;
                let secret =
                    base64::decode::<Vec<u8>>(&base64).inspect_err(|err| eprintln!("Error: Invalid base64 encoded TSIG key secret for key '{key_name}' with base64 '{base64}': {err}")).map_err(|_| ExitError::Generic)?;
                let key = Key::new(alg, &secret, encoded_key_name, None, None)
                    .inspect_err(|err| {
                        eprintln!("Error: Invalid TSIG key inputs for key '{key_name}': {err}")
                    }).map_err(|_| ExitError::Generic)?;

                println!("Adding TSIG key '{}' ({}) to the key store", key.name(), key.algorithm());

                let key_id = (key.name().clone(), key.algorithm());
                key_store.insert(key_id.clone(), key);
            }
 
            let key_store = Arc::new(key_store);
            let maintainer_config = crate::zonemaintenance::maintainer::Config::<
                _,
                DefaultConnFactory,
            >::new(key_store.clone());
            let zones = Arc::new(ZoneMaintainer::new_with_config(maintainer_config));

            for (zone_name, zone_path) in process.config().zones.iter() {
                let mut zone = if !zone_path.is_empty() {
                    // Load the specified zone file.
                    println!("Loading primary zone '{zone_name}' from '{zone_path}'..");
                    let mut zone_bytes = File::open(zone_path)
                        .inspect_err(|err| {
                            eprintln!("Error: Failed to open zone file at '{zone_path}': {err}")
                        })
                        .map_err(|_| ExitError::Generic)?;
                    let reader = inplace::Zonefile::load(&mut zone_bytes)
                        .inspect_err(|err| {
                            eprintln!("Error: Failed to load zone file from '{zone_path}': {err}")
                        })
                        .map_err(|_| ExitError::Generic)?;
                    let res = Zone::try_from(reader);
                    let Ok(zone) = res else {
                        let errors = res.unwrap_err();
                        let mut msg = format!("Failed to parse zone: {} errors", errors.len());
                        for (name, err) in errors.into_iter() {
                            msg.push_str(&format!("  {name}: {err}\n"));
                        }
                        eprintln!("{}", msg);
                        return Err(ExitError::Generic);
                    };
                    zone
                } else {
                    let apex_name = Name::from_str(zone_name)
                        .inspect_err(|err| {
                            eprintln!("Error: Invalid zone name '{zone_name}': {err}")
                        })
                        .map_err(|_| ExitError::Generic)?;
                    println!("Adding secondary zone '{zone_name}'");
                    let builder = ZoneBuilder::new(apex_name, Class::IN);
                    builder.build()
                };

                let mut zone_cfg = ZoneConfig::new();

                if let Some(xfr_in) = process.config().xfr_in.get(zone_name) {
                    let src = parse_xfr_acl(xfr_in, &mut xfr_cfg, &mut notify_cfg, &key_store)?;

                    println!("Allowing NOTIFY from {} for zone '{zone_name}'", src.ip());
                    zone_cfg
                        .allow_notify_from
                        .add_src(src.ip(), notify_cfg.clone());
                    if src.port() != 0 {
                        println!("Adding XFR primary {src} for zone '{zone_name}'");
                        zone_cfg.request_xfr_from.add_dst(src, xfr_cfg.clone());
                    }
                }

                if let Some(xfr_out) = process.config().xfr_out.get(zone_name) {
                    let dst = parse_xfr_acl(xfr_out, &mut xfr_cfg, &mut notify_cfg, &key_store)?;

                    if dst.port() != 0 {
                        println!("Adding XFR secondary {dst} for zone '{zone_name}'");
                        zone_cfg.send_notify_to.add_dst(dst, notify_cfg.clone());
                    }

                    println!("Allowing XFR requests from {} for zone '{zone_name}'", dst.ip());
                    zone_cfg.provide_xfr_to.add_src(dst.ip(), xfr_cfg.clone());
                }

                if let Some(xfr_store_path) = &process.config().xfr_store_path {
                    let archive_zone = ArchiveZone::new(zone, &xfr_store_path);
                    zone = Zone::new(archive_zone);
                }

                let zone = TypedZone::new(zone, zone_cfg);
                zones.insert_zone(zone).await.unwrap();
            }

            let max_concurrency = std::thread::available_parallelism().unwrap().get() / 2;

            let svc = service_fn(my_service, zones.clone());
            let svc =
                XfrMiddlewareSvc::<_, _, Option<Key>, _>::new(svc, zones.clone(), max_concurrency);
            let svc = NotifyMiddlewareSvc::new(svc, zones.clone());
            let svc = CookiesMiddlewareSvc::with_random_secret(svc);
            let svc = EdnsMiddlewareSvc::new(svc);
            let svc = MandatoryMiddlewareSvc::new(svc);
            let svc = TsigMiddlewareSvc::new(svc, key_store);
            let svc = Arc::new(svc);

            tokio::spawn(async move { zones.run().await });
        
            for addr in process.config().listen.iter().cloned() {
                eprintln!("Binding on {:?}", addr);
                let svc = svc.clone();
                tokio::spawn(async move {
                    if let Err(err) = server(addr, svc).await {
                        println!("{}", err);
                    }
                });
            }
            pending().await
        })
}

fn parse_xfr_acl(xfr_in: &str, xfr_cfg: &mut XfrConfig, notify_cfg: &mut NotifyConfig, key_store: &Arc<ZoneMaintainerKeyStore>) -> Result<SocketAddr, ExitError> {
    let parts: Vec<String> = xfr_in
    .split(" KEY ").map(ToString::to_string).collect();
    let (addr, key_name) = match parts.len() {
        1 => (&parts[0], None),
        2 => (&parts[0], Some(&parts[1])),
        _ => {
            eprintln!("Invalid XFR ACL specification: Should be <addr>[:<port>][ KEY <TSIG key name>]");
            return Err(ExitError::Generic);
        }    
    };
    let addr = SocketAddr::from_str(addr)
        .or(IpAddr::from_str(addr).map(|ip| SocketAddr::new(ip, 0)))
        .inspect_err(|err| eprintln!("Error: Invalid XFR ACL address '{addr}': {err}"))
        .map_err(|_| ExitError::Generic)?;

    xfr_cfg.tsig_key = None;
    notify_cfg.tsig_key = None;

    if let Some(key_name) = key_name {
        let encoded_key_name = KeyName::from_str(key_name).inspect_err(|err| eprintln!("Error: Invalid TSIG key name '{key_name}': {err}")).map_err(|_| ExitError::Generic)?;
        for (name, alg) in key_store.keys() {
            if name == &encoded_key_name {
                xfr_cfg.tsig_key = Some((encoded_key_name.clone(), *alg));
                notify_cfg.tsig_key = Some((encoded_key_name, *alg));
                break;
            }
        }
    }

    Ok(addr)
}

#[cfg(feature = "tls")]
mod tls {
    use std::{
        io,
        net::SocketAddr,
        task::{Context, Poll},
    };

    use domain::net::server::sock::AsyncAccept;
    use tokio::net::{TcpListener, TcpStream};

    pub struct RustlsTcpListener {
        listener: TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
    }

    impl RustlsTcpListener {
        pub fn new(listener: TcpListener, acceptor: tokio_rustls::TlsAcceptor) -> Self {
            Self { listener, acceptor }
        }
    }

    impl AsyncAccept for RustlsTcpListener {
        type Error = io::Error;
        type StreamType = tokio_rustls::server::TlsStream<TcpStream>;
        type Future = tokio_rustls::Accept<TcpStream>;

        #[allow(clippy::type_complexity)]
        fn poll_accept(
            &self,
            cx: &mut Context,
        ) -> Poll<Result<(Self::Future, SocketAddr), io::Error>> {
            TcpListener::poll_accept(&self.listener, cx)
                .map(|res| res.map(|(stream, addr)| (self.acceptor.accept(stream), addr)))
        }
    }
}

async fn server<Svc>(addr: ListenAddr, svc: Svc) -> Result<(), io::Error>
where
    Svc: Clone + Service + Send + Sync + 'static,
    <Svc as Service>::Future: Send,
    <Svc as Service>::Target: Composer + Default + Send + Sync,
    <Svc as Service>::Stream: Send,
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
            let sock = TcpListener::bind(addr).await?;
            let mut conn_config = ConnectionConfig::new();
            conn_config.set_max_queued_responses(10000);
            let mut config = stream::Config::new();
            config.set_connection_config(conn_config);
            let srv = StreamServer::with_config(sock, buf, svc, config);
            let srv = Arc::new(srv);
            srv.run().await;
        }
        #[cfg(feature = "tls")]
        ListenAddr::Tls(addr, config) => {
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let sock = TcpListener::bind(addr).await?;
            let sock = tls::RustlsTcpListener::new(sock, acceptor);
            let mut conn_config = ConnectionConfig::new();
            conn_config.set_max_queued_responses(10000);
            let mut config = stream::Config::new();
            config.set_connection_config(conn_config);
            let srv = StreamServer::with_config(sock, buf, svc, config);
            let srv = Arc::new(srv);
            srv.run().await;
        }
    }
    Ok(())
}

fn my_service<T: ZoneLookup>(request: Request<Vec<u8>>, zones: T) -> ServiceResult<Vec<u8>> {
    let question = request.message().sole_question().unwrap();
    let zones = zones.zones();
    let zone = zones
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
