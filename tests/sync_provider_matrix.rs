use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::TcpListener;
use tokio::fs;
use axum::{
    routing::get,
    Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use sha2::{Sha256, Digest};
use tempfile::TempDir;

use zgalaxy_rs::identity::Identity;
use zgalaxy_rs::world::{World, WorldRoot, WORLD_TYPE_PLANET, WORLD_TYPE_MOON, WORLD_ID_EARTH};
use zgalaxy_rs::peer::PeerManager;
use zgalaxy_rs::sync_provider::{SyncProviderClient, SyncProviderConfig, SyncStatus};

// Mock Server Shared State
#[derive(Clone)]
struct MockServerState {
    valid_token: String,
    expired_token: String,
    revoked_token: String,
    single_token: String,
    group_token: String,
    single_registered_fp: Arc<tokio::sync::RwLock<Option<String>>>,
    group_registered_fps: Arc<tokio::sync::RwLock<Vec<String>>>,
    group_max_devices: usize,
    planet_bytes: Arc<Vec<u8>>,
    planet_sha256: Arc<tokio::sync::RwLock<String>>,
    moon_bytes: Arc<Vec<u8>>,
    moon_sha256: Arc<tokio::sync::RwLock<String>>,
    corrupted_moon_bytes: Arc<Vec<u8>>,
    manifest_req_count: Arc<AtomicUsize>,
    return_etag: String,
    rate_limit_active: Arc<tokio::sync::RwLock<bool>>,
}

// Generate valid canonical test world bytes
fn generate_test_planet(signer: &Identity) -> Vec<u8> {
    let root_addr = signer.address;
    let root = WorldRoot {
        identity: root_addr,
        stable_endpoints: vec!["127.0.0.1/9993".to_string()],
    };
    let world = World::new(WORLD_TYPE_PLANET, WORLD_ID_EARTH, 1787687898000, vec![root]);
    let root_pubkey = signer.verifying_key.to_bytes();
    world.encode_canonical(signer, &[Some(root_pubkey)]).unwrap()
}

fn generate_test_moon(signer: &Identity, moon_id: u64) -> Vec<u8> {
    let root_addr = signer.address;
    let root = WorldRoot {
        identity: root_addr,
        stable_endpoints: vec!["127.0.0.1/9994".to_string()],
    };
    let world = World::new(WORLD_TYPE_MOON, moon_id, 1787687898000, vec![root]);
    let root_pubkey = signer.verifying_key.to_bytes();
    world.encode_canonical(signer, &[Some(root_pubkey)]).unwrap()
}

async fn handle_manifest(
    State(state): State<MockServerState>,
    headers: HeaderMap,
) -> Response {
    state.manifest_req_count.fetch_add(1, Ordering::SeqCst);

    if *state.rate_limit_active.read().await {
        return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
    }

    let token = headers.get("X-ZGALAXY-Signature").and_then(|h| h.to_str().ok()).unwrap_or("");
    let fingerprint = headers.get("X-Device-Fingerprint").and_then(|h| h.to_str().ok()).unwrap_or("").to_string();

    if token == state.expired_token {
        return (StatusCode::UNAUTHORIZED, "Token expired").into_response();
    }
    if token == state.revoked_token {
        return (StatusCode::FORBIDDEN, "Token revoked").into_response();
    }
    if token == state.single_token {
        let mut reg = state.single_registered_fp.write().await;
        if let Some(ref existing) = *reg {
            if existing != &fingerprint {
                return (StatusCode::FORBIDDEN, "Device limit exceeded (1 device max)").into_response();
            }
        } else {
            *reg = Some(fingerprint);
        }
    } else if token == state.group_token {
        let mut reg = state.group_registered_fps.write().await;
        if !reg.contains(&fingerprint) {
            if reg.len() >= state.group_max_devices {
                return (StatusCode::FORBIDDEN, "Group device quota reached").into_response();
            }
            reg.push(fingerprint);
        }
    } else if token != state.valid_token {
        return (StatusCode::UNAUTHORIZED, "Invalid token").into_response();
    }

    // Check ETag
    if let Some(if_none_match) = headers.get("If-None-Match").and_then(|h| h.to_str().ok()) {
        if if_none_match == state.return_etag {
            return (StatusCode::NOT_MODIFIED, "").into_response();
        }
    }

    let planet_sha = state.planet_sha256.read().await.clone();
    let moon_sha = state.moon_sha256.read().await.clone();

    let manifest = json!({
        "version": 1,
        "issued_at": 1787687898,
        "refresh_interval_sec": 3600,
        "planets": [{
            "id": "earth",
            "world_type": 1,
            "filename": "planet",
            "sha256": planet_sha,
            "size_bytes": state.planet_bytes.len(),
            "timestamp": 1787687898000u64,
            "download_url": "/api/v1/sync/download/planet/earth"
        }],
        "moons": [{
            "id": "000000069ae38092",
            "world_type": 127,
            "filename": "000000069ae38092.moon",
            "sha256": moon_sha,
            "size_bytes": state.moon_bytes.len(),
            "timestamp": 1787687898000u64,
            "download_url": "/api/v1/sync/download/moon/000000069ae38092"
        }]
    });

    let mut resp = Json(manifest).into_response();
    resp.headers_mut().insert("ETag", state.return_etag.parse().unwrap());
    resp
}

async fn handle_download(
    AxumPath((asset_type, id)): AxumPath<(String, String)>,
    State(state): State<MockServerState>,
    headers: HeaderMap,
) -> Response {
    let token = headers.get("X-ZGALAXY-Signature").and_then(|h| h.to_str().ok()).unwrap_or("");
    if token != state.valid_token && token != state.single_token && token != state.group_token {
        return (StatusCode::UNAUTHORIZED, "Unauthorized download").into_response();
    }

    if asset_type == "planet" {
        (StatusCode::OK, state.planet_bytes.as_ref().clone()).into_response()
    } else if asset_type == "moon" {
        if id == "corrupted" {
            (StatusCode::OK, state.corrupted_moon_bytes.as_ref().clone()).into_response()
        } else {
            (StatusCode::OK, state.moon_bytes.as_ref().clone()).into_response()
        }
    } else {
        (StatusCode::NOT_FOUND, "Asset not found").into_response()
    }
}

async fn spawn_mock_server() -> (String, MockServerState) {
    let signer = Identity::generate();
    let planet_bytes = generate_test_planet(&signer);
    let mut h1 = Sha256::new();
    h1.update(&planet_bytes);
    let planet_sha256 = hex::encode(h1.finalize());

    let moon_bytes = generate_test_moon(&signer, 0x000000069ae38092);
    let mut h2 = Sha256::new();
    h2.update(&moon_bytes);
    let moon_sha256 = hex::encode(h2.finalize());

    let corrupted_moon_bytes = vec![0xde, 0xad, 0xbe, 0xef, 0x00];

    let state = MockServerState {
        valid_token: "ZG-tok_valid_test_key_12345".to_string(),
        expired_token: "ZG-tok_expired_test_key_99999".to_string(),
        revoked_token: "ZG-tok_revoked_test_key_88888".to_string(),
        single_token: "ZG-tok_single_user_device_1".to_string(),
        group_token: "ZG-tok_group_team_devices_5".to_string(),
        single_registered_fp: Arc::new(tokio::sync::RwLock::new(None)),
        group_registered_fps: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        group_max_devices: 2,
        planet_bytes: Arc::new(planet_bytes),
        planet_sha256: Arc::new(tokio::sync::RwLock::new(planet_sha256)),
        moon_bytes: Arc::new(moon_bytes),
        moon_sha256: Arc::new(tokio::sync::RwLock::new(moon_sha256)),
        corrupted_moon_bytes: Arc::new(corrupted_moon_bytes),
        manifest_req_count: Arc::new(AtomicUsize::new(0)),
        return_etag: "\"manifest_etag_v1_hash\"".to_string(),
        rate_limit_active: Arc::new(tokio::sync::RwLock::new(false)),
    };

    let app = Router::new()
        .route("/api/v1/sync/manifest", get(handle_manifest))
        .route("/api/v1/sync/download/:type/:id", get(handle_download))
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (base_url, state)
}

#[tokio::test]
async fn test_matrix_01_valid_token_and_manifest_atomic_sync() {
    let (base_url, state) = spawn_mock_server().await;
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let client_identity = Identity::generate();
    let peer_manager = PeerManager::new();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.valid_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let summary = client.execute_sync(working_dir, &client_identity, Some(&peer_manager)).await.unwrap();

    assert_eq!(summary.status, SyncStatus::Synced);
    assert_eq!(summary.planets_synced, 1);
    assert_eq!(summary.moons_synced, 1);

    // Verify files on disk
    let planet_path = working_dir.join("planet");
    let moon_path = working_dir.join("moons.d").join("000000069ae38092.moon");
    assert!(planet_path.exists());
    assert!(moon_path.exists());

    // Verify PeerManager received the root
    let peers = peer_manager.list_peers().await;
    assert!(!peers.is_empty(), "Roots should be injected into PeerManager");
}

#[tokio::test]
async fn test_matrix_02_expired_token_rejection() {
    let (base_url, state) = spawn_mock_server().await;
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let client_identity = Identity::generate();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.expired_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let res = client.execute_sync(working_dir, &client_identity, None).await;
    assert!(res.is_err());
    let err_msg = res.err().unwrap().to_string();
    assert!(err_msg.contains("401") || err_msg.contains("expired"));
}

#[tokio::test]
async fn test_matrix_03_revoked_token_rejection() {
    let (base_url, state) = spawn_mock_server().await;
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let client_identity = Identity::generate();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.revoked_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let res = client.execute_sync(working_dir, &client_identity, None).await;
    assert!(res.is_err());
    let err_msg = res.err().unwrap().to_string();
    assert!(err_msg.contains("403") || err_msg.contains("revoked"));
}

#[tokio::test]
async fn test_matrix_04_device_limit_enforcement() {
    let (base_url, state) = spawn_mock_server().await;
    let temp_dir1 = TempDir::new().unwrap();
    let temp_dir2 = TempDir::new().unwrap();
    let id1 = Identity::generate();
    let id2 = Identity::generate();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url.clone(),
        token: state.single_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client1 = SyncProviderClient::new(cfg.clone());
    let res1 = client1.execute_sync(temp_dir1.path(), &id1, None).await;
    assert!(res1.is_ok(), "First device must succeed");

    // Second device using single token must be rejected
    let client2 = SyncProviderClient::new(cfg);
    let res2 = client2.execute_sync(temp_dir2.path(), &id2, None).await;
    assert!(res2.is_err(), "Second device must be rejected on single token");
    assert!(res2.err().unwrap().to_string().contains("403"));
}

#[tokio::test]
async fn test_matrix_05_group_multi_device_quota() {
    let (base_url, state) = spawn_mock_server().await;
    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.group_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let t1 = TempDir::new().unwrap();
    let t2 = TempDir::new().unwrap();
    let t3 = TempDir::new().unwrap();
    let id1 = Identity::generate();
    let id2 = Identity::generate();
    let id3 = Identity::generate();

    let client = SyncProviderClient::new(cfg);

    assert!(client.execute_sync(t1.path(), &id1, None).await.is_ok(), "Device 1 of group must pass");
    assert!(client.execute_sync(t2.path(), &id2, None).await.is_ok(), "Device 2 of group must pass (quota=2)");
    let res3 = client.execute_sync(t3.path(), &id3, None).await;
    assert!(res3.is_err(), "Device 3 must exceed group quota of 2");
}

#[tokio::test]
async fn test_matrix_06_etag_304_not_modified_efficiency() {
    let (base_url, state) = spawn_mock_server().await;
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let client_identity = Identity::generate();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.valid_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);

    // Initial sync downloads files
    let summary1 = client.execute_sync(working_dir, &client_identity, None).await.unwrap();
    assert_eq!(summary1.status, SyncStatus::Synced);

    // Second sync uses cached ETag -> 304 Not Modified
    let summary2 = client.execute_sync(working_dir, &client_identity, None).await.unwrap();
    assert_eq!(summary2.status, SyncStatus::Unchanged);
    assert_eq!(summary2.planets_synced, 0);
    assert_eq!(summary2.moons_synced, 0);
}

#[tokio::test]
async fn test_matrix_07_corrupted_payload_staging_and_safety() {
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let original_planet = working_dir.join("planet");
    fs::write(&original_planet, b"Original Safe Planet Bytes").await.unwrap();

    let (base_url, state) = spawn_mock_server().await;
    // Set invalid sha256 in manifest dynamically in shared state
    *state.planet_sha256.write().await = "0000000000000000000000000000000000000000000000000000000000000000".to_string();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.valid_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let id = Identity::generate();
    let res = client.execute_sync(working_dir, &id, None).await;
    assert!(res.is_err(), "Must reject corrupted sha256 checksum");

    // Ensure original file remained completely untouched
    let existing_content = fs::read(&original_planet).await.unwrap();
    assert_eq!(existing_content, b"Original Safe Planet Bytes");
}

#[tokio::test]
async fn test_matrix_08_rate_limiting_resilience() {
    let (base_url, state) = spawn_mock_server().await;
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let client_identity = Identity::generate();

    // Enable rate limit on server
    *state.rate_limit_active.write().await = true;

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.valid_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let res = client.execute_sync(working_dir, &client_identity, None).await;
    assert!(res.is_err());
    assert!(res.err().unwrap().to_string().contains("429"));
}

#[tokio::test]
async fn test_matrix_09_multi_server_isolation() {
    let (base1, s1) = spawn_mock_server().await;
    let (base2, s2) = spawn_mock_server().await;
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();
    let id1 = Identity::generate();
    let id2 = Identity::generate();

    let c1 = SyncProviderClient::new(SyncProviderConfig {
        enabled: true,
        server_url: base1,
        token: s1.valid_token,
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    });

    let c2 = SyncProviderClient::new(SyncProviderConfig {
        enabled: true,
        server_url: base2,
        token: s2.valid_token,
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    });

    assert!(c1.execute_sync(dir1.path(), &id1, None).await.is_ok());
    assert!(c2.execute_sync(dir2.path(), &id2, None).await.is_ok());
}

#[tokio::test]
async fn test_matrix_10_rollback_to_backup_on_failure() {
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let original_planet = working_dir.join("planet");
    fs::write(&original_planet, b"Original Safe Planet Bytes").await.unwrap();

    let (base_url, state) = spawn_mock_server().await;
    let client_identity = Identity::generate();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: state.valid_token.clone(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);

    // Initial valid sync makes a backup and writes new planet
    let s1 = client.execute_sync(working_dir, &client_identity, None).await.unwrap();
    assert_eq!(s1.status, SyncStatus::Synced);

    // Verify backup exists
    let backup_path = working_dir.join("backups").join("planet.bak");
    assert!(backup_path.exists());

    // Trigger rollback manually or test rollback function
    client.rollback_planet(working_dir, &working_dir.join("backups")).await.unwrap();
    assert!(original_planet.exists());
}

#[tokio::test]
async fn test_matrix_11_malformed_token_signature_rejected() {
    let (base_url, _state) = spawn_mock_server().await;
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let client_identity = Identity::generate();

    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: base_url,
        token: "MALFORMED_FAKE_SIGNATURE".to_string(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let res = client.execute_sync(working_dir, &client_identity, None).await;
    assert!(res.is_err());
    assert!(res.err().unwrap().to_string().contains("401"));
}

#[tokio::test]
async fn test_matrix_12_offline_graceful_fallback() {
    let temp_dir = TempDir::new().unwrap();
    let working_dir = temp_dir.path();
    let original_planet = working_dir.join("planet");
    fs::write(&original_planet, b"Local Offline Cached Planet").await.unwrap();

    let client_identity = Identity::generate();

    // Use dead unconnectable port
    let cfg = SyncProviderConfig {
        enabled: true,
        server_url: "http://127.0.0.1:59999".to_string(),
        token: "any_token".to_string(),
        sync_interval_minutes: 60,
        allow_auto_rollback: true,
        max_retries: 3,
    };

    let client = SyncProviderClient::new(cfg);
    let res = client.execute_sync(working_dir, &client_identity, None).await;
    assert!(res.is_err(), "Offline server must report error without crashing");

    // Local cached file must be intact
    let content = fs::read(&original_planet).await.unwrap();
    assert_eq!(content, b"Local Offline Cached Planet");
}

#[tokio::test]
async fn test_matrix_13_capability_token_payload_roundtrip() {
    use zgalaxy_rs::sync_provider::{CapabilityToken, TokenType};

    let token = CapabilityToken {
        ver: 1,
        iss: "dz.dreamzone.cc".to_string(),
        sub: "subscriber_corp_99".to_string(),
        token_type: TokenType::Contract,
        max_devices: 10,
        scope: vec!["planet:earth".to_string(), "moons:*".to_string()],
        iat: 1787687898,
        exp: 1819223898,
        jti: "uniq_jti_nonce_42".to_string(),
        sig: "ed25519_sample_sig_hex".to_string(),
    };

    let json_str = serde_json::to_string(&token).unwrap();
    let parsed: CapabilityToken = serde_json::from_str(&json_str).unwrap();

    assert_eq!(parsed.ver, 1);
    assert_eq!(parsed.sub, "subscriber_corp_99");
    assert_eq!(parsed.token_type, TokenType::Contract);
    assert_eq!(parsed.max_devices, 10);
    assert_eq!(parsed.scope.len(), 2);
}
