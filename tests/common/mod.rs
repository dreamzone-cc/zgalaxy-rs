//! Shared end-to-end scenario for the mesh engine, run over every mesh
//! transport: a controller node and a client node run the full daemon
//! wiring (ControlEngine + config sync loop + outbound forwarder)
//! in-process over real loopback sessions.
//!
//! Covers: identity proof → join → pending → authorize → address assigned →
//! frames flow (including a full 2800-MTU frame); a proven peer that is NOT
//! the network's controller cannot push a config; an unproven session cannot
//! inject frames; disconnect sticks.
//!
//! Each transport runs it from its own test binary (one test per binary: it
//! sets ZGALAXY_EXTRA_ENDPOINTS, which is process-global).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use zgalaxy_rs::control::{ControlMessage, SignedConfig};
use zgalaxy_rs::control_engine::{self, ControlEngine, MeshContext};
use zgalaxy_rs::controller::EmbeddedController;
use zgalaxy_rs::identity::Identity;
use zgalaxy_rs::l2_switch::L2Switch;
use zgalaxy_rs::mesh::{MeshTransport, PeerSessions, TransportEvent, TransportKind};
use zgalaxy_rs::network::{NetworkManager, NetworkStatus};
use zgalaxy_rs::peer::PeerManager;
use zgalaxy_rs::quic::{QuicMesh, QuicTransport};
#[cfg(feature = "gtp")]
use zgalaxy_rs::gtp_transport::{GtpMesh, GtpTransport};
use zgalaxy_rs::resolver::DynamicDnsResolver;

struct Node {
    addr: SocketAddr,
    ctx: MeshContext,
    tun_rx: mpsc::Receiver<Bytes>,
    tun_out: mpsc::Sender<Bytes>,
    _dir: tempfile::TempDir,
}

/// Bind a loopback mesh transport of `kind` and start its accept loop.
pub async fn bind(kind: TransportKind) -> (Arc<dyn MeshTransport>, mpsc::Receiver<TransportEvent>) {
    let (events_tx, events_rx) = mpsc::channel(256);
    let transport: Arc<dyn MeshTransport> = match kind {
        TransportKind::Quic => {
            let quic = Arc::new(QuicTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
            let runner = Arc::clone(&quic);
            tokio::spawn(async move { runner.run(events_tx).await });
            Arc::new(QuicMesh(quic))
        }
        #[cfg(feature = "gtp")]
        TransportKind::Gtp => {
            let gtp = Arc::new(GtpTransport::bind("127.0.0.1:0".parse().unwrap()).await.unwrap());
            let runner = Arc::clone(&gtp);
            tokio::spawn(async move { runner.run(events_tx).await });
            Arc::new(GtpMesh(gtp))
        }
        other => panic!("no mesh backend for {} in this build", other),
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    (transport, events_rx)
}

async fn spawn_node(kind: TransportKind, is_controller: bool, sync: bool) -> Node {
    spawn_node_as(kind, Identity::generate(), is_controller, sync).await
}

async fn spawn_node_as(kind: TransportKind, identity: Identity, is_controller: bool, sync: bool) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let (transport, events_rx) = bind(kind).await;
    let addr = transport.local_addr().unwrap();

    let controller = EmbeddedController::new(identity.clone(), dir.path().to_path_buf());
    if is_controller {
        controller.init().await.unwrap();
    }
    let networks = NetworkManager::new();
    networks.set_node_address(identity.address.to_string()).await;
    networks.set_working_dir(dir.path().to_path_buf()).await;

    let (tun_tx, tun_rx) = mpsc::channel(64);
    let (tun_out, tun_out_rx) = mpsc::channel(64);
    let ctx = MeshContext {
        transport,
        sessions: PeerSessions::new(),
        identity,
        controller,
        is_controller,
        networks,
        peers: PeerManager::new(),
        l2: L2Switch::new(),
        tun_tx,
        interface: None,
        sync_now: Arc::new(tokio::sync::Notify::new()),
    };

    tokio::spawn(ControlEngine::new(ctx.clone()).run(events_rx));
    tokio::spawn(control_engine::forward_outbound(ctx.clone(), tun_out_rx));
    if sync {
        tokio::spawn(control_engine::config_sync_loop(ctx.clone(), Arc::new(DynamicDnsResolver::new(30))));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    Node { addr, ctx, tun_rx, tun_out, _dir: dir }
}

async fn wait_for<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {}", what);
}

pub fn ethernet(dst: [u8; 6], src: [u8; 6], len: usize) -> Bytes {
    let mut f = Vec::with_capacity(len);
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&[0x08, 0x00]);
    f.resize(len, 0x5a);
    Bytes::from(f)
}

/// The whole scenario, over the transport `kind`.
pub async fn full_cycle(kind: TransportKind) {
    let mut ctrl = spawn_node(kind, true, false).await;
    let net = ctrl
        .ctx
        .controller
        .save_network(json!({
            "name": "Arena",
            "private": true,
            "routes": [{"target": "10.77.0.0/16", "via": null}],
            "ipAssignmentPools": [{"ipRangeStart": "10.77.0.10", "ipRangeEnd": "10.77.0.20"}],
            "v4AssignMode": {"zt": true}
        }))
        .await
        .unwrap();
    let nwid = net.id.clone();
    assert_eq!(&nwid[..10], ctrl.ctx.identity.address.to_string(), "nwid carries the controller address");

    std::env::set_var("ZGALAXY_EXTRA_ENDPOINTS", ctrl.addr.to_string());
    let mut client = spawn_node(kind, false, true).await;
    let client_id = client.ctx.identity.address.to_string();
    client.ctx.networks.join(&nwid).await.unwrap();
    client.ctx.sync_now.notify_one();

    // 1. Identity proof + join request → pending member, ACCESS_DENIED.
    let (c, id) = (ctrl.ctx.controller.clone(), client_id.clone());
    let n = nwid.clone();
    wait_for("member registered at the controller", || {
        let (c, id, n) = (c.clone(), id.clone(), n.clone());
        async move { c.get_member(&n, &id).await.is_some() }
    })
    .await;
    assert!(!ctrl.ctx.controller.get_member(&nwid, &client_id).await.unwrap().authorized);
    let nm = client.ctx.networks.clone();
    let n = nwid.clone();
    wait_for("client sees ACCESS_DENIED", || {
        let (nm, n) = (nm.clone(), n.clone());
        async move { nm.get(&n).await.map(|x| x.status == NetworkStatus::AccessDenied).unwrap_or(false) }
    })
    .await;

    // A genuine controller signature captured NOW is older than every
    // config the client applies later: replaying it in step 4c must fail.
    let stale = SignedConfig::sign(
        &ctrl.ctx.identity,
        &nwid,
        &client_id,
        &json!({"nwid": nwid, "authorized": true, "ipAssignments": ["10.77.0.99"], "routes": []}),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;

    // 2. Authorize (what ztnet does over REST) → OK with a pool address.
    ctrl.ctx
        .controller
        .save_member(&nwid, &client_id, json!({"authorized": true}))
        .await
        .unwrap();
    let nm = client.ctx.networks.clone();
    let n = nwid.clone();
    wait_for("client authorized with an address", || {
        let (nm, n) = (nm.clone(), n.clone());
        async move {
            nm.get(&n)
                .await
                .map(|x| x.status == NetworkStatus::Ok && x.assigned_addresses == vec!["10.77.0.10".to_string()])
                .unwrap_or(false)
        }
    })
    .await;
    let joined = client.ctx.networks.get(&nwid).await.unwrap();
    assert_eq!(joined.name, "Arena");
    assert!(joined.membership_token.is_some(), "authorized member holds a signed token");

    // 3. Data plane: a frame written to the client's TAP reaches the
    // controller's TAP (broadcast → flood), and the reply is unicast back
    // through the learned MAC. The reply is 2814 bytes: a full 2800-MTU frame.
    let client_mac = [0x02, 0, 0, 0, 0, 0x01];
    let ctrl_mac = [0x02, 0, 0, 0, 0, 0x02];
    client.tun_out.send(ethernet([0xff; 6], client_mac, 60)).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), ctrl.tun_rx.recv()).await.unwrap().unwrap();
    assert_eq!(&got[6..12], &client_mac);
    ctrl.tun_out.send(ethernet(client_mac, ctrl_mac, 2814)).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), client.tun_rx.recv()).await.unwrap().unwrap();
    assert_eq!(got.len(), 2814);
    assert_eq!(&got[0..6], &client_mac);

    // 4. A proven peer that is NOT the network's controller cannot push a config.
    let rogue = spawn_node(kind, false, false).await;
    rogue.ctx.transport.connect(client.addr).await.unwrap();
    assert!(
        rogue.ctx.sessions.await_accepted_us(client.addr, Duration::from_secs(5)).await,
        "rogue completes the identity proof"
    );
    rogue
        .ctx
        .transport
        .send_control(
            client.addr,
            &ControlMessage::NetworkConfigResponse {
                nwid: nwid.clone(),
                config: json!({"authorized": true, "ipAssignments": ["6.6.6.6"], "routes": [{"target": "0.0.0.0/0", "via": null}]}),
                signed: None,
            },
        )
        .await
        .unwrap();
    // Poll densely: the real controller re-sends its config every few
    // seconds, so a single late check could miss a briefly applied rogue one.
    for _ in 0..50 {
        let now = client.ctx.networks.get(&nwid).await.unwrap();
        assert_eq!(now.assigned_addresses, vec!["10.77.0.10".to_string()], "rogue config must be ignored");
        assert!(now.routes.iter().all(|r| r.target != "0.0.0.0/0"), "rogue default route must be ignored");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 4b. An impostor claiming the CONTROLLER's address with its own key
    // (64-byte public key blob: the stated address used to be trusted
    // as-is) must not pass the identity proof, so it cannot push a config.
    let attacker = Identity::generate();
    let mut blob = vec![0u8; 32];
    blob.extend_from_slice(attacker.verifying_key.as_bytes());
    let impostor_id = Identity {
        address: ctrl.ctx.identity.address,
        verifying_key: attacker.verifying_key,
        signing_key: attacker.signing_key.clone(),
        raw_public_key: blob,
    };
    let impostor = spawn_node_as(kind, impostor_id, false, false).await;
    impostor.ctx.transport.connect(client.addr).await.unwrap();
    assert!(
        !impostor.ctx.sessions.await_accepted_us(client.addr, Duration::from_secs(2)).await,
        "impostor passed the identity proof as the controller"
    );
    impostor
        .ctx
        .transport
        .send_control(
            client.addr,
            &ControlMessage::NetworkConfigResponse {
                nwid: nwid.clone(),
                config: json!({"authorized": true, "ipAssignments": ["6.6.6.7"], "routes": []}),
                signed: None,
            },
        )
        .await
        .unwrap();
    for _ in 0..50 {
        let now = client.ctx.networks.get(&nwid).await.unwrap();
        assert_eq!(now.assigned_addresses, vec!["10.77.0.10".to_string()], "impostor config must be ignored");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 4c. On-path tampering: messages over the REAL controller's proven
    // session (what a relay forwarding its أ3 proof could inject). Only a
    // fresh controller signature for this member counts.
    // Stamped a day ahead: the replay guard cannot be what rejects these,
    // only the signature/addressing checks can. (A rejected config never
    // advances the guard, so the real controller's configs keep flowing.)
    let future = zgalaxy_rs::control::unix_millis() + 86_400_000;
    let forged = json!({"nwid": nwid, "authorized": true, "ipAssignments": ["6.6.6.8"], "routes": []});
    let genuine = SignedConfig::sign_at(&ctrl.ctx.identity, &nwid, &client_id, &forged, future).unwrap();
    let attacks = [
        ("unsigned", None),
        ("edited body", Some(SignedConfig { body: genuine.body.replace("6.6.6.8", "6.6.6.9"), ..genuine.clone() })),
        ("replayed stale", Some(stale)),
        (
            "issued to another member",
            Some(SignedConfig::sign_at(&ctrl.ctx.identity, &nwid, "0123456789", &forged, future).unwrap()),
        ),
        (
            "signed by another key",
            Some(SignedConfig::sign_at(&Identity::generate(), &nwid, &client_id, &forged, future).unwrap()),
        ),
    ];
    for (what, signed) in attacks {
        ctrl.ctx
            .transport
            .send_control(
                client.addr,
                &ControlMessage::NetworkConfigResponse { nwid: nwid.clone(), config: forged.clone(), signed },
            )
            .await
            .unwrap();
        for _ in 0..20 {
            let now = client.ctx.networks.get(&nwid).await.unwrap();
            assert_eq!(now.assigned_addresses, vec!["10.77.0.10".to_string()], "{} config was applied", what);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    // Control: the same path DOES apply a fresh genuine signature, so the
    // rejections above are the checks working, not a dead channel.
    let renamed = json!({"nwid": nwid, "name": "Signed", "authorized": true, "ipAssignments": ["10.77.0.10"], "routes": [{"target": "10.77.0.0/16", "via": null}]});
    let fresh = SignedConfig::sign(&ctrl.ctx.identity, &nwid, &client_id, &renamed).unwrap();
    ctrl.ctx
        .transport
        .send_control(
            client.addr,
            &ControlMessage::NetworkConfigResponse { nwid: nwid.clone(), config: renamed.clone(), signed: Some(fresh) },
        )
        .await
        .unwrap();
    let nm = client.ctx.networks.clone();
    let n = nwid.clone();
    wait_for("a genuine signed config applies", || {
        let (nm, n) = (nm.clone(), n.clone());
        async move { nm.get(&n).await.map(|x| x.name == "Signed").unwrap_or(false) }
    })
    .await;

    // 5. An anonymous session (no identity proof) cannot inject frames.
    let (anon, _anon_rx) = bind(kind).await;
    anon.connect(ctrl.addr).await.unwrap();
    anon.send_frame(ctrl.addr, ethernet([0xff; 6], [0x02, 0, 0, 0, 0, 0x66], 60)).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), ctrl.tun_rx.recv()).await.is_err(),
        "a frame from an unproven session reached the TAP"
    );

    // 6. Disconnect wins over late responses: the sync loop stops asking and
    // nothing flips the network back on.
    client.ctx.networks.disconnect(&nwid).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let after = client.ctx.networks.get(&nwid).await.unwrap();
    assert!(!after.connected);
    assert_eq!(after.status, NetworkStatus::Disconnected);
}
