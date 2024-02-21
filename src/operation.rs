//! Running the daemon.

use crate::config::{Config, ListenAddr};
use crate::error::ExitError;
use crate::process::Process;
use domain::base::iana::Class;
use domain::base::name::{Dname, FlattenInto};
use domain::base::Rtype;
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::DgramServer;
use domain::net::server::middleware::builder::MiddlewareBuilder;
use domain::net::server::service::{CallResult, Transaction};
use domain::net::server::stream::StreamServer;
use domain::net::server::util::{
    mk_builder_for_target, mk_service, MkServiceRequest, MkServiceResult,
};
use domain::zonefile::inplace::{Entry, Zonefile};
use domain::zonetree::{Answer, ZoneSet};
use futures::future::pending;
use std::fs::File;
use std::io;
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::{TcpListener, UdpSocket};
use tokio::runtime::Runtime;

pub fn prepare() -> Result<(), ExitError> {
    Process::init()?;
    Ok(())
}

#[allow(clippy::mutable_key_type)]
pub fn run(config: Config) -> Result<(), ExitError> {
    let process = Process::new(config);
    process.switch_logging(false)?;

    Runtime::new()
        .map_err(|_| ExitError::Generic)?
        .block_on(async move {
            let reader =
                Zonefile::load(&mut File::open("/etc/nsd/example.com.zone").unwrap()).unwrap();
            let mut zonefile = domain::zonefile::parsed::Zonefile::new(
                Dname::from_str("example.com.").unwrap(),
                Class::In,
            );
            for item in reader {
                match item.unwrap() {
                    Entry::Record(record) => {
                        zonefile.insert(record.flatten_into()).unwrap();
                    }
                    _ => panic!("unsupported item"),
                }
            }
            let zone = zonefile.into_zone_builder().unwrap().finalize();

            let mut zones = ZoneSet::new();

            zones.insert_zone(zone).unwrap();

            let zones = Arc::new(zones);

            for addr in process.config().listen.iter().cloned() {
                eprintln!("Binding on {:?}", addr);
                let zones = zones.clone();
                tokio::spawn(async move {
                    if let Err(err) = server(addr, zones).await {
                        println!("{}", err);
                    }
                });
            }
            pending().await
        })
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
        type Stream = tokio_rustls::Accept<TcpStream>;

        #[allow(clippy::type_complexity)]
        fn poll_accept(
            &self,
            cx: &mut Context,
        ) -> Poll<Result<(Self::Stream, SocketAddr), io::Error>> {
            TcpListener::poll_accept(&self.listener, cx)
                .map(|res| res.map(|(stream, addr)| (self.acceptor.accept(stream), addr)))
        }
    }
}

async fn server(addr: ListenAddr, zones: Arc<ZoneSet>) -> Result<(), io::Error> {
    let buf = VecBufSource;
    let middleware = MiddlewareBuilder::<Vec<u8>>::default().finish();
    let svc = mk_service(query::<Vec<u8>>, zones);
    match addr {
        ListenAddr::Udp(addr) => {
            let sock = UdpSocket::bind(addr).await?;
            let srv = DgramServer::new(sock, buf, svc);
            let srv = srv.with_middleware(middleware);
            let srv = Arc::new(srv);
            srv.run().await;
        }
        ListenAddr::Tcp(addr) => {
            let sock = TcpListener::bind(addr).await?;
            let srv = StreamServer::new(sock, buf, svc);
            let srv = srv.with_middleware(middleware);
            let srv = Arc::new(srv);
            srv.run().await;
        }
        #[cfg(feature = "tls")]
        ListenAddr::Tls(addr, config) => {
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let sock = TcpListener::bind(addr).await?;
            let sock = tls::RustlsTcpListener::new(sock, acceptor);
            let srv = StreamServer::new(sock, buf, svc);
            let srv = srv.with_middleware(middleware);
            let srv = Arc::new(srv);
            srv.run().await;
        }
    }
    Ok(())
}

type MyError = ();

fn query<Target>(
    msg: MkServiceRequest<Vec<u8>>,
    zones: Arc<ZoneSet>,
) -> MkServiceResult<Vec<u8>, MyError> {
    let qtype = msg.sole_question().unwrap().qtype();
    if qtype == Rtype::Axfr {
        let mut txn = Transaction::stream();

        let cloned_msg = msg.clone();
        let cloned_zones = zones.clone();
        txn.push(async move {
            let question = cloned_msg.sole_question().unwrap();
            let answer = cloned_zones
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None))
                .and_then(|zone| zone.query(question.qname(), Rtype::Soa).ok())
                .unwrap_or_else(|| Answer::refused());
            let builder = mk_builder_for_target();
            let additional = answer.to_message(&cloned_msg, builder);
            Ok(CallResult::new(additional))
        });

        let cloned_msg = msg.clone();
        let cloned_zones = zones.clone();
        txn.push(async move {
            let question = cloned_msg.sole_question().unwrap();
            let zone = cloned_zones
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));
            let answer = match zone {
                Some(zone) => zone.query(question.qname(), Rtype::Soa).unwrap(),
                None => Answer::refused(),
            };
            let builder = mk_builder_for_target();
            let additional = answer.to_message(&cloned_msg, builder);
            Ok(CallResult::new(additional))
        });
        Ok(txn)
    } else {
        let fut = async move {
            let question = msg.sole_question().unwrap();
            let zone = zones
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));
            let answer = match zone {
                Some(zone) => zone.query(question.qname(), question.qtype()).unwrap(),
                None => Answer::refused(),
            };

            let builder = mk_builder_for_target();
            let additional = answer.to_message(&msg, builder);
            Ok(CallResult::new(additional))
        };
        Ok(Transaction::single(Box::pin(fut)))
    }
}
