//! Running the daemon.

use std::fs::File;
use std::{io};
use std::future::Future;
use std::str::FromStr;
use domain::base::{Message, MessageBuilder, StreamTarget};
use domain::base::iana::Class;
use domain::base::name::{Dname, FlattenInto};
use domain::zonefile::inplace::{Zonefile, Entry};
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
use crate::zones::{Answer, SharedZoneSet};

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
            Dname::from_str("example.com.").unwrap(),
            Class::In
        );
        for item in reader {
            match item.unwrap() {
                Entry::Record(record) => {
                    zonefile.insert(record.flatten_into()).unwrap();
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
        let zones = match process.config().initialize {
            true => {
                match SharedZoneSet::init(store).await {
                    Ok(zones) => zones,
                    Err(err) => {
                        eprintln!("Failed to initialize zones: {}", err);
                        return Err(ExitError::Generic);
                    }
                }
            }
            false => {
                match SharedZoneSet::load(store).await {
                    Ok(zones) => zones,
                    Err(err) => {
                        eprintln!("Failed to load zones: {}", err);
                        return Err(ExitError::Generic);
                    }
                }
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

