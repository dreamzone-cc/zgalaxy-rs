use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tracing::{info, warn};
use crate::controller::EmbeddedController;
use crate::identity::Identity;
use crate::network::NetworkManager;
use crate::peer::PeerManager;
use crate::resolver::DynamicDnsResolver;

#[derive(Clone)]
pub struct AppState {
    pub identity: Identity,
    pub auth_token: String,
    pub peer_manager: PeerManager,
    pub network_manager: NetworkManager,
    pub controller: EmbeddedController,
    pub resolver: Arc<DynamicDnsResolver>,
}

pub struct ControllerServer;

impl ControllerServer {
    pub fn build_router(state: AppState) -> Router {
        Router::new()
            // Standard ZeroTier Node Status
            .route("/status", get(get_status))
            .route("/controller", get(get_controller_status))
            .route("/metrics", get(get_metrics))
            
            // Client Network Join/Leave Routes
            .route("/network", get(list_networks))
            .route("/network/:nwid", post(join_network).delete(leave_network))
            
            // Peer Discovery Routes
            .route("/peer", get(list_peers))
            .route("/peer/:address", get(get_peer_details))
            
            // ZeroTier Controller Network Management Routes (100% Backward Compatible with ZTNET)
            .route("/controller/network", get(list_controller_networks).post(create_controller_network))
            .route("/controller/network/:nwid", get(get_controller_network).post(update_controller_network).delete(delete_controller_network))
            
            // ZeroTier Controller Member Management Routes
            .route("/controller/network/:nwid/member", get(list_controller_members))
            .route("/controller/network/:nwid/member/:member_id", get(get_controller_member).post(update_controller_member).delete(delete_controller_member))
            
            // Dynamic Domain Management Routes (Runtime Provisioning without Rebuild)
            .route("/api/v1/domains", get(list_dynamic_domains).post(add_dynamic_domain))
            .route("/api/v1/domains/:domain", delete(remove_dynamic_domain))
            .route("/api/v1/domains/sync", post(sync_dynamic_domains))
            .with_state(state)
    }

    pub async fn start(state: AppState, port: u16) -> anyhow::Result<()> {
        let app = Self::build_router(state);
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        info!("[ZGALAXY LOCAL REST API] Listening on http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

// Authentication Helper
fn check_auth(headers: &HeaderMap, expected_token: &str) -> bool {
    if let Some(auth_val) = headers.get("X-ZT1-Auth") {
        if let Ok(token_str) = auth_val.to_str() {
            return token_str.trim() == expected_token.trim();
        }
    }
    if let Some(auth_val) = headers.get("Authorization") {
        if let Ok(token_str) = auth_val.to_str() {
            if let Some(bearer) = token_str.strip_prefix("Bearer ") {
                return bearer.trim() == expected_token.trim();
            }
            if let Some(token) = token_str.strip_prefix("token ") {
                return token.trim() == expected_token.trim();
            }
        }
    }
    false
}

async fn get_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Ok(Json(json!({
        "address": state.identity.address.to_string(),
        "publicIdentity": state.identity.to_public_string(),
        "planetWorldId": 0,
        "planetWorldTimestamp": now,
        "version": "1.3.0",
        "versionMajor": 1,
        "versionMinor": 3,
        "versionRev": 0,
        "clock": now,
        "online": true,
        "tcpFallbackActive": false
    })))
}

async fn get_controller_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(Json(json!({
        "controller": true,
        "apiVersion": 1,
        "clock": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "instanceId": format!("zgalaxy_rs_{}", state.identity.address)
    })))
}

async fn get_metrics() -> impl IntoResponse {
    const METRICS: &str = "# HELP zgalaxy_controller_status Status of ZGALAXY controller\n# TYPE zgalaxy_controller_status gauge\nzgalaxy_controller_status 1\n# HELP zgalaxy_version Controller version\n# TYPE zgalaxy_version gauge\nzgalaxy_version 1\n";
    (
        StatusCode::OK,
        [("Content-Type", "text/plain; version=0.0.4")],
        METRICS,
    )
}

async fn list_networks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let nets = state.network_manager.list().await;
    Ok(Json(nets))
}

async fn join_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state.network_manager.join(&nwid).await {
        Ok(net) => Ok(Json(net)),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

async fn leave_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state.network_manager.leave(&nwid).await {
        Ok(true) => Ok(Json(json!({ "nwid": nwid, "deleted": true }))),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

async fn list_peers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let peers = state.peer_manager.list_peers().await;
    Ok(Json(peers))
}

async fn get_peer_details(
    State(state): State<AppState>,
    Path(address_str): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Ok(addr) = address_str.parse() {
        if let Some(peer) = state.peer_manager.get_peer(&addr).await {
            return Ok(Json(peer));
        }
    }
    Err(StatusCode::NOT_FOUND)
}

// ----------------------------------------------------------------------------
// ZeroTier Controller Endpoints (Preserved for ZTNET / Automation Integration)
// ----------------------------------------------------------------------------

async fn list_controller_networks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let list = state.controller.list_networks().await;
    Ok(Json(list))
}

async fn create_controller_network(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match state.controller.save_network(payload).await {
        Ok(net) => Ok(Json(serde_json::to_value(net).unwrap_or_default())),
        Err(e) => {
            warn!("[ZGALAXY CONTROLLER ERROR] Failed to create network: {:?}", e);
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

async fn get_controller_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Some(net) = state.controller.get_network(&nwid).await {
        return Ok(Json(serde_json::to_value(net).unwrap_or_default()));
    }
    Err(StatusCode::NOT_FOUND)
}

async fn update_controller_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
    Json(mut payload): Json<Value>,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !nwid.contains("______") {
        payload["id"] = json!(nwid);
        payload["nwid"] = json!(nwid);
    }

    match state.controller.save_network(payload).await {
        Ok(net) => Ok(Json(serde_json::to_value(net).unwrap_or_default())),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

async fn delete_controller_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state.controller.delete_network(&nwid).await {
        Ok(true) => Ok(Json(json!({ "id": nwid, "deleted": true }))),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

async fn list_controller_members(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let map = state.controller.list_members(&nwid).await;
    Ok(Json(json!(map)))
}

async fn get_controller_member(
    State(state): State<AppState>,
    Path((nwid, member_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Some(member) = state.controller.get_member(&nwid, &member_id).await {
        return Ok(Json(serde_json::to_value(member).unwrap_or_default()));
    }
    Err(StatusCode::NOT_FOUND)
}

async fn update_controller_member(
    State(state): State<AppState>,
    Path((nwid, member_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state.controller.save_member(&nwid, &member_id, payload).await {
        Ok(member) => Ok(Json(serde_json::to_value(member).unwrap_or_default())),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

async fn delete_controller_member(
    State(state): State<AppState>,
    Path((nwid, member_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state.controller.delete_member(&nwid, &member_id).await {
        Ok(true) => Ok(Json(json!({ "id": member_id, "deleted": true }))),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

// ----------------------------------------------------------------------------
// Dynamic Domain Management Endpoints (Runtime Provisioning)
// ----------------------------------------------------------------------------

async fn list_dynamic_domains(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let domains = state.resolver.list_domains().await;
    let addrs = state.resolver.get_all_active_addresses().await;
    Ok(Json(json!({
        "success": true,
        "data": {
            "domains": domains,
            "resolved_addresses": addrs
        }
    })))
}

async fn add_dynamic_domain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let domain = payload.get("domain").and_then(|v| v.as_str()).unwrap_or("").trim();
    let port = payload.get("port").and_then(|v| v.as_u64()).unwrap_or(9993) as u16;
    let desc = payload.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());

    if domain.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    match state.resolver.add_domain(domain, port, desc).await {
        Ok(_) => Ok(Json(json!({ "success": true, "domain": domain, "port": port }))),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

async fn remove_dynamic_domain(
    State(state): State<AppState>,
    Path(domain): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match state.resolver.remove_domain(&domain, 9993).await {
        Ok(true) => Ok(Json(json!({ "success": true, "removed": domain }))),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

async fn sync_dynamic_domains(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    state.resolver.check_and_update_all().await;
    let addrs = state.resolver.get_all_active_addresses().await;
    Ok(Json(json!({ "success": true, "synced_addresses": addrs })))
}
