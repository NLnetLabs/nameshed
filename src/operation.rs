//! Running the daemon.

use std::fs::File;
use std::{io};
use std::future::Future;
use std::str::FromStr;
use bytes::Bytes;
use domain::base::{Message, MessageBuilder, StreamTarget, ToDname};
use domain::base::iana::Class;
use domain::base::Dname;
use domain::rdata::{ZoneRecordData, Cname, Mb, Md, Mf, Minfo, Mr, Mx, Ns, Ptr, Soa, Srv, Rrsig, Nsec};
use domain::zonefile::inplace::{Zonefile, Entry};
//use domain::base::RecordData;
use futures::future::{pending, Pending};
use futures::stream::Once;
use octseq::octets::Octets;
use tokio::net::{TcpListener, UdpSocket};
use tokio::runtime::Runtime;
use crate::config::{Config, ListenAddr};
use crate::error::ExitError;
use crate::net::server::{BufSource, DgramServer, StreamServer, Transaction};
use crate::process::Process;
use crate::store::Store;
use crate::zones::{Answer, StoredDname, SharedZoneSet, StoredRecord, /*Zone*/};

pub fn prepare() -> Result<(), ExitError> {
    Process::init()?;
    Ok(())
}

#[allow(clippy::mutable_key_type)]
pub fn run(config: Config) -> Result<(), ExitError> {
    let process = Process::new(config);
    process.switch_logging(false)?;

    Runtime::new().map_err(|_| ExitError::Generic)?.block_on(async move {
        let reader = Zonefile::load(&mut File::open("/etc/nsd/example.com.zone").unwrap()).unwrap();
        let mut zonefile = crate::zonefile::Zonefile::new(
            StoredDname::from_str("example.com.").unwrap(),
            Class::In
        );
        for item in reader {
            match item.unwrap() {
                Entry::Record(record) => {
                    let (owner, class, ttl, data) = (record.owner(), record.class(), record.ttl(), record.data());
                    let owner: Dname<Bytes> = owner.to_dname().unwrap();
                    let data: ZoneRecordData<Bytes, StoredDname> = match data.to_owned() {
                        ZoneRecordData::A(v) => v.into(),
                        ZoneRecordData::Cname(v) => ZoneRecordData::<Bytes, StoredDname>::Cname(Cname::new(v.to_dname().unwrap())),
                        ZoneRecordData::Hinfo(v) => v.into(),
                        ZoneRecordData::Mb(v) => ZoneRecordData::<Bytes, StoredDname>::Mb(Mb::new(v.madname().to_dname().unwrap())),
                        ZoneRecordData::Md(v) => ZoneRecordData::<Bytes, StoredDname>::Md(Md::new(v.madname().to_dname().unwrap())),
                        ZoneRecordData::Mf(v) => ZoneRecordData::<Bytes, StoredDname>::Mf(Mf::new(v.madname().to_dname().unwrap())),
                        ZoneRecordData::Minfo(v) => ZoneRecordData::<Bytes, StoredDname>::Minfo(Minfo::new(v.rmailbx().to_dname().unwrap(), v.emailbx().to_dname().unwrap())),
                        ZoneRecordData::Mr(v) => ZoneRecordData::<Bytes, StoredDname>::Mr(Mr::new(v.newname().to_dname().unwrap())),
                        ZoneRecordData::Mx(v) => ZoneRecordData::<Bytes, StoredDname>::Mx(Mx::new(v.preference(), v.exchange().to_dname().unwrap())),
                        ZoneRecordData::Ns(v) => ZoneRecordData::<Bytes, StoredDname>::Ns(Ns::new(v.nsdname().to_dname().unwrap())),
                        ZoneRecordData::Ptr(v) => ZoneRecordData::<Bytes, StoredDname>::Ptr(Ptr::new(v.ptrdname().to_dname().unwrap())),
                        ZoneRecordData::Soa(v) => ZoneRecordData::<Bytes, StoredDname>::Soa(Soa::new(v.mname().to_dname().unwrap(), v.rname().to_dname().unwrap(), v.serial(), v.refresh(), v.retry(), v.expire(), v.minimum())),
                        ZoneRecordData::Txt(v) => v.into(),
                        ZoneRecordData::Srv(v) => ZoneRecordData::<Bytes, StoredDname>::Srv(Srv::new(v.priority(), v.weight(), v.port(), v.target().to_dname().unwrap())),
                        ZoneRecordData::Aaaa(v) => v.into(),
                        ZoneRecordData::Dnskey(v) => v.into(),
                        ZoneRecordData::Rrsig(v) => ZoneRecordData::<Bytes, StoredDname>::Rrsig(Rrsig::new(v.type_covered(), v.algorithm(), v.labels(), v.original_ttl(), v.expiration(), v.inception(), v.key_tag(), v.signer_name().to_dname().unwrap(), v.signature().clone())),
                        ZoneRecordData::Nsec(v) => ZoneRecordData::<Bytes, StoredDname>::Nsec(Nsec::new(v.next_name().to_dname().unwrap(), v.types().clone())),
                        ZoneRecordData::Ds(v) => v.into(),
                        ZoneRecordData::Dname(v) => ZoneRecordData::<Bytes, StoredDname>::Dname(domain::rdata::Dname::new(v.to_dname().unwrap())),
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
                _ => panic!("unsupported item")
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
        let zones = match SharedZoneSet::load(store).await {
            Ok(zones) => zones,
            Err(err) => {
                eprintln!("Failed to load zones: {}", err);
                return Err(ExitError::Generic);
            }
        };

        zones.write().await.insert_zone(zone).await.unwrap();

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

async fn server(
    addr: ListenAddr,
    zones: SharedZoneSet,
) -> Result<(), io::Error> {
    match addr {
        ListenAddr::Udp(addr) => {
            let sock = UdpSocket::bind(addr).await?;
            DgramServer::new(
                sock, VecBufSource, service(zones)
            ).run().await
        }
        ListenAddr::Tcp(addr) => {
            let sock = TcpListener::bind(addr).await?;
            StreamServer::new(
                sock, VecBufSource, service(zones)
            ).run().await
        }
    }
}

fn service<RequestOctets: Octets + Send + Sync + 'static>(
    zones: SharedZoneSet,
) -> impl crate::net::server::Service<RequestOctets> {
    #[allow(clippy::type_complexity)]
    fn query<RequestOctets: Octets>(
        message: Message<RequestOctets>,
        zones: SharedZoneSet,
    ) -> Transaction<
        impl Future<Output = Result<StreamTarget<Vec<u8>>, io::Error>>,
        Once<Pending<Result<StreamTarget<Vec<u8>>, io::Error>>>
    > {
        Transaction::Single(async move {
            let zones = zones.read().await;
            let question = message.sole_question().unwrap();
            let zone =
                zones.find_zone(question.qname(), question.qclass())
                .map(|zone| zone.read(None));
            let answer = match zone {
                Some(zone) => {
                    zone.query(
                        question.qname(), question.qtype()
                    ).unwrap()
                }
                None => Answer::refused()
            };
            Ok(answer.to_message(
                message.for_slice(), MessageBuilder::new_stream_vec()
            ))
        })
    }

    move |message| { query(message, zones.clone()) }
}


struct VecBufSource;

impl BufSource for VecBufSource {
    type Output = Vec<u8>;

    fn create_buf(&self) -> Vec<u8> {
        vec![0; 64 * 1024]
    }

    fn create_sized(&self, size: usize) -> Vec<u8> {
        vec![0; size]
    }
}

