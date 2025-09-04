use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::OriginalUri;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use bytes::Bytes;
use domain::base::Name;
use domain::base::Serial;
use log::warn;
use log::{debug, error, info};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::api::PolicyListResult;
use crate::api::PolicyReloadResult;
use crate::api::PolicyShowResult;
use crate::api::ServerStatusResult;
use crate::api::ZoneAdd;
use crate::api::ZoneAddResult;
use crate::api::ZoneReloadResult;
use crate::api::ZoneRemoveResult;
use crate::api::ZoneStage;
use crate::api::ZoneStatusResult;
use crate::api::ZonesListEntry;
use crate::api::ZonesListResult;
use crate::center;
use crate::center::Center;
use crate::comms::{ApplicationCommand, Terminated};
use crate::zone;
// use crate::zone::Zones;

const HTTP_UNIT_NAME: &str = "HS";

// NOTE: To send data back from a unit, send them an app command with
// a transmitter they can use to send the reply

pub struct HttpServer {
    pub center: Arc<Center>,
    pub listen_addr: SocketAddr,
}

struct HttpServerState {
    pub center: Arc<Center>,
}

impl HttpServer {
    pub async fn run(
        self,
        mut cmd_rx: mpsc::UnboundedReceiver<ApplicationCommand>,
    ) -> Result<(), Terminated> {
        // Setup listener
        let sock = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })?;

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

        let state = Arc::new(HttpServerState {
            center: self.center,
        });

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
            // Using the /_unit sub-path to not clutter the rest of the API
            .nest("/_unit", unit_router)
            .route("/status", get(Self::status))
            .route("/zones/list", get(Self::zones_list))
            .route("/zone/add", post(Self::zone_add))
            .route("/zone/{name}/remove", post(Self::zone_remove))
            .route("/zone/{name}/status", get(Self::zone_status))
            .route("/zone/{name}/reload", post(Self::zone_reload))
            .route("/policy/reload", post(Self::policy_reload))
            .route("/policy/list", get(Self::policy_list))
            .route("/policy/{name}", post(Self::policy_show))
            .with_state(state);

        axum::serve(sock, app).await.map_err(|e| {
            error!("[{HTTP_UNIT_NAME}]: {}", e);
            Terminated
        })
    }

    async fn zone_add(
        State(state): State<Arc<HttpServerState>>,
        Json(zone_register): Json<ZoneAdd>,
    ) -> Json<ZoneAddResult> {
        // TODO: Use the result.
        let _ = center::add_zone(&state.center, zone_register.name.clone());
        let _ = zone::change_policy(
            &state.center,
            zone_register.name.clone(),
            zone_register.policy.clone().into(),
        );

        let zone_name = zone_register.name.clone();
        state
            .center
            .app_cmd_tx
            .send((
                "ZL".into(),
                ApplicationCommand::RegisterZone {
                    register: zone_register,
                },
            ))
            .unwrap();
        Json(ZoneAddResult {
            name: zone_name,
            status: "Submitted".to_string(),
        })
    }

    async fn zone_remove(
        State(state): State<Arc<HttpServerState>>,
        Path(name): Path<Name<Bytes>>,
    ) -> Json<ZoneRemoveResult> {
        // TODO: Use the result.
        let _ = center::remove_zone(&state.center, name);

        Json(ZoneRemoveResult {})
    }

    async fn zones_list(State(state): State<Arc<HttpServerState>>) -> Json<ZonesListResult> {
        // The zone trees in the Component overlap. Therefore we take the
        // furthest a zone has progressed. We use a BTreeMap to sort the zones
        // while we're doing this.
        let mut all_zones = BTreeMap::new();

        let unsigned_zones = state.center.unsigned_zones.load();
        for zone in unsigned_zones.iter_zones() {
            all_zones.insert(zone.apex_name().clone(), ZoneStage::Unsigned);
        }

        let unsigned_zones = state.center.signed_zones.load();
        for zone in unsigned_zones.iter_zones() {
            all_zones.insert(zone.apex_name().clone(), ZoneStage::Signed);
        }

        let unsigned_zones = state.center.published_zones.load();
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

    async fn policy_list() -> Json<PolicyListResult> {
        todo!()
    }

    async fn policy_reload() -> Json<PolicyReloadResult> {
        todo!()
    }

    async fn policy_show() -> Json<PolicyShowResult> {
        todo!()
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
            .center
            .app_cmd_tx
            .send((
                unit.into(),
                ApplicationCommand::HandleZoneReviewApiStatus { http_tx: tx },
            ))
            .unwrap();

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

        let Some(zone_name) = params.get("zone") else {
            warn!("[{HTTP_UNIT_NAME}]: Invalid HTTP request: {uri}");
            return Err(StatusCode::BAD_REQUEST);
        };

        let Some(zone_serial) = params.get("serial") else {
            warn!("[{HTTP_UNIT_NAME}]: Invalid HTTP request: {uri}");
            return Err(StatusCode::BAD_REQUEST);
        };

        if token.is_empty() || !["approve", "reject"].contains(&action.as_ref()) {
            warn!("[{HTTP_UNIT_NAME}]: Invalid HTTP request: {uri}");
            return Err(StatusCode::BAD_REQUEST);
        }

        let Ok(zone_name) = Name::<Bytes>::from_str(zone_name) else {
            warn!("[{HTTP_UNIT_NAME}]: Invalid zone name '{zone_name}' in request.");
            return Err(StatusCode::BAD_REQUEST);
        };

        let Ok(zone_serial) = Serial::from_str(zone_serial) else {
            warn!("[{HTTP_UNIT_NAME}]: Invalid zone serial '{zone_serial}' in request.");
            return Err(StatusCode::BAD_REQUEST);
        };

        let (tx, mut rx) = mpsc::channel(10);
        state
            .center
            .app_cmd_tx
            .send((
                unit.into(),
                ApplicationCommand::HandleZoneReviewApi {
                    zone_name,
                    zone_serial,
                    approval_token: token,
                    operation: action,
                    http_tx: tx,
                },
            ))
            .unwrap();

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

        ret
    }
}
