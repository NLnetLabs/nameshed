//! Running the daemon.

use crate::config::{Config, ListenAddr};
use crate::error::ExitError;
use crate::process::Process;
use crate::store::Store;
use crate::zones::{Answer, SharedZoneSet, StoredDname, StoredRecord /*Zone*/};
use bytes::Bytes;
use domain::base::iana::Class;
use domain::base::wire::Composer;
use domain::base::{Dname, MessageBuilder, Rtype, StreamTarget};
use domain::base::{Message, ToDname};
use domain::dep::octseq::{self, FreezeBuilder, Octets};
use domain::net::server::buf::VecBufSource;
use domain::net::server::middleware::builder::MiddlewareBuilder;
use domain::net::server::servers::dgram::server::DgramServer;
use domain::net::server::servers::stream::server::StreamServer;
use domain::net::server::traits::message::ContextAwareMessage;
use domain::net::server::traits::server::Server;
use domain::net::server::traits::service::{
    CallResult, ServiceResult, ServiceResultItem, Transaction,
};
use domain::net::server::util::mk_service;
use domain::rdata::{
    Cname, Mb, Md, Mf, Minfo, Mr, Mx, Ns, Nsec, Ptr, Rrsig, Soa, Srv, ZoneRecordData,
};
use domain::zonefile::inplace::{Entry, Zonefile};
use futures::future::pending;
use std::fmt::Debug;
use std::fs::File;
use std::future::Future;
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
            let mut zonefile = crate::zonefile::Zonefile::new(
                StoredDname::from_str("example.com.").unwrap(),
                Class::In,
            );
            for item in reader {
                match item.unwrap() {
                    Entry::Record(record) => {
                        let (owner, class, ttl, data) =
                            (record.owner(), record.class(), record.ttl(), record.data());
                        let owner: Dname<Bytes> = owner.to_dname().unwrap();
                        let data: ZoneRecordData<Bytes, StoredDname> = match data.to_owned() {
                            ZoneRecordData::A(v) => v.into(),
                            ZoneRecordData::Cname(v) => {
                                ZoneRecordData::<Bytes, StoredDname>::Cname(Cname::new(
                                    v.into_cname().to_canonical_dname().unwrap(),
                                ))
                            }
                            ZoneRecordData::Hinfo(v) => v.into(),
                            ZoneRecordData::Mb(v) => ZoneRecordData::<Bytes, StoredDname>::Mb(
                                Mb::new(v.madname().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Md(v) => ZoneRecordData::<Bytes, StoredDname>::Md(
                                Md::new(v.madname().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Mf(v) => ZoneRecordData::<Bytes, StoredDname>::Mf(
                                Mf::new(v.madname().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Minfo(v) => {
                                ZoneRecordData::<Bytes, StoredDname>::Minfo(Minfo::new(
                                    v.rmailbx().to_dname().unwrap(),
                                    v.emailbx().to_dname().unwrap(),
                                ))
                            }
                            ZoneRecordData::Mr(v) => ZoneRecordData::<Bytes, StoredDname>::Mr(
                                Mr::new(v.newname().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Mx(v) => ZoneRecordData::<Bytes, StoredDname>::Mx(
                                Mx::new(v.preference(), v.exchange().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Ns(v) => ZoneRecordData::<Bytes, StoredDname>::Ns(
                                Ns::new(v.nsdname().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Ptr(v) => ZoneRecordData::<Bytes, StoredDname>::Ptr(
                                Ptr::new(v.ptrdname().to_dname().unwrap()),
                            ),
                            ZoneRecordData::Soa(v) => {
                                ZoneRecordData::<Bytes, StoredDname>::Soa(Soa::new(
                                    v.mname().to_dname().unwrap(),
                                    v.rname().to_dname().unwrap(),
                                    v.serial(),
                                    v.refresh(),
                                    v.retry(),
                                    v.expire(),
                                    v.minimum(),
                                ))
                            }
                            ZoneRecordData::Txt(v) => v.into(),
                            ZoneRecordData::Srv(v) => {
                                ZoneRecordData::<Bytes, StoredDname>::Srv(Srv::new(
                                    v.priority(),
                                    v.weight(),
                                    v.port(),
                                    v.target().to_dname().unwrap(),
                                ))
                            }
                            ZoneRecordData::Aaaa(v) => v.into(),
                            ZoneRecordData::Dnskey(v) => v.into(),
                            ZoneRecordData::Rrsig(v) => {
                                ZoneRecordData::<Bytes, StoredDname>::Rrsig(
                                    Rrsig::new(
                                        v.type_covered(),
                                        v.algorithm(),
                                        v.labels(),
                                        v.original_ttl(),
                                        v.expiration(),
                                        v.inception(),
                                        v.key_tag(),
                                        v.signer_name().to_dname().unwrap(),
                                        v.signature().clone(),
                                    )
                                    .unwrap(),
                                )
                            }
                            ZoneRecordData::Nsec(v) => ZoneRecordData::<Bytes, StoredDname>::Nsec(
                                Nsec::new(v.next_name().to_dname().unwrap(), v.types().clone()),
                            ),
                            ZoneRecordData::Ds(v) => v.into(),
                            ZoneRecordData::Dname(v) => {
                                ZoneRecordData::<Bytes, StoredDname>::Dname(
                                    domain::rdata::Dname::new(v.dname().clone().unwrap().1),
                                )
                            }
                            ZoneRecordData::Nsec3(v) => v.into(),
                            ZoneRecordData::Nsec3param(v) => v.into(),
                            ZoneRecordData::Cdnskey(v) => v.into(),
                            ZoneRecordData::Cds(v) => v.into(),
                            ZoneRecordData::Unknown(v) => v.into(),
                            _ => todo!(),
                        };
                        let record = StoredRecord::new(owner, class, ttl, data);
                        zonefile.insert(record).unwrap();
                    }
                    _ => panic!("unsupported item"),
                }
            }
            let zone = zonefile.into_zone_builder().unwrap().finalize();

            let store = match Store::init(process.config().data_dir.clone()) {
                Ok(store) => store,
                Err(err) => {
                    eprintln!("Failed to open database: {}", err);
                    return Err(ExitError::Generic);
                }
            };
            let zones = match process.config().initialize {
                true => match SharedZoneSet::init(store).await {
                    Ok(zones) => zones,
                    Err(err) => {
                        eprintln!("Failed to initialize zones: {}", err);
                        return Err(ExitError::Generic);
                    }
                },
                false => match SharedZoneSet::load(store).await {
                    Ok(zones) => zones,
                    Err(err) => {
                        eprintln!("Failed to load zones: {}", err);
                        return Err(ExitError::Generic);
                    }
                },
            };

            zones.write().await.insert_zone(zone).await.unwrap();

            for addr in process.config().listen.iter().cloned() {
                eprintln!("Binding on {}", addr);
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

    use domain::net::server::traits::sock::AsyncAccept;
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

async fn server(addr: ListenAddr, zones: SharedZoneSet) -> Result<(), io::Error> {
    let buf = Arc::new(VecBufSource);
    let middleware = MiddlewareBuilder::<Vec<u8>>::default().finish();
    let svc = mk_service(query, zones).into();
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

fn query<Target>(
    msg: Arc<ContextAwareMessage<Message<Vec<u8>>>>,
    zones: SharedZoneSet,
) -> ServiceResult<Target, (), impl Future<Output = ServiceResultItem<Target, ()>> + Send>
where
    Target: Composer + Default + Octets + FreezeBuilder<Octets = Target> + Send + Sync + 'static,
    <Target as octseq::OctetsBuilder>::AppendError: Debug,
{
    let qtype = msg.sole_question().unwrap().qtype();
    if qtype == Rtype::Axfr {
        let mut txn = Transaction::stream();

        let cloned_msg = msg.clone();
        let cloned_zones = zones.clone();
        txn.push(async move {
            let question = cloned_msg.sole_question().unwrap();
            let zone = cloned_zones
                .read()
                .await
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));
            let answer = match zone {
                Some(zone) => zone.query(question.qname(), Rtype::Soa).unwrap(),
                None => Answer::refused(),
            };
            let target = StreamTarget::new(Target::default()).unwrap(); // SAFETY
            let builder = MessageBuilder::from_target(target).unwrap(); // SAFETY
            let additional = answer.to_message(&cloned_msg, builder);
            Ok(CallResult::new(additional))
        });

        let cloned_msg = msg.clone();
        let cloned_zones = zones.clone();
        txn.push(async move {
            let question = cloned_msg.sole_question().unwrap();
            let zone = cloned_zones
                .read()
                .await
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));
            let answer = match zone {
                Some(zone) => zone.query(question.qname(), Rtype::Soa).unwrap(),
                None => Answer::refused(),
            };
            let target = StreamTarget::new(Target::default()).unwrap(); // SAFETY
            let builder = MessageBuilder::from_target(target).unwrap(); // SAFETY
            let additional = answer.to_message(&cloned_msg, builder);
            Ok(CallResult::new(additional))
        });
        Ok(txn)
    } else {
        let fut = async move {
            let question = msg.sole_question().unwrap();
            let zone = zones
                .read()
                .await // ERROR! No async here yet!
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));
            let answer = match zone {
                Some(zone) => zone.query(question.qname(), question.qtype()).unwrap(),
                None => Answer::refused(),
            };

            let target = StreamTarget::new(Target::default()).unwrap(); // SAFETY
            let builder = MessageBuilder::from_target(target).unwrap(); // SAFETY
            let additional = answer.to_message(&msg, builder);
            Ok(CallResult::new(additional))
        };
        Ok(Transaction::single(fut))
    }
}
