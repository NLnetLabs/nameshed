use std::collections::BTreeMap;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::extract;
use axum::extract::Path;
use axum::extract::State;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use bytes::Bytes;
use chrono::DateTime;
use domain::base::Name;
use domain::base::ToName;
use domain::utils::dst::UnsizedCopy;
use domain::zonetree::StoredName;
use domain::zonetree::ZoneTree;
use log::{debug, error, info};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::api::ServerStatusResult;
use crate::api::ZoneAdd;
use crate::api::ZoneAddResult;
use crate::api::ZoneReloadResult;
use crate::api::ZoneSource;
use crate::api::ZoneStage;
use crate::api::ZoneStatusResult;
use crate::api::ZonesListEntry;
use crate::api::ZonesListResult;
use crate::comms::{ApplicationCommand, Terminated};
use crate::manager::Component;
// use crate::zone::Zones;

const HTTP_UNIT_NAME: &str = "HS";

// NOTE: To send data back from a unit, send them an app command with
// a transmitter they can use to send the reply

pub struct HttpServer {
    pub listen_addr: SocketAddr,
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

        let app = Router::new()
            .route("/", get(|| async { "Hello, World!" }))
            .route("/status", get(Self::status))
            .route("/zones/list", get(Self::zones_list))
            .route("/zone/add", post(Self::zone_add))
            .route("/zone/{name}/status", get(Self::zone_status))
            .route("/zone/{name}/reload", post(Self::zone_reload))
            .with_state(component);

        axum::serve(sock, app).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })
    }

    async fn zone_add(
        State(component): State<Arc<RwLock<Component>>>,
        Json(zone_register): Json<ZoneAdd>,
    ) -> Json<ZoneAddResult> {
        let zone_name = zone_register.name.clone();
        component
            .read()
            .await
            .send_command(
                "ZL",
                ApplicationCommand::RegisterZone {
                    register: zone_register,
                },
            )
            .await;
        Json(ZoneAddResult {
            name: zone_name,
            status: "Submitted".to_string(),
        })
    }

    async fn zones_list(State(state): State<Arc<RwLock<Component>>>) -> Json<ZonesListResult> {
        let state = state.read().await;

        // The zone trees in the Component overlap. Therefore we take the
        // furthest a zone has progressed. We use a BTreeMap to sort the zones
        // while we're doing this.
        let mut all_zones = BTreeMap::new();

        let unsigned_zones = state.unsigned_zones().load();
        for zone in unsigned_zones.iter_zones() {
            all_zones.insert(zone.apex_name().clone(), ZoneStage::Unsigned);
        }

        let unsigned_zones = state.signed_zones().load();
        for zone in unsigned_zones.iter_zones() {
            all_zones.insert(zone.apex_name().clone(), ZoneStage::Signed);
        }

        let unsigned_zones = state.published_zones().load();
        for zone in unsigned_zones.iter_zones() {
            all_zones.insert(zone.apex_name().clone(), ZoneStage::Published);
        }

        let zones = all_zones
            .into_iter()
            .map(|(name, stage)| ZonesListEntry { name, stage })
            .collect();

        Json(ZonesListResult { zones })
    }

    async fn zone_status(Path(name): Path<Name<Bytes>>) -> Json<ZoneStatusResult> {
        Json(ZoneStatusResult { name })
    }

    async fn zone_reload(
        Path(payload): Path<Name<Bytes>>,
    ) -> Result<Json<ZoneReloadResult>, String> {
        Ok(Json(ZoneReloadResult { name: payload }))
    }

    async fn status() -> Json<ServerStatusResult> {
        Json(ServerStatusResult {})
    }
}
