use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    extract::{ConnectInfo, Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
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
    pub controller_enabled: bool,
    pub resolver: Arc<DynamicDnsResolver>,
    /// CIDRs / plain IPs allowed to reach the management API.
    /// Loopback is always allowed. (local.conf allowManagementFrom)
    pub allow_management_from: Vec<String>,
    pub join_trigger: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

pub struct ControllerServer;

impl ControllerServer {
    pub fn build_router(state: AppState) -> Router {
        Router::new()
            // Standard ZeroTier Node Status
            .route("/status", get(get_status))
            .route("/controller", get(get_controller_status))
            .route("/metrics", get(get_metrics))
            
            // Client Network Join/Leave Routes (ZeroTier local-service contract)
            .route("/network", get(list_networks))
            .route("/network/:nwid", get(get_network).post(join_network).delete(leave_network))
            
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
        let allowed = state.allow_management_from.clone();
        let app = Self::build_router(state).layer(
            axum::middleware::from_fn_with_state(allowed, management_source_guard),
        );
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        info!("[ZGALAXY REST API] Listening on http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        Ok(())
    }
}

// Authentication Helper
fn check_auth(headers: &HeaderMap, expected_token: &str) -> bool {
    use subtle::ConstantTimeEq;
    let expected = expected_token.trim().as_bytes();
    let supplied = headers
        .get("X-ZT1-Auth")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| {
                    s.strip_prefix("Bearer ")
                        .or_else(|| s.strip_prefix("token "))
                })
                .map(str::trim)
        });
    match supplied {
        Some(token) => {
            // Constant-time comparison over equal-length views; length itself
            // is padded so it leaks nothing about the token contents.
            let a = token.as_bytes();
            let equal_len = a.len() == expected.len();
            let ct = a.ct_eq(if equal_len { expected } else { a });
            equal_len & bool::from(ct)
        }
        None => false,
    }
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
        "controller": state.controller_enabled,
        "apiVersion": if state.controller_enabled { 1 } else { 0 },
        "clock": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "instanceId": format!("zgalaxy_rs_{}", state.identity.address)
    })))
}

async fn get_metrics(State(state): State<AppState>, headers: HeaderMap) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    const METRICS: &str = "# HELP zgalaxy_controller_status Status of ZGALAXY controller\n# TYPE zgalaxy_controller_status gauge\nzgalaxy_controller_status 1\n# HELP zgalaxy_version Controller version\n# TYPE zgalaxy_version gauge\nzgalaxy_version 1\n";
    Ok((
        StatusCode::OK,
        [("Content-Type", "text/plain; version=0.0.4")],
        METRICS,
    ))
}

async fn list_networks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let nets = state.network_manager.list().await;
    let arr: Vec<serde_json::Value> = nets.iter().map(network_to_zt_json).collect();
    Ok(Json(serde_json::Value::Array(arr)))
}

async fn get_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state.network_manager.get(&nwid.to_lowercase()).await {
        Some(net) => Ok(Json(network_to_zt_json(&net))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn join_network(
    State(state): State<AppState>,
    Path(nwid): Path<String>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<impl IntoResponse, StatusCode> {
    if !check_auth(&headers, &state.auth_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let clean = nwid.to_lowercase();
    let settings = match body {
        Ok(Json(v)) => crate::network::NetworkSettingsUpdate::from_json(&v),
        Err(_) => crate::network::NetworkSettingsUpdate::default(),
    };

    // If request explicitly asks to disconnect (connected: false):
    // Perform pure disconnect, update status, and cleanly tear down routes and interface IPs.
    // Preserves the network and its configuration permanently on disk!
    if settings.connected == Some(false) {
        if let Some(disc_net) = state.network_manager.disconnect(&clean).await {
            for ip in &disc_net.assigned_addresses {
                let _ = crate::route_manager::RouteManager::remove_interface_ip(&disc_net.port_device_name, ip);
                let _ = crate::route_manager::RouteManager::remove_interface_ip("zgalaxy0", ip);
            }
            for r in &disc_net.routes {
                let _ = crate::route_manager::RouteManager::remove_route(&disc_net.port_device_name, &r.target);
                let _ = crate::route_manager::RouteManager::remove_route("zgalaxy0", &r.target);
            }
            tracing::info!("[ZGALAXY CLIENT API] Disconnected network {} (retained in client)", clean);
            return Ok(Json(network_to_zt_json(&disc_net)));
        }
    }

    match state.network_manager.join(&clean).await {
        Ok(net) => {
            if let Some(ref tx) = state.join_trigger {
                let _ = tx.send(clean.clone());
            }
            if !settings.is_empty() {
                if let Some(updated) =
                    state.network_manager.update_settings(&clean, settings).await
                {
                    return Ok(Json(network_to_zt_json(&updated)));
                }
            }
            Ok(Json(network_to_zt_json(&net)))
        }
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
    let clean = nwid.to_lowercase();

    // Mandatory Pre-Disconnect Protocol:
    // If the network exists and was connected or has assigned IPs/routes,
    // execute complete disconnect and interface teardown FIRST before deletion.
    if let Some(existing) = state.network_manager.get(&clean).await {
        if existing.connected || existing.status != crate::network::NetworkStatus::Disconnected {
            tracing::info!("[ZGALAXY CLIENT API] Pre-disconnecting network {} before removal", clean);
            let _ = state.network_manager.disconnect(&clean).await;
        }
        for ip in &existing.assigned_addresses {
            let _ = crate::route_manager::RouteManager::remove_interface_ip(&existing.port_device_name, ip);
            let _ = crate::route_manager::RouteManager::remove_interface_ip("zgalaxy0", ip);
        }
        for r in &existing.routes {
            let _ = crate::route_manager::RouteManager::remove_route(&existing.port_device_name, &r.target);
            let _ = crate::route_manager::RouteManager::remove_route("zgalaxy0", &r.target);
        }
    }

    match state.network_manager.leave(&clean).await {
        Ok(Some(_)) => {
            tracing::info!("[ZGALAXY CLIENT API] Removed network {} completely from client and disk", clean);
            Ok(Json(json!({ "result": true, "deleted": true, "nwid": clean })))
        }
        Ok(None) => Ok(Json(json!({ "result": false, "deleted": false }))),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(member) = state.controller.get_effective_member(&nwid, &member_id).await {
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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
    if !state.controller_enabled {
        return Err(StatusCode::FORBIDDEN);
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

    match state.resolver.remove_domain_any(&domain).await {
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

// ---------------------------------------------------------------------------
// Local network membership API — mirrors ZeroTier's local service contract:
//   GET    /network          -> JSON array of joined networks
//   GET    /network/{nwid}   -> single network, 404 if not joined
//   POST   /network/{nwid}   -> join (idempotent)
//   DELETE /network/{nwid}   -> leave, {"result": true} if it existed
// ---------------------------------------------------------------------------

fn network_to_zt_json(net: &crate::network::Network) -> serde_json::Value {
    use serde_json::json;
    let status = match net.status {
        crate::network::NetworkStatus::Ok => "OK",
        crate::network::NetworkStatus::AccessDenied => "ACCESS_DENIED",
        crate::network::NetworkStatus::NotFound => "NOT_FOUND",
        crate::network::NetworkStatus::PortError => "PORT_ERROR",
        crate::network::NetworkStatus::RequestingConfiguration => "REQUESTING_CONFIGURATION",
        crate::network::NetworkStatus::Disconnected => "DISCONNECTED",
    };
    json!({
        "id": net.nwid,
        "nwid": net.nwid,
        "name": net.name,
        "status": status,
        "connected": net.connected,
        "type": net.type_name,
        "mac": net.mac,
        "mtu": net.mtu,
        "dhcp": false,
        "bridge": false,
        "broadcastEnabled": net.broadcast_enabled,
        "portError": 0,
        "netconfRevision": 1,
        "portDeviceName": net.port_device_name,
        "allowManaged": net.allow_managed,
        "allowGlobal": net.allow_global,
        "allowDefault": net.allow_default,
        "allowDNS": net.allow_dns,
        "assignedAddresses": net.assigned_addresses,
        "routes": net.routes.iter().map(|r| json!({
            "target": r.target,
            "via": r.via,
            "flags": 0,
            "metric": 0,
        })).collect::<Vec<_>>(),
        "multicastSubscriptions": [],
        "dns": { "domain": "", "servers": [] },
        // Observability for أ1: the controller-signed membership token this
        // client will present on its next config request (null until issued).
        "membershipToken": net.membership_token,
    })
}

/// Enforce local.conf `allowManagementFrom` on every management route.
/// Loopback is always allowed; everything else must match a listed
/// CIDR / IP. Applied before bearer-token checks (defense in depth).
async fn management_source_guard(
    State(allowed): State<Vec<String>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !ip_allowed(peer, &allowed) {
        warn!(
            "[ZGALAXY REST API] management access from {} refused by allowManagementFrom",
            peer
        );
        return (StatusCode::FORBIDDEN, "management access not allowed from this address").into_response();
    }
    next.run(req).await
}

fn ip_allowed(peer: SocketAddr, allowed: &[String]) -> bool {
    // An IPv4 client on a dual-stack socket shows up as ::ffff:a.b.c.d;
    // compare it as the IPv4 address it is.
    let ip = peer.ip().to_canonical();
    if ip.is_loopback() {
        return true;
    }
    allowed.iter().any(|entry| {
        if let Ok(cidr) = entry.parse::<ipnet::IpNet>() {
            return cidr.contains(&ip);
        }
        entry
            .parse::<std::net::IpAddr>()
            .map(|a| a == ip)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_guard_handles_cidrs_plain_ips_and_v4_mapped() {
        let allowed = vec!["172.31.255.0/29".to_string(), "10.0.0.5".to_string()];
        let ok = |s: &str| ip_allowed(s.parse().unwrap(), &allowed);
        assert!(ok("127.0.0.1:1"), "loopback is always allowed");
        assert!(ok("[::1]:1"));
        assert!(ok("172.31.255.3:1"));
        assert!(ok("10.0.0.5:1"));
        assert!(ok("[::ffff:172.31.255.3]:1"), "v4-mapped client must match its IPv4 entry");
        assert!(ok("[::ffff:127.0.0.1]:1"), "v4-mapped loopback is loopback");
        assert!(!ok("172.31.255.9:1"));
        assert!(!ok("[2001:db8::1]:1"));
    }
}
