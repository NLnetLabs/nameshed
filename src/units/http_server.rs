#![warn(dead_code)]
#![warn(unused_variables)]

use std::convert::Infallible;
use std::future::Future;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use domain::zonetree::StoredName;
use hyper::body::Body;
use hyper::body::Incoming;
use hyper::server::conn::http2;
use hyper::service::service_fn as tokio_service_fn;
use hyper::service::Service;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use log::{debug, error, info};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::comms::{ApplicationCommand, Terminated};
use crate::http::HttpError;
use crate::manager::Component;

const HTTP_UNIT_NAME: &str = "HS";

#[derive(Debug)]
pub struct HttpServer {
    pub listen_addr: SocketAddr,
    pub cmd_rx: mpsc::Receiver<ApplicationCommand>,
}

impl HttpServer {
    pub async fn run(mut self, component: Component) -> Result<(), Terminated> {
        let handler = RequestHandler::new(Arc::new(RwLock::new(component)));
        // Setup listener
        let sock = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })?;

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        return Err(Terminated);
                    };
                    info!("[{HTTP_UNIT_NAME}] Received command: {cmd:?}");
                    match &cmd {
                        ApplicationCommand::Terminate => {
                            return Err(Terminated);
                        },
                        // ...
                        _ => { /* not for us */ }
                    }
                },
                conn = sock.accept() => {
                    let (stream, remote) = match conn {
                        Ok(x) => x,
                        Err(e) => {
                            error!("[{HTTP_UNIT_NAME}] Failed accepting incoming connection: {e}");
                            continue;
                        }
                    };
                    info!("[{HTTP_UNIT_NAME}] Incoming HTTP connection from {remote}");
                    let io = TokioIo::new(stream);

                    let handler = handler.clone();
                    tokio::task::spawn(async move {
                        if let Err(err) = http2::Builder::new(TokioExecutor::new())
                            .serve_connection(io, handler)
                            .await {
                            error!("[{HTTP_UNIT_NAME}] Error serving connection: {}", err);
                        }
                    });
                }
            }
        }
    }
}

//------------ RequestHandler ------------------------------------------------

#[derive(Clone)]
pub struct RequestHandler {
    component: Arc<RwLock<Component>>,
}

impl Service<Request<Incoming>> for RequestHandler {
    type Response = Response<String>;
    type Error = Infallible;
    type Future = Pin<
        Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let comp = self.component.clone();
        Box::pin(async { Self::handle_request(comp, req).await })
    }
}

impl RequestHandler {
    pub fn new(component: Arc<RwLock<Component>>) -> Self {
        RequestHandler { component }
    }

    async fn handle_request(
        component: Arc<RwLock<Component>>,
        req: Request<Incoming>,
    ) -> Result<Response<String>, Infallible> {
        info!("[{HTTP_UNIT_NAME}] Got request: {req:?}");
        let write_lock = component.write().await;
        write_lock.send_command(
            "ZS",
            ApplicationCommand::SignZone {
                zone_name: StoredName::from_str("example.com").unwrap(),
                zone_serial: None,
            },
        ).await;
        Ok(Response::new(String::from("Hello World!")))
    }
}
