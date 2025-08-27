use std::borrow::Cow;
use std::collections::HashMap;
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
use axum::extract::OriginalUri;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::response::Html;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use bytes::Bytes;
use domain::base::Name;
use domain::base::Serial;
use domain::zonetree::StoredName;
use log::warn;
use log::{debug, error, info};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::api::ServerStatusResult;
use crate::api::ZoneRegister;
use crate::api::ZoneRegisterResult;
use crate::api::ZoneReloadResult;
use crate::api::ZoneStatusResult;
use crate::api::ZonesListResult;
use crate::comms::{ApplicationCommand, Terminated};
use crate::manager::Component;
// use crate::zone::Zones;

const HTTP_UNIT_NAME: &str = "HS";

// NOTE: To send data back from a unit, send them an app command with
// a transmitter they can use to send the reply

pub struct HttpServer {
    pub listen_addr: SocketAddr,
    // pub zones: Arc<Zones>,
    pub cmd_rx: Option<mpsc::Receiver<ApplicationCommand>>,
}

struct HttpServerState {
    pub component: Component,
}

impl HttpServer {
    pub async fn run(mut self, component: Component) -> Result<(), Terminated> {
        // let _component = Arc::new(RwLock::new(component));
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

        let state = Arc::new(HttpServerState { component });

        // For now, only implemented the ZoneReviewApi for the Units:
        // "RS"   ZoneServerUnit
        // "RS2"  ZoneServerUnit
        // "PS"   ZoneServerUnit
        //
        // Skipping SigningHistoryApi and ZoneListApi's for
        // "ZL"   ZoneLoader
        // "ZS"   ZoneSignerUnit
        // "CC"   CentralCommand
        //
        // Noting them down, but without a previously existing API:
        // "HS"   HttpServer
        // "KM"   KeyManagerUnit

        let unit_router = Router::new()
            .route("/ps/", get(Self::handle_ps_base))
            .route("/ps/{action}/{token}", get(Self::handle_ps))
            .route("/rs/", get(Self::handle_rs_base))
            .route("/rs/{action}/{token}", get(Self::handle_rs))
            .route("/rs2/", get(Self::handle_rs2_base))
            .route("/rs2/{action}/{token}", get(Self::handle_rs2));
            // .route("/zl/", get(Self::handle_zl_base))
            // .route("/zs/", get(Self::handle_zs_base));

        let app = Router::new()
            .route("/", get(|| async { "Hello, World!" }))
            .nest("/_unit", unit_router)
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

    async fn zone_register(Json(payload): Json<ZoneRegister>) -> Json<ZoneRegisterResult> {
        Json(ZoneRegisterResult {
            name: payload.name.clone(),
            status: "Maybe Success".into(),
        })
    }

    async fn zones_list(State(_state): State<Arc<HttpServerState>>) -> Json<ZonesListResult> {
        // let zones = state
        //     .zones
        //     .list()
        //     .iter()
        //     .map(|n| {
        //         // TODO: find a way to get correct `Name`s back from `Zones`
        //         let mut sv = Vec::new();
        //         let mut buf = String::with_capacity(256);
        //         let mut first = true;
        //         for l in n.labels() {
        //             sv.push(l);
        //         }
        //         sv.reverse();
        //         for l in sv {
        //             if !first {
        //                 buf.push('.');
        //             }
        //             buf.push_str(&l.to_string());
        //             first = false;
        //         }
        //         Name::from_str(&buf).unwrap()
        //     })
        //     .collect();
        // Json(ZonesListResult { zones })
        Json(ZonesListResult {
            zones: Vec::default(),
        })
    }

    async fn zone_status(Path(name): Path<Name<Bytes>>) -> Json<ZoneStatusResult> {
        Json(ZoneStatusResult { name })
    }

    async fn zone_reload(Path(name): Path<Name<Bytes>>) -> Json<ZoneReloadResult> {
        Json(ZoneReloadResult { name })
    }

    async fn status() -> Json<ServerStatusResult> {
        Json(ServerStatusResult {})
    }
}

//------------ HttpServer Handler for /<unit>/ -------------------------------

impl HttpServer {
    //--- /ps/
    async fn handle_ps_base(
        uri: OriginalUri,
        State(state): State<Arc<HttpServerState>>,
    ) -> Result<Html<String>, StatusCode> {
        Self::zone_server_unit_api_base_common("PS", uri, state).await
    }

    async fn handle_ps(
        uri: OriginalUri,
        State(state): State<Arc<HttpServerState>>,
        Path((action, token)): Path<(String, String)>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Result<(), StatusCode> {
        Self::zone_server_unit_api_common("PS", uri, state, action, token, params).await
    }

    // //--- /zs/
    // async fn handle_zs_base(
    //     uri: OriginalUri,
    //     State(state): State<Arc<HttpServerState>>,
    // ) -> Result<Html<String>, StatusCode> {
    //     Self::zone_server_unit_api_base_common("ZS", uri, state).await
    // }

    // async fn handle_zs(
    //     uri: OriginalUri,
    //     State(state): State<Arc<HttpServerState>>,
    //     Path((action, token)): Path<(String, String)>,
    //     Query(params): Query<HashMap<String, String>>,
    // ) -> Result<(), StatusCode> {
    //     Self::zone_server_unit_api_common("ZS", uri, state, action, token, params).await
    // }

    // //--- /zl/
    // async fn handle_zl_base(
    //     uri: OriginalUri,
    //     State(state): State<Arc<HttpServerState>>,
    // ) -> Result<Html<String>, StatusCode> {
    //     Self::zone_server_unit_api_base_common("ZL", uri, state).await
    // }

    // async fn handle_zl(
    //     uri: OriginalUri,
    //     State(state): State<Arc<HttpServerState>>,
    //     Path((action, token)): Path<(String, String)>,
    //     Query(params): Query<HashMap<String, String>>,
    // ) -> Result<(), StatusCode> {
    //     Self::zone_server_unit_api_common("ZL", uri, state, action, token, params).await
    // }

    //--- /rs2/
    async fn handle_rs2_base(
        uri: OriginalUri,
        State(state): State<Arc<HttpServerState>>,
    ) -> Result<Html<String>, StatusCode> {
        Self::zone_server_unit_api_base_common("RS2", uri, state).await
    }

    async fn handle_rs2(
        uri: OriginalUri,
        State(state): State<Arc<HttpServerState>>,
        Path((action, token)): Path<(String, String)>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Result<(), StatusCode> {
        Self::zone_server_unit_api_common("RS2", uri, state, action, token, params).await
    }

    //--- /rs/
    async fn handle_rs_base(
        uri: OriginalUri,
        State(state): State<Arc<HttpServerState>>,
    ) -> Result<Html<String>, StatusCode> {
        Self::zone_server_unit_api_base_common("RS", uri, state).await
    }

    async fn handle_rs(
        uri: OriginalUri,
        State(state): State<Arc<HttpServerState>>,
        Path((action, token)): Path<(String, String)>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Result<(), StatusCode> {
        Self::zone_server_unit_api_common("RS", uri, state, action, token, params).await
    }


    //--- common api implementations
    async fn zone_server_unit_api_base_common(
        unit: &str,
        uri: OriginalUri,
        state: Arc<HttpServerState>,
    ) -> Result<Html<String>, StatusCode> {
        let (tx, mut rx) = mpsc::channel(10);
        state
            .component
            .send_command(
                unit,
                ApplicationCommand::HandleZoneReviewApiStatus { http_tx: tx },
            )
            .await;

        let res = rx.recv().await;
        let Some(res) = res else {
            // Failed to receive response... When would that happen?
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        };

        debug!(
            "[{HTTP_UNIT_NAME}]: Handled HTTP request: {}",
            uri.path_and_query()
                .map(|p| { p.as_str() })
                .unwrap_or_default()
        );

        Ok(Html(res))
    }

    // All ZoneServerUnit's have the same review API
    //
    // API: GET /{approve,reject}/<approval token>?zone=<zone name>&serial=<zone serial>
    //
    // NOTE: We use query parameters for the zone details because dots that appear in zone names
    // are decoded specially by HTTP standards compliant libraries, especially occurences of
    // handling of /./ are problematic as that gets collapsed to /.
    async fn zone_server_unit_api_common(
        unit: &str,
        uri: OriginalUri,
        state: Arc<HttpServerState>,
        action: String,
        token: String,
        params: HashMap<String, String>,
    ) -> Result<(), StatusCode> {
        let uri = uri.path_and_query().map(|p| p.as_str()).unwrap_or_default();
        let zone_name = params.get("zone");
        let zone_serial = params.get("serial");
        if matches!(action.as_ref(), "approve" | "reject")
            && zone_name.is_some()
            && zone_serial.is_some()
            && token.len() > 0
        {
            let zone_name = zone_name.unwrap();
            let zone_serial = zone_serial.unwrap();

            if let Ok(zone_name) = Name::<Bytes>::from_str(zone_name) {
                if let Ok(zone_serial) = Serial::from_str(zone_serial) {
                    let (tx, mut rx) = mpsc::channel(10);
                    state
                        .component
                        .send_command(
                            unit,
                            ApplicationCommand::HandleZoneReviewApi {
                                zone_name,
                                zone_serial,
                                approval_token: token,
                                operation: action,
                                http_tx: tx,
                            },
                        )
                        .await;

                    let res = rx.recv().await;
                    let Some(res) = res else {
                        // Failed to receive response... When would that happen?
                        return Err(StatusCode::INTERNAL_SERVER_ERROR);
                    };

                    let ret = match res {
                        Ok(_) => Ok(()),
                        Err(_) => Err(StatusCode::BAD_REQUEST),
                    };
                    // TODO: make debug when setting log level is fixed
                    warn!("[{HTTP_UNIT_NAME}]: Handled HTTP request: {uri} :: {ret:?}");
                    // debug!("[{HTTP_UNIT_NAME}]: Handled HTTP request: {uri} :: {ret:?}");

                    return ret;
                } else {
                    warn!("[{HTTP_UNIT_NAME}]: Invalid zone serial '{zone_serial}' in request.");
                }
            } else {
                warn!("[{HTTP_UNIT_NAME}]: Invalid zone name '{zone_name}' in request.");
            }
        }
        warn!("[{HTTP_UNIT_NAME}]: Invalid HTTP request: {uri}");
        Err(StatusCode::BAD_REQUEST)
    }
}
