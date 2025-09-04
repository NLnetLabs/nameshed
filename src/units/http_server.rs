use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::Command;
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
use domain::base::iana::Class;
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
        let res = zone::change_policy(
            &state.center,
            zone_register.name.clone(),
            zone_register.policy.clone().into(),
        );

        if let Err(e) = res {
            error!("Could not change policy: {e}");
        }

        let zone_name = zone_register.name.clone();
        state
            .center
            .app_cmd_tx
            .send((
                "ZL".into(),
                ApplicationCommand::RegisterZone {
                    register: zone_register.clone(),
                },
            ))
            .unwrap();

        state
            .center
            .app_cmd_tx
            .send((
                "KM".into(),
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

    async fn zones_list(State(http_state): State<Arc<HttpServerState>>) -> Json<ZonesListResult> {
        let state = http_state.center.state.lock().unwrap();
        let names: Vec<_> = state.zones.iter().map(|z| z.0.name.clone()).collect();
        drop(state);

        let zones = names
            .iter()
            .map(|z| Self::get_zone_status(http_state.clone(), z))
            .collect();

        Json(ZonesListResult { zones })
    }

    async fn zone_status(
        State(state): State<Arc<HttpServerState>>,
        Path(name): Path<Name<Bytes>>,
    ) -> Json<ZoneStatusResult> {
        Json(Self::get_zone_status(state, &name))
    }

    fn get_zone_status(state: Arc<HttpServerState>, name: &Name<Bytes>) -> ZoneStatusResult {
        let center = &state.center;

        let state = center.state.lock().unwrap();
        let zone = state.zones.get(name).unwrap();
        let zone_state = zone.0.state.lock().unwrap();

        // TODO: Needs some info from the zone loader?
        let source = "<unimplemented>".into();

        let policy = zone_state
            .policy
            .as_ref()
            .map_or("<none>".into(), |p| p.name.to_string());

        // TODO: We need to show multiple versions here
        let stage = if center
            .published_zones
            .load()
            .get_zone(&name, Class::IN)
            .is_some()
        {
            ZoneStage::Published
        } else if center
            .signed_zones
            .load()
            .get_zone(&name, Class::IN)
            .is_some()
        {
            ZoneStage::Signed
        } else {
            ZoneStage::Unsigned
        };

        let dnst_binary = &state.config.dnst_binary_path;
        let keys_dir = &state.config.keys_dir;
        let cfg = keys_dir.join(format!("{name}.cfg"));
        let key_status = Command::new(dnst_binary.as_std_path())
            .arg("keyset")
            .arg("-c")
            .arg(cfg)
            .arg("status")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

        ZoneStatusResult {
            name: name.clone(),
            source,
            policy,
            stage,
            key_status,
        }
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
