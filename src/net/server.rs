//! Networking for a DNS server.

use std::io;
use std::future::Future;
use std::sync::Arc;
use domain::base::{Message, StreamTarget};
use futures::pin_mut;
use futures::future::poll_fn;
use futures::stream::{Stream, StreamExt};
use octseq::builder::OctetsBuilder;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc;
use super::sock::{AsyncAccept, AsyncDgramSock};


//------------ Service -------------------------------------------------------

/// A type accepts and dispatches incoming DNS requests.
pub trait Service<RequestOctets: AsRef<[u8]>> {
    type ResponseOctets: OctetsBuilder + Send + Sync + 'static + std::convert::AsRef<[u8]>;
    type Single: Future<
        Output = Result<StreamTarget<Self::ResponseOctets>, io::Error>
    > + Send + 'static;
    type Stream: Stream<
        Item = Result<StreamTarget<Self::ResponseOctets>, io::Error>
    > + Send + 'static;

    fn handle_request(
        &self, message: Message<RequestOctets>
    ) -> Transaction<Self::Single, Self::Stream>;
}

impl<F, RequestOctets, ResponseOctets, Single, Strm>
Service<RequestOctets> for F
where
    F: Fn(Message<RequestOctets>) -> Transaction<Single, Strm>,
    RequestOctets: AsRef<[u8]>,
    ResponseOctets: OctetsBuilder + Send + Sync + 'static + std::convert::AsRef<[u8]>,
    Single: Future<
        Output = Result<StreamTarget<ResponseOctets>, io::Error>
    > + Send + 'static,
    Strm: Stream<
        Item = Result<StreamTarget<ResponseOctets>, io::Error>
    > + Send + 'static,
{
    type ResponseOctets = ResponseOctets;
    type Single = Single;
    type Stream = Strm;

    fn handle_request(
        &self, message: Message<RequestOctets>
    ) -> Transaction<Self::Single, Self::Stream> {
        (*self)(message)
    }
}


//------------ Transaction ---------------------------------------------------

/// A server transaction generating the responses for a request.
pub enum Transaction<SingleFut, StreamFut> {
    /// The transaction will be concluded with a single response.
    Single(SingleFut),

    /// The transaction will results in stream of multiple responses.
    Stream(StreamFut),
}


//------------ BufSource ----------------------------------------------------

pub trait BufSource {
    type Output: AsRef<[u8]> + AsMut<[u8]>;

    fn create_buf(&self) -> Self::Output;
    fn create_sized(&self, size: usize) -> Self::Output;
}



//------------ DgramServer ---------------------------------------------------

pub struct DgramServer<Sock, Buf, Svc> {
    sock: Arc<Sock>,
    buf: Buf,
    service: Svc,
}

impl<Sock, Buf, Svc> DgramServer<Sock, Buf, Svc>
where
    Sock: AsyncDgramSock + Send + Sync + 'static,
    Buf: BufSource,
    Svc: Service<Buf::Output>
{
    pub fn new(sock: Sock, buf: Buf, service: Svc) -> Self {
        DgramServer {
            sock: sock.into(), buf, service
        }
    }

    pub async fn run(self) -> Result<(), io::Error> {
        loop {
            let (msg, addr) = self.recv_from().await?;
            let msg = match Message::from_octets(msg) {
                Ok(msg) => msg,
                Err(_) => continue,
            };
            let sock = self.sock.clone();
            let tran = self.service.handle_request(msg);
            tokio::spawn(async move {
                match tran {
                    Transaction::Single(fut) => {
                        if let Ok(response) = fut.await {
                            let _ = Self::send_to(
                                &sock, response.as_dgram_slice(), &addr
                            ).await;
                        }
                    }
                    Transaction::Stream(stream) => {
                        pin_mut!(stream);
                        while let Some(response) = stream.next().await {
                            match response {
                                Ok(response) => {
                                    let _  = Self::send_to(
                                        &sock, response.as_dgram_slice(),
                                        &addr
                                    ).await;
                                }
                                Err(_) => break
                            }
                        }
                    }
                }
            });
        }
    }

    async fn recv_from(
        &self
    ) -> Result<(Buf::Output, Sock::Addr), io::Error> {
        let mut res = self.buf.create_buf();
        let addr = {
            let mut buf = ReadBuf::new(res.as_mut());
            poll_fn(|ctx| {
                self.sock.poll_recv_from(ctx, &mut buf)
            }).await?
        };
        Ok((res, addr))
    }

    async fn send_to(
        sock: &Sock,
        data: &[u8],
        dest: &Sock::Addr,
    ) -> Result<(), io::Error> {
        let sent = poll_fn(|ctx| {
            sock.poll_send_to(ctx, data, dest)
        }).await?;
        if sent != data.len() {
            Err(io::Error::new(io::ErrorKind::Other, "short send"))
        }
        else {
            Ok(())
        }
    }
}


//------------ StreamServer --------------------------------------------------

pub struct StreamServer<Sock, Buf, Svc> {
    sock: Sock,
    buf: Arc<Buf>,
    service: Arc<Svc>,
}

impl<Sock, Buf, Svc> StreamServer<Sock, Buf, Svc>
where
    Sock: AsyncAccept + Send + 'static,
    Sock::Stream: AsyncRead + AsyncWrite + Send + Sync + 'static,
    Buf: BufSource + Send + Sync + 'static,
    Buf::Output: Send + Sync + 'static,
    Svc: Service<Buf::Output> + Send + Sync + 'static,
{
    pub fn new(sock: Sock, buf: Buf, service: Svc) -> Self {
        StreamServer {
            sock, buf: buf.into(), service: service.into(),
        }
    }

    pub async fn run(self) -> Result<(), io::Error> {
        loop {
            let (stream, _addr) = self.accept().await?;
            let (buf, service) = (self.buf.clone(), self.service.clone());
            tokio::spawn(async move {
                let _ = Self::conn(stream, buf, service).await;
            });
        }
    }

    async fn accept(
        &self
    ) -> Result<(Sock::Stream, Sock::Addr), io::Error> {
        poll_fn(|ctx| {
            self.sock.poll_accept(ctx)
        }).await
    }

    async fn conn(
        stream: Sock::Stream,
        buf_source: Arc<Buf>,
        service: Arc<Svc>,
    ) -> Result<(), io::Error> {
        let (read, mut write) = tokio::io::split(stream);
        let (tx, mut rx) = mpsc::channel::<StreamTarget<Svc::ResponseOctets>>(
            10
        ); // XXX Channel length?

        // Sending end: Read messages from the channel and send them.
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if write.write_all(msg.as_stream_slice()).await.is_err() {
                    break
                }
            }
        });

        pin_mut!(read);
        loop {
            let size = read.read_i16().await? as usize;
            let mut buf = buf_source.create_sized(size);
            if buf.as_ref().len() < size {
                // XXX Maybe do something better here?
                return Err(io::Error::new(io::ErrorKind::Other, "short buf"))
            }
            read.read_exact(buf.as_mut()).await?;
            let msg = match Message::from_octets(buf) {
                Ok(msg) => msg,
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other, "short message"
                    ))
                }
            };
            let tran = service.handle_request(msg);
            let tx = tx.clone();
            tokio::spawn(async move {
                match tran {
                    Transaction::Single(fut) => {
                        if let Ok(response) = fut.await {
                            let _ = tx.send(response).await;
                        }
                    }
                    Transaction::Stream(stream) => {
                        pin_mut!(stream);
                        while let Some(response) = stream.next().await {
                            match response {
                                Ok(response) => {
                                    if tx.send(response).await.is_err() {
                                        break
                                    }
                                }
                                Err(_) => break
                            }
                        }
                    }
                }
            });
        }
    }
}

