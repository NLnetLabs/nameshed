//! Running the daemon.

use std::io;
use std::future::Future;
use domain::base::{Message, MessageBuilder, StreamTarget};
use domain::base::iana::Rcode;
use domain::base::octets::OctetsRef;
use futures::future::{pending, Pending};
use futures::stream::Once;
use tokio::net::{TcpListener, UdpSocket};
use tokio::runtime::Runtime;
use crate::config::{Config, ListenAddr};
use crate::error::ExitError;
use crate::net::server::{BufSource, DgramServer, StreamServer, Transaction};
use crate::process::Process;


pub fn run(config: Config) -> Result<(), ExitError> {
    Process::init()?;
    let process = Process::new(config);
    process.switch_logging(false)?;
    Runtime::new().map_err(|_| ExitError::Generic)?.block_on(async move {
        for addr in process.config().listen.iter().cloned() {
            tokio::spawn(async move {
                if let Err(err) = server(addr).await {
                    println!("{}", err);
                }
            });
        }
        pending().await
    })
}

async fn server(addr: ListenAddr) -> Result<(), io::Error> {
    match addr {
        ListenAddr::Udp(addr) => {
            let sock = UdpSocket::bind(addr).await?;
            DgramServer::new(sock, VecBufSource, refuse).run().await
        }
        ListenAddr::Tcp(addr) => {
            let sock = TcpListener::bind(addr).await?;
            StreamServer::new(sock, VecBufSource, refuse).run().await
        }
    }
}

fn refuse<RequestOctets: AsRef<[u8]>>(
    message: Message<RequestOctets>
) -> Transaction<
    impl Future<Output = Result<StreamTarget<Vec<u8>>, io::Error>>,
    Once<Pending<Result<StreamTarget<Vec<u8>>, io::Error>>>
>
where for<'a> &'a RequestOctets: OctetsRef
{
    Transaction::Single(async move {
        MessageBuilder::new_stream_vec().start_answer(
            &message, Rcode::Refused
        ).map(|builder| builder.finish()).map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "short buf")
        })
    })
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

