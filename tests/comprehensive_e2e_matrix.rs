use std::net::SocketAddr;
use std::sync::Arc;
use bytes::Bytes;
use serde_json::json;
use tempfile::tempdir;

use zgalaxy_rs::identity::{Address, Identity};
use zgalaxy_rs::packet::{Packet, PacketType, PACKET_HEADER_SIZE};
use zgalaxy_rs::world::{World, WorldRoot, WORLD_TYPE_PLANET};
use zgalaxy_rs::resolver::DynamicDnsResolver;
use zgalaxy_rs::controller::EmbeddedController;
use zgalaxy_rs::controller_api::{AppState, ControllerServer};
use zgalaxy_rs::peer::PeerManager;
use zgalaxy_rs::network::NetworkManager;
use zgalaxy_rs::nat::NatTraversalEngine;

// ============================================================================
// 1. Cryptography & Identity Matrix Tests
// ============================================================================

#[test]
fn test_matrix_identity_derivation_and_pow() {
    let id = Identity::generate();
    assert_ne!(id.address, Address::NULL);
    assert!(!id.address.is_reserved(), "Address must not start with 0xff");

    // Public and secret string serialization and round-trip parsing
    let pub_str = id.to_public_string();
    let parsed_pub = Identity::parse(&pub_str).expect("Failed to parse public identity");
    assert_eq!(parsed_pub.address, id.address);
    assert!(parsed_pub.signing_key.is_none());

    let sec_str = id.to_secret_string().expect("Failed to serialize secret identity");
    let parsed_sec = Identity::parse(&sec_str).expect("Failed to parse secret identity");
    assert_eq!(parsed_sec.address, id.address);
    assert!(parsed_sec.signing_key.is_some());

    // Signature creation and verification
    let payload = b"ZGALAXY Sovereign Network Mesh Authentication 2026";
    let sig = id.sign(payload).expect("Failed to sign payload");
    assert!(parsed_pub.verify(payload, &sig), "Signature verification must succeed");
    assert!(!parsed_pub.verify(b"Corrupted Payload", &sig), "Corrupted payload signature must fail");
}

#[test]
fn test_matrix_identity_invalid_and_edge_cases() {
    assert!(Identity::parse("").is_err(), "Empty identity string must fail");
    assert!(Identity::parse("invalid:format").is_err(), "Malformed identity string must fail");
    assert!(Identity::parse("0000000000:0:invalidhex").is_err(), "Invalid hex public key must fail");
    assert!(Identity::parse("0000000000:0:0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20").is_err(), 
        "Mismatched address and public key must fail");
}

// ============================================================================
// 2. Wire Protocol & Packet Framing Matrix Tests
// ============================================================================

#[test]
fn test_matrix_wire_packet_all_verbs_round_trip() {
    let src = Address([1, 2, 3, 4, 5]);
    let dst = Address([5, 4, 3, 2, 1]);
    let payload = Bytes::from_static(b"Realtime Mesh Layer-2 Payload Data");

    let all_types = [
        PacketType::Nop,
        PacketType::Hello,
        PacketType::Error,
        PacketType::Ok,
        PacketType::Whois,
        PacketType::Rendezvous,
        PacketType::Frame,
        PacketType::ExtFrame,
        PacketType::Echo,
        PacketType::MulticastLike,
        PacketType::NetworkCredentials,
        PacketType::NetworkConfigRequest,
        PacketType::NetworkConfig,
        PacketType::MulticastGather,
        PacketType::MulticastFrame,
        PacketType::Pong,
        PacketType::PushDirectPaths,
    ];

    for ptype in all_types {
        let pkt = Packet::new(dst, src, 0x123456789abcdef0, ptype, payload.clone());
        let encoded = pkt.encode();
        assert_eq!(encoded.len(), PACKET_HEADER_SIZE + payload.len());

        let decoded = Packet::decode(encoded).expect("Failed to decode canonical packet");
        assert_eq!(decoded.dest, dst);
        assert_eq!(decoded.source, src);
        assert_eq!(decoded.packet_id, 0x123456789abcdef0);
        assert_eq!(decoded.packet_type, ptype);
        assert_eq!(decoded.payload, payload);
    }
}

#[test]
fn test_matrix_wire_packet_error_handling() {
    // Truncated packet
    let truncated = Bytes::from_static(b"\x00\x01\x02\x03");
    assert!(Packet::decode(truncated).is_err(), "Truncated packet must fail decoding");

    // Zero-length payload packet
    let empty_payload = Packet::new(Address::NULL, Address::NULL, 1, PacketType::Echo, Bytes::new());
    let encoded = empty_payload.encode();
    assert_eq!(encoded.len(), PACKET_HEADER_SIZE);
    let decoded = Packet::decode(encoded).expect("Empty payload packet must decode cleanly");
    assert_eq!(decoded.payload.len(), 0);
}

// ============================================================================
// 3. World & Moon Topology Matrix Tests
// ============================================================================

#[test]
fn test_matrix_world_topology_lifecycle() {
    let ep_str = "154.250.69.161:9993".to_string();
    let root = WorldRoot {
        identity: Address([0x06, 0x9a, 0xe3, 0x80, 0x92]),
        stable_endpoints: vec![ep_str.clone()],
    };

    let mut world = World::new(WORLD_TYPE_PLANET, 149604618, 1723626000, vec![root]);
    let encoded = world.encode();
    let decoded = World::parse_binary(&encoded).expect("Failed to decode binary world");
    assert_eq!(decoded.world_type, WORLD_TYPE_PLANET);
    assert_eq!(decoded.id, 149604618);
    assert_eq!(decoded.roots.len(), 1);
    assert_eq!(decoded.roots[0].stable_endpoints[0], ep_str);

    // In-place dynamic endpoint update (Zero-Restart Native DNS update)
    let new_ep_str = "198.51.100.25:9993".to_string();
    world.set_root_stable_endpoints(0, vec![new_ep_str.clone()]);
    assert_eq!(world.roots[0].stable_endpoints[0], new_ep_str);
}

// ============================================================================
// 4. Decoupled Dynamic DNS & Multi-Domain Matrix Tests
// ============================================================================

#[tokio::test]
async fn test_matrix_dynamic_dns_multi_domain_and_drift() {
    let resolver = DynamicDnsResolver::new(30);

    // Add multiple dynamic domain sources
    assert!(resolver.add_domain("127.0.0.1", 9993, Some("Primary Loopback".to_string())).await.is_ok());
    assert!(resolver.add_domain("127.0.0.1", 9994, Some("Secondary Loopback".to_string())).await.is_ok());

    let domains = resolver.list_domains().await;
    assert_eq!(domains.len(), 2);

    let addrs = resolver.get_all_active_addresses().await;
    assert!(addrs.contains(&"127.0.0.1:9993".parse().unwrap()));
    assert!(addrs.contains(&"127.0.0.1:9994".parse().unwrap()));

    // Dynamic deletion during runtime
    assert!(resolver.remove_domain("127.0.0.1", 9994).await.unwrap());
    let updated_domains = resolver.list_domains().await;
    assert_eq!(updated_domains.len(), 1);

    // Parse host port utility tests
    assert_eq!(DynamicDnsResolver::parse_host_port("dz.dreamzone.cc:9993").unwrap(), ("dz.dreamzone.cc".to_string(), 9993));
    assert_eq!(DynamicDnsResolver::parse_host_port("myplanet.org/9994").unwrap(), ("myplanet.org".to_string(), 9994));
    assert_eq!(DynamicDnsResolver::parse_host_port("solo.net").unwrap(), ("solo.net".to_string(), 9993));
}

// ============================================================================
// 5. Embedded Controller & FileDB Persistence Matrix Tests
// ============================================================================

#[tokio::test]
async fn test_matrix_embedded_controller_filedb_and_ip_allocation() {
    let temp_dir = tempdir().unwrap();
    let id = Identity::generate();
    let controller = EmbeddedController::new(id.clone(), temp_dir.path().to_path_buf());
    controller.init().await.expect("Failed to initialize controller FileDB storage");

    // 1. Create Network with IP Assignment Pool
    let create_payload = json!({
        "name": "Production Mesh Matrix",
        "private": true,
        "mtu": 2800,
        "ipAssignmentPools": [
            { "ipRangeStart": "10.147.17.10", "ipRangeEnd": "10.147.17.20" }
        ],
        "v4AssignMode": { "zt": true }
    });

    let net = controller.save_network(create_payload).await.expect("Failed to save network");
    let nwid = net.id.clone();
    assert_eq!(nwid.len(), 16);
    assert_eq!(net.name, "Production Mesh Matrix");

    // 2. Authorize multiple members and verify unique IP allocations (No conflict)
    let member1_id = "1111111111";
    let member2_id = "2222222222";
    let member3_id = "3333333333";

    let m1 = controller.save_member(&nwid, member1_id, json!({ "authorized": true })).await.unwrap();
    let m2 = controller.save_member(&nwid, member2_id, json!({ "authorized": true })).await.unwrap();
    let m3 = controller.save_member(&nwid, member3_id, json!({ "authorized": true })).await.unwrap();

    assert_eq!(m1.ip_assignments, vec!["10.147.17.10"]);
    assert_eq!(m2.ip_assignments, vec!["10.147.17.11"]);
    assert_eq!(m3.ip_assignments, vec!["10.147.17.12"]);

    // 3. Restart Controller and verify full state reload from disk (FileDB reload test)
    let restarted_controller = EmbeddedController::new(id.clone(), temp_dir.path().to_path_buf());
    restarted_controller.init().await.expect("Failed to reload controller from disk");

    let restored_net = restarted_controller.get_network(&nwid).await.expect("Network must persist across restart");
    assert_eq!(restored_net.name, "Production Mesh Matrix");

    let members = restarted_controller.list_members(&nwid).await;
    assert_eq!(members.len(), 3);
    assert!(members.contains_key(member1_id));
    assert!(members.contains_key(member2_id));
    assert!(members.contains_key(member3_id));
}

// ============================================================================
// 6. Local REST API (Port 9993) & ZTNET Contract Compatibility Matrix Tests
// ============================================================================

#[tokio::test]
async fn test_matrix_rest_api_auth_and_contract_invariance() {
    let temp_dir = tempdir().unwrap();
    let id = Identity::generate();
    let auth_token = "secret_test_token_2026".to_string();

    let controller = EmbeddedController::new(id.clone(), temp_dir.path().to_path_buf());
    controller.init().await.unwrap();

    let peer_manager = PeerManager::new();
    let network_manager = NetworkManager::new();
    let resolver = Arc::new(DynamicDnsResolver::new(30));

    let state = AppState {
        identity: id.clone(),
        allow_management_from: vec!["127.0.0.1".to_string()],
        auth_token: auth_token.clone(),
        peer_manager,
        network_manager,
        controller,
        resolver,
    };

    let router = ControllerServer::build_router(state);

    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // Test 1: /status without auth -> 401 Unauthorized
    let req_unauth = Request::builder()
        .uri("/status")
        .body(axum::body::Body::empty())
        .unwrap();
    let res_unauth = router.clone().oneshot(req_unauth).await.unwrap();
    assert_eq!(res_unauth.status(), StatusCode::UNAUTHORIZED);

    // Test 2: /status with X-ZT1-Auth -> 200 OK with proper schema
    let req_auth = Request::builder()
        .uri("/status")
        .header("X-ZT1-Auth", &auth_token)
        .body(axum::body::Body::empty())
        .unwrap();
    let res_auth = router.clone().oneshot(req_auth).await.unwrap();
    assert_eq!(res_auth.status(), StatusCode::OK);

    // Test 3: /controller status
    let req_ctrl = Request::builder()
        .uri("/controller")
        .header("X-ZT1-Auth", &auth_token)
        .body(axum::body::Body::empty())
        .unwrap();
    let res_ctrl = router.clone().oneshot(req_ctrl).await.unwrap();
    assert_eq!(res_ctrl.status(), StatusCode::OK);

    // Test 4: /metrics (Prometheus) -> authenticated only (audit fix)
    let req_metrics = Request::builder()
        .uri("/metrics")
        .body(axum::body::Body::empty())
        .unwrap();
    let res_metrics = router.clone().oneshot(req_metrics).await.unwrap();
    assert_eq!(res_metrics.status(), StatusCode::UNAUTHORIZED);
    let req_metrics_auth = Request::builder()
        .uri("/metrics")
        .header("X-ZT1-Auth", &auth_token)
        .body(axum::body::Body::empty())
        .unwrap();
    let res_metrics_auth = router.clone().oneshot(req_metrics_auth).await.unwrap();
    assert_eq!(res_metrics_auth.status(), StatusCode::OK);
}

// ============================================================================
// 7. NAT Candidate & Traversal Engine Matrix Tests
// ============================================================================

#[tokio::test]
async fn test_matrix_nat_path_registration_and_ranking() {
    let peer_mgr = PeerManager::new();
    let nat_engine = NatTraversalEngine::new(peer_mgr.clone());
    let node_addr = Address([0x12, 0x34, 0x56, 0x78, 0x9a]);

    // Register Relay Path
    let relay_ep: SocketAddr = "203.0.113.10:9993".parse().unwrap();
    nat_engine.register_path_candidate(node_addr, relay_ep, false, 80).await;

    let peer = peer_mgr.get_peer(&node_addr).await.expect("Peer must exist");
    assert_eq!(peer.paths.len(), 1);

    // Register Direct Lower-Latency Path
    let direct_ep: SocketAddr = "198.51.100.25:9993".parse().unwrap();
    nat_engine.register_path_candidate(node_addr, direct_ep, true, 15).await;

    let updated_peer = peer_mgr.get_peer(&node_addr).await.expect("Peer must exist");
    assert_eq!(updated_peer.paths.len(), 2);
    assert_eq!(updated_peer.latency_ms, 15);
}

// ============================================================================
// 8. Join-Request Guard Matrix (audit 2026-08-22)
// ============================================================================

#[tokio::test]
async fn test_matrix_join_request_guards() {
    let temp_dir = tempdir().unwrap();
    let id = Identity::generate();
    let controller = EmbeddedController::new(id.clone(), temp_dir.path().to_path_buf());
    controller.init().await.unwrap();

    // A valid network must exist first.
    let net = controller
        .save_network(serde_json::json!({ "name": "guard-net" }))
        .await
        .unwrap();
    let nwid = net.id.clone();

    // 1) Non-hex / wrong-length member ids must be rejected.
    for bad in ["192.168.1.5", "abcdefghij!", "12345"] {
        let err = controller.register_join_request(&nwid, bad, None).await;
        assert!(err.is_err(), "member id {:?} must be rejected", bad);
    }

    // 2) Joins for networks the controller does not own must be rejected —
    //    unauthenticated peers must not be able to mint orphan member files.
    let err = controller
        .register_join_request(&format!("{}000001", "ffffffffff"), "0123456789", None)
        .await;
    assert!(err.is_err(), "join to non-existent network must be rejected");

    // 3) A valid join still succeeds and creates the member record.
    let rec = controller
        .register_join_request(&nwid, "a1b2c3d4e5", None)
        .await
        .expect("valid join must succeed");
    assert_eq!(rec.id, "a1b2c3d4e5");

    // 4) No member files were created for rejected networks.
    let orphan_dir = temp_dir.path().join("network").join("ffffffffff000001");
    assert!(!orphan_dir.exists(), "rejected network must not create files");
}

// ============================================================================
// 9. Membership Token Matrix (Gate A1 — COM replacement)
// ============================================================================

#[tokio::test]
async fn test_matrix_membership_token_lifecycle() {
    let temp_dir = tempdir().unwrap();
    let id = Identity::generate();
    let controller = EmbeddedController::new(id.clone(), temp_dir.path().to_path_buf());
    controller.init().await.unwrap();

    // Controller must be able to sign (secret identity present).
    let net = controller
        .save_network(serde_json::json!({ "name": "token-net", "private": true }))
        .await
        .unwrap();
    let nwid = net.id.clone();

    // Unauthorized member: no token issued.
    controller
        .register_join_request(&nwid, "a1b2c3d4e5", None)
        .await
        .unwrap();
    assert!(
        controller.issue_membership_token(&nwid, "a1b2c3d4e5").await.is_none(),
        "no token for unauthorized member"
    );

    // Authorize → token issued and verifies for the right member.
    controller
        .save_member(
            &nwid,
            "a1b2c3d4e5",
            serde_json::json!({ "authorized": true }),
        )
        .await
        .unwrap();
    let token = controller
        .issue_membership_token(&nwid, "a1b2c3d4e5")
        .await
        .expect("token for authorized member");
    assert!(
        controller
            .verify_membership_token(&token, &nwid, "a1b2c3d4e5")
            .is_some(),
        "valid token must verify"
    );

    // Wrong member / wrong network must fail.
    assert!(controller.verify_membership_token(&token, &nwid, "ffffffffee").is_none());
    assert!(
        controller
            .verify_membership_token(&token, &format!("{}ffff", &nwid[..12]), "a1b2c3d4e5")
            .is_none()
    );

    // Tampered payload must fail signature verification.
    let mut parts = token.split('.');
    let body = parts.next().unwrap().to_string();
    let sig = parts.next().unwrap().to_string();
    let tampered = format!("e30.{}", sig); // {} payload with real signature
    assert!(controller.verify_membership_token(&tampered, &nwid, "a1b2c3d4e5").is_none());
    let _ = body;

    // Deauthorize → issuance stops immediately (revocation within TTL).
    controller
        .save_member(&nwid, "a1b2c3d4e5", serde_json::json!({ "authorized": false }))
        .await
        .unwrap();
    assert!(controller.issue_membership_token(&nwid, "a1b2c3d4e5").await.is_none());
}
