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
use domain::new::base::name::RevNameBuf;
use domain::new::base::wire::BuildBytes;
use domain::new::base::wire::ParseBytes;
use domain::utils::dst::UnsizedCopy;
use domain::zonetree::StoredName;
use domain::zonetree::ZoneTree;
use log::{debug, error, info};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::api::LoadType;
use crate::api::ServerStatusResult;
use crate::api::ZoneListEntry;
use crate::api::ZoneRegister;
use crate::api::ZoneRegisterResult;
use crate::api::ZoneReloadResult;
use crate::api::ZoneSource;
use crate::api::ZoneStatusResult;
use crate::api::ZoneVersion;
use crate::api::ZoneVersionStatus;
use crate::api::ZonesListResult;
use crate::comms::{ApplicationCommand, Terminated};
use crate::loader::Loader;
use crate::manager::Component;
use crate::zone::loader::DnsServerAddr;
use crate::zone::loader::EnqueuedRefresh;
use crate::zone::loader::OngoingRefresh;
use crate::zone::loader::Source;
use crate::zone::LoaderState;
use crate::zone::Zones;

const HTTP_UNIT_NAME: &str = "HS";

// NOTE: To send data back from a unit, send them an app command with
// a transmitter they can use to send the reply

pub struct HttpServer {
    pub listen_addr: SocketAddr,
    pub loader: Arc<Loader>,
    pub zones: Arc<Zones>,
    pub unsigned_zones: Arc<ArcSwap<ZoneTree>>,
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
            .route("/zone/{name}/source", post(Self::zone_change_source))
            .route("/zone/{name}/status", get(Self::zone_status))
            .route("/zone/{name}/reload", post(Self::zone_reload))
            .with_state(state);
        axum::serve(sock, app).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })
    }

    async fn zone_register(
        State(state): State<Arc<HttpServer>>,
        Json(payload): Json<ZoneRegister>,
    ) -> Result<Json<ZoneRegisterResult>, String> {
        // TODO: This conversion needs to be improved
        let mut bytes = Vec::new();
        payload.name.compose(&mut bytes).unwrap();
        let name = RevNameBuf::parse_bytes(&bytes).unwrap();
        let result = state.zones.add(name.unsized_copy_into());
        match result {
            Ok(new_zone) => {
                state.unsigned_zones.rcu(|tree| {
                    let mut tree = tree.clone();
                    Arc::make_mut(&mut tree)
                        .insert_zone(new_zone.loaded.clone())
                        .unwrap();
                    tree
                });
                Ok(Json(ZoneRegisterResult {
                    name: payload.name.clone(),
                    status: "Success".into(),
                }))
            }
            Err(_old_zone) => Err("already exists".into()),
        }
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
                let mut bytes = vec![0u8; n.built_bytes_size()];
                n.build_bytes(&mut bytes).unwrap();
                let bytes = bytes::Bytes::from(bytes);
                let mut parser = octseq::Parser::from_ref(&bytes);
                let name = Name::parse(&mut parser).unwrap();

                let mut versions = Vec::new();

                let zone = state.zones.get(n).unwrap();
                let state = zone.data.lock().unwrap();
                if let Some(refreshes) = &state.loader.refreshes {
                    if let Some(enqueued) = &refreshes.enqueued {
                        let load_type = match enqueued {
                            EnqueuedRefresh::Refresh => LoadType::Refresh,
                            EnqueuedRefresh::Reload => LoadType::Reload,
                        };
                        versions.push(ZoneVersion {
                            serial: None,
                            status: ZoneVersionStatus::Enqueued(load_type),
                        });
                    }
                    let load_type = match refreshes.ongoing {
                        OngoingRefresh::Refresh { .. } => LoadType::Refresh,
                        OngoingRefresh::Reload { .. } => LoadType::Reload,
                    };
                    versions.push(ZoneVersion {
                        serial: None,
                        status: ZoneVersionStatus::Loading(load_type),
                    });
                }

                if let Some(contents) = &state.contents {
                    let serial = contents.latest.soa.rdata.serial;
                    versions.push(ZoneVersion {
                        serial: Some(serial.into()),
                        // TODO: Published is not (always) right.
                        status: ZoneVersionStatus::Published,
                    });

                    // TODO: We should stop this loop once we found a zone that
                    // is actually published. The rest doesn't matter.
                    for zone in contents.previous.iter().rev() {
                        let serial = zone.soa.rdata.serial;
                        versions.push(ZoneVersion {
                            serial: Some(serial.into()),
                            // TODO: Published is not (always) right.
                            status: ZoneVersionStatus::Published,
                        });
                    }
                }

                ZoneListEntry { name, versions }
            })
            .collect();
        Json(ZonesListResult { zones })
    }

    async fn zone_status(
        Path(name): Path<Name<Bytes>>,
    ) -> Json<ZoneStatusResult> {
        Json(ZoneStatusResult { name })
    }

    async fn zone_change_source(
        State(state): State<Arc<HttpServer>>,
        Path(payload): Path<Name<Bytes>>,
        Json(zone_source): Json<ZoneSource>,
    ) -> Result<Json<ZoneReloadResult>, String> {
        // TODO: This conversion needs to be improved
        let mut bytes = Vec::new();
        payload.compose(&mut bytes).unwrap();
        let name = RevNameBuf::parse_bytes(&bytes).unwrap();

        let Some(zone) = state.zones.get(&name) else {
            return Err("zone does not exist".into());
        };

        let mut zone_state = zone.data.lock().unwrap();

        let source = match zone_source {
            ZoneSource::Zonefile { path } => Source::Zonefile { path },
            ZoneSource::Server { addr } => Source::Server {
                addr: DnsServerAddr {
                    ip: addr.ip(),
                    tcp_port: addr.port(),
                    udp_port: Some(addr.port()),
                },
            },
        };

        LoaderState::set_source(&mut zone_state, &zone, source, &state.loader);

        Ok(Json(ZoneReloadResult { name: payload }))
    }

    async fn zone_reload(
        State(state): State<Arc<HttpServer>>,
        Path(payload): Path<Name<Bytes>>,
    ) -> Result<Json<ZoneReloadResult>, String> {
        // TODO: This conversion needs to be improved
        let mut bytes = Vec::new();
        payload.compose(&mut bytes).unwrap();
        let name = RevNameBuf::parse_bytes(&bytes).unwrap();

        let Some(zone) = state.zones.get(&name) else {
            return Err("zone does not exist".into());
        };

        let mut zone_state = zone.data.lock().unwrap();
        LoaderState::enqueue_refresh(
            &mut zone_state,
            &zone,
            true,
            &state.loader,
        );

        Ok(Json(ZoneReloadResult { name: payload }))
    }

    async fn status() -> Json<ServerStatusResult> {
        Json(ServerStatusResult {})
    }
}
