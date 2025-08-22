#![warn(dead_code)]
#![warn(unused_variables)]

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract;
use axum::extract::Path;
use axum::extract::State;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use bytes::Bytes;
use domain::base::Name;
use domain::zonetree::StoredName;
use log::{debug, error, info};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::api::ZonesListResult;
use crate::api::ZoneRegister;
use crate::api::ZoneRegisterResult;
use crate::api::ServerStatusResult;
use crate::api::ZoneReloadResult;
use crate::api::ZoneStatusResult;
use crate::comms::{ApplicationCommand, Terminated};
use crate::http::HttpError;
use crate::manager::Component;
use crate::zone::Zones;

const HTTP_UNIT_NAME: &str = "HS";

// NOTE: To send data back from a unit, send them an app command with
// a transmitter they can use to send the reply

pub struct HttpServer {
    pub listen_addr: SocketAddr,
    pub zones: Arc<Zones>,
    pub cmd_rx: Option<mpsc::Receiver<ApplicationCommand>>,
}

impl HttpServer {
    pub async fn run(mut self, component: Component) -> Result<(), Terminated> {
        let component = Arc::new(RwLock::new(component));
        // Setup listener
        let sock = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })?;

        let mut cmd_rx = self
            .cmd_rx
            .take()
            .expect("This should always exist in the beginning");
        tokio::task::spawn(async move {
            loop {
                let cmd = cmd_rx.recv().await;
                let Some(cmd) = cmd else {
                    return Result::<(), Terminated>::Err(Terminated);
                };
                info!("[{HTTP_UNIT_NAME}] Received command: {cmd:?}");
                match &cmd {
                    ApplicationCommand::Terminate => {
                        return Err(Terminated);
                    }
                    // ...
                    _ => { /* not for us */ }
                }
            }
        });

        let state = Arc::new(self);

        let app = Router::new()
            .route("/", get(|| async { "Hello, World!" }))
            .route("/status", get(Self::status))
            .route("/zones/list", get(Self::zones_list))
            .route("/zone/register", post(Self::zone_register))
            .route("/zone/{name}/status", get(Self::zone_status))
            .route("/zone/{name}/reload", post(Self::zone_reload))
            .with_state(state);
        axum::serve(sock, app).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })
    }

    async fn zone_register(
        Json(payload): Json<ZoneRegister>,
    ) -> Json<ZoneRegisterResult> {
        Json(ZoneRegisterResult {
            name: payload.name.clone(),
            status: "Maybe Success".into(),
        })
    }

    async fn zones_list(
        // TODO: replace HttpServer with slimmed down state struct?
        State(state): State<Arc<HttpServer>>,
    ) -> Json<ZonesListResult> {
        let zones = state
            .zones
            .list()
            .iter()
            .map(|n| {
                // TODO: find a way to get correct `Name`s back from `Zones`
                let mut buf = String::with_capacity(256);
                let first = true;
                for l in n.labels() {
                    if !first {
                        buf.push('.')
                    }
                    buf.push_str(&l.to_string())
                }
                Name::from_str(&buf).unwrap()
            })
            .collect();
        Json(ZonesListResult { zones })
    }

    async fn zone_status(
        Path(name): Path<Name<Bytes>>,
    ) -> Json<ZoneStatusResult> {
        Json(ZoneStatusResult { name })
    }

    async fn zone_reload(
        Path(name): Path<Name<Bytes>>,
    ) -> Json<ZoneReloadResult> {
        Json(ZoneReloadResult { name })
    }

    async fn status() -> Json<ServerStatusResult> {
        Json(ServerStatusResult {})
    }
}
