//! Running the daemon.

use crate::config::{Config, ListenAddr};
use crate::error::ExitError;
use crate::process::Process;
use domain::base::iana::{Class, Opcode};
use domain::base::message_builder::AdditionalBuilder;
use domain::base::name::{Dname, FlattenInto};
use domain::base::Rtype;
use domain::dep::octseq::OctetsBuilder;
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::DgramServer;
use domain::net::server::middleware::builder::MiddlewareBuilder;
use domain::net::server::service::{CallResult, Transaction, TransactionStream};
use domain::net::server::stream::StreamServer;
use domain::net::server::util::{
    mk_builder_for_target, mk_service, MkServiceRequest, MkServiceResult,
};
use domain::zonefile::inplace::{Entry, Zonefile};
use domain::zonetree::{Answer, AnswerContent, ZoneSet};
use futures::future::pending;
use std::fs::File;
use std::future::ready;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
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
    let svc = mk_service(query, zones);
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

fn set_axfr_header<Target>(
    msg: &MkServiceRequest<Vec<u8>>,
    additional: &mut AdditionalBuilder<Target>,
) where
    Target: AsMut<[u8]>,
    Target: OctetsBuilder,
{
    // https://datatracker.ietf.org/doc/html/rfc5936#section-2.2.1
    // 2.2.1: Header Values
    //
    // "These are the DNS message header values for AXFR responses.
    //
    //     ID          MUST be copied from request -- see Note a)
    //
    //     QR          MUST be 1 (Response)
    //
    //     OPCODE      MUST be 0 (Standard Query)
    //
    //     Flags:
    //        AA       normally 1 -- see Note b)
    //        TC       MUST be 0 (Not truncated)
    //        RD       RECOMMENDED: copy request's value; MAY be set to 0
    //        RA       SHOULD be 0 -- see Note c)
    //        Z        "mbz" -- see Note d)
    //        AD       "mbz" -- see Note d)
    //        CD       "mbz" -- see Note d)"
    let header = additional.header_mut();
    header.set_id(msg.header().id());
    header.set_qr(true);
    header.set_opcode(Opcode::Query);
    header.set_aa(true);
    header.set_tc(false);
    header.set_rd(msg.header().rd());
    header.set_ra(false);
    header.set_z(false);
    header.set_ad(false);
    header.set_cd(false);
}

fn query(msg: MkServiceRequest<Vec<u8>>, zones: Arc<ZoneSet>) -> MkServiceResult<Vec<u8>, MyError> {
    let qtype = msg.sole_question().unwrap().qtype();
    if qtype == Rtype::Axfr {
        let stream_fut = async move {
            let question = msg.sole_question().unwrap();
            let zone = zones
                .find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));

            let stream = Arc::new(Mutex::new(TransactionStream::default()));

            if let Some(zone) = zone {
                // https://datatracker.ietf.org/doc/html/rfc5936#section-2.2
                // 2.2: AXFR Response
                //
                // "An AXFR response that is transferring the zone's contents
                //  will consist of a series (which could be a series of
                //  length 1) of DNS messages.  In such a series, the first
                //  message MUST begin with the SOA resource record of the
                //  zone, and the last message MUST conclude with the same SOA
                //  resource record.  Intermediate messages MUST NOT contain
                //  the SOA resource record.  The AXFR server MUST copy the
                //  Question section from the corresponding AXFR query message
                //  into the first response message's Question section.  For
                //  subsequent messages, it MAY do the same or leave the
                //  Question section empty."

                // Get the SOA record as AXFR transfers must start and end
                // with the SOA record
                let question = msg.sole_question().unwrap();
                let answer = zone
                    .query(question.qname(), Rtype::Soa)
                    .unwrap_or_else(|_| Answer::refused());
                let builder = mk_builder_for_target();
                let mut additional = answer.to_message(&msg, builder);
                set_axfr_header(&msg, &mut additional);
                let begin_soa_response = CallResult::new(additional);
                let end_soa_response = begin_soa_response.clone();

                // Note: The code above already added the question from the
                // request into the SOA response messages.

                // Push the begin SOA response message into the stream
                stream.lock().unwrap().push(ready(Ok(begin_soa_response)));

                // "The AXFR protocol treats the zone contents as an unordered
                //  collection (or to use the mathematical term, a "set") of
                //  RRs.  Except for the requirement that the transfer must
                //  begin and end with the SOA RR, there is no requirement to
                //  send the RRs in any particular order or grouped into
                //  response messages in any particular way.  Although servers
                //  typically do attempt to send related RRs (such as the RRs
                //  forming an RRset, and the RRsets of a name) as a
                //  contiguous group or, when message space allows, in the
                //  same response message, they are not required to do so, and
                //  clients MUST accept any ordering and grouping of the
                //  non-SOA RRs.  Each RR SHOULD be transmitted only once, and
                //  AXFR clients MUST ignore any duplicate RRs received.
                //
                //  Each AXFR response message SHOULD contain a sufficient
                //  number of RRs to reasonably amortize the per-message
                //  overhead, up to the largest number that will fit within a
                //  DNS message (taking the required content of the other
                //  sections into account, as described below).
                //
                //  Some old AXFR clients expect each response message to
                //  contain only a single RR.  To interoperate with such
                //  clients, the server MAY restrict response messages to a
                //  single RR.  As there is no standard way to automatically
                //  detect such clients, this typically requires manual
                //  configuration at the server."

                // TODO: Support a setting for sending only one RR per
                // response message for backward compatibility.

                let cloned_msg = msg.clone();
                let cloned_stream = stream.clone();
                zone.walk(Box::new(move |answer: Answer| {
                    if let AnswerContent::Data(rrset) = answer.content() {
                        if rrset.rtype() != Rtype::Soa {
                            let builder = mk_builder_for_target();
                            let mut additional = answer.to_message(&cloned_msg, builder);
                            set_axfr_header(&cloned_msg, &mut additional);
                            cloned_stream
                                .lock()
                                .unwrap()
                                .push(ready(Ok(CallResult::new(additional))));
                        }
                    }
                }));

                // Push the end SOA response message into the stream
                stream.lock().unwrap().push(ready(Ok(end_soa_response)));
            }

            let mutex = Arc::into_inner(stream).unwrap();
            mutex.into_inner().unwrap()
        };

        Ok(Transaction::stream(Box::pin(stream_fut)))
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
