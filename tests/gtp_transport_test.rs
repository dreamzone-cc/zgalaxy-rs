//! Integration tests for the GTP transport (feature `gtp`): the same
//! commitments the QUIC tests pin down (docs/GTP-INTEGRATION.md §3),
//! over real loopback GTP sessions.

#![cfg(feature = "gtp")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use zgalaxy_rs::control::ControlMessage;
use zgalaxy_rs::gtp_transport::{GtpMesh, GtpTransport, MAX_FRAME_SIZE};
use zgalaxy_rs::mesh::{MeshTransport, TransportEvent, TransportKind};

async fn recv_until(rx: &mut mpsc::Receiver<TransportEvent>, mut pred: impl FnMut(&TransportEvent) -> bool) -> TransportEvent {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for GTP event")
            .expect("event channel closed");
        if pred(&ev) {
            return ev;
        }
    }
}

async fn node_on(addr: &str) -> (Arc<GtpTransport>, mpsc::Receiver<TransportEvent>) {
    let t = Arc::new(GtpTransport::bind(addr.parse().unwrap()).await.unwrap());
    let (tx, rx) = mpsc::channel(256);
    let runner = Arc::clone(&t);
    tokio::spawn(async move { runner.run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (t, rx)
}

async fn node() -> (Arc<GtpTransport>, mpsc::Receiver<TransportEvent>) {
    node_on("127.0.0.1:0").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gtp_handshake_control_and_frames() {
    let (a, mut rx_a) = node().await;
    let (b, mut rx_b) = node().await;
    let a_addr: SocketAddr = a.local_addr().unwrap();
    let b_addr: SocketAddr = b.local_addr().unwrap();

    // Sending a frame never dials: without a session it fails fast.
    assert!(b.send_frame(a_addr, Bytes::from_static(b"x")).await.is_err());

    b.connect(a_addr).await.expect("B should connect to A");
    recv_until(&mut rx_b, |e| matches!(e, TransportEvent::Connected { .. })).await;
    match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Connected { .. })).await {
        TransportEvent::Connected { remote, .. } => assert_eq!(remote, b_addr, "acceptor keys the dialer by its socket"),
        _ => unreachable!(),
    }

    // --- Control, both directions, in order.
    for i in 0..20u64 {
        b.send_control(a_addr, &ControlMessage::Ping { nonce: i, sent_ms: 0 }).await.expect("control send");
    }
    for i in 0..20u64 {
        match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Control { .. })).await {
            TransportEvent::Control { remote, message: ControlMessage::Ping { nonce, .. } } => {
                assert_eq!(remote, b_addr);
                assert_eq!(nonce, i, "control messages keep their order");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }
    a.send_control(b_addr, &ControlMessage::Pong { nonce: 7, sent_ms: 0, received_ms: 0 }).await.unwrap();
    assert!(matches!(
        recv_until(&mut rx_b, |e| matches!(e, TransportEvent::Control { .. })).await,
        TransportEvent::Control { message: ControlMessage::Pong { nonce: 7, .. }, .. }
    ));

    // --- Small frame: one Unreliable message.
    let small = Bytes::from(vec![0xFFu8; 64]);
    b.send_frame(a_addr, small.clone()).await.unwrap();
    match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Frame { .. })).await {
        TransportEvent::Frame { data, .. } => assert_eq!(data, small),
        _ => unreachable!(),
    }

    // --- Full 2800-MTU Ethernet frame: above the Unreliable ceiling, must
    // arrive intact over the ReliableUnordered fallback.
    let big = Bytes::from((0..2814u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
    b.send_frame(a_addr, big.clone()).await.unwrap();
    match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Frame { data, .. } if data.len() == big.len())).await {
        TransportEvent::Frame { data, .. } => assert_eq!(data, big),
        _ => unreachable!(),
    }
    // ...and the next small frame still goes out (the learned ceiling
    // only diverts frames above it).
    b.send_frame(a_addr, small.clone()).await.unwrap();
    recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Frame { data, .. } if data.len() == 64)).await;

    // --- Beyond the mesh frame limit: rejected, not truncated.
    assert!(b.send_frame(a_addr, Bytes::from(vec![0u8; MAX_FRAME_SIZE + 1])).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mesh_adapter_reports_sessions_and_close_reaches_the_peer() {
    let (a, mut rx_a) = node().await;
    let (b, mut rx_b) = node().await;
    let a_addr = a.local_addr().unwrap();
    let mesh_b = GtpMesh(Arc::clone(&b));

    assert_eq!(mesh_b.kind(), TransportKind::Gtp);
    assert!(mesh_b.capabilities().interface_mtu() >= 1280, "IPv6 needs ≥ 1280");
    assert!(mesh_b.active_generation(a_addr).await.is_none());

    mesh_b.connect(a_addr).await.unwrap();
    let generation = match recv_until(&mut rx_b, |e| matches!(e, TransportEvent::Connected { .. })).await {
        TransportEvent::Connected { generation, .. } => generation,
        _ => unreachable!(),
    };
    assert_eq!(mesh_b.active_generation(a_addr).await, Some(generation));
    assert_eq!(mesh_b.connected_peers().await, vec![a_addr]);
    // Idempotent: re-dialing reuses the session.
    mesh_b.connect(a_addr).await.unwrap();
    assert_eq!(mesh_b.active_generation(a_addr).await, Some(generation));
    let a_gen = match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Connected { .. })).await {
        TransportEvent::Connected { generation, .. } => generation,
        _ => unreachable!(),
    };

    mesh_b.close(a_addr).await;
    assert!(mesh_b.connected_peers().await.is_empty());
    match recv_until(&mut rx_b, |e| matches!(e, TransportEvent::Disconnected { .. })).await {
        TransportEvent::Disconnected { generation: g, .. } => assert_eq!(g, generation),
        _ => unreachable!(),
    }
    // The explicit CLOSE reaches A quickly — not after the 15 s idle timeout.
    let t = std::time::Instant::now();
    match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Disconnected { .. })).await {
        TransportEvent::Disconnected { generation: g, .. } => assert_eq!(g, a_gen),
        _ => unreachable!(),
    }
    assert!(t.elapsed() < Duration::from_secs(2), "peer saw the close after {:?}", t.elapsed());
    assert!(a.local_addr().is_ok());
}

/// Every session gets its own generation, and a closed session leaves the
/// table, so the engine can tell a late Disconnected of an old session from
/// the live one. (Two LIVE sessions from one remote cannot be produced
/// through the public API — `connect` is idempotent — so in-place
/// replacement is covered by the code path shared with QUIC, not here.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_get_fresh_generations_and_leave_on_close() {
    let (a, mut rx_a) = node().await;
    let a_addr = a.local_addr().unwrap();

    let (b1, _rx_b1) = node().await;
    let b_addr = b1.local_addr().unwrap();
    b1.connect(a_addr).await.unwrap();
    let first = match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Connected { .. })).await {
        TransportEvent::Connected { generation, .. } => generation,
        _ => unreachable!(),
    };
    b1.close_all().await;
    recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Disconnected { .. })).await;
    drop(b1);

    // B comes back (a restarted node; GTP-rs keeps the old socket bound
    // after drop, so it returns on a new port).
    let (b2, _rx_b2) = node().await;
    b2.connect(a_addr).await.unwrap();
    let second = match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Connected { .. })).await {
        TransportEvent::Connected { generation, .. } => generation,
        _ => unreachable!(),
    };
    assert_ne!(first, second, "every session gets its own generation");
    let mesh_a = GtpMesh(Arc::clone(&a));
    assert_eq!(mesh_a.active_generation(b2.local_addr().unwrap()).await, Some(second));
    assert!(mesh_a.active_generation(b_addr).await.is_none(), "closed session is gone");
}

/// Dual-stack `[::]` sockets: the acceptor sees `::ffff:a.b.c.d`; keys and
/// events must use the canonical IPv4 form (commitment 4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dual_stack_keys_are_canonical() {
    let Ok(a) = GtpTransport::bind("[::]:0".parse().unwrap()).await else {
        eprintln!("skipping: no IPv6 dual-stack socket available");
        return;
    };
    let a = Arc::new(a);
    let (tx_a, mut rx_a) = mpsc::channel(64);
    let ra = Arc::clone(&a);
    tokio::spawn(async move { ra.run(tx_a).await });
    let (b, _rx_b) = node_on("[::]:0").await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let target: SocketAddr = format!("127.0.0.1:{}", a.local_addr().unwrap().port()).parse().unwrap();
    let mesh_b = GtpMesh(Arc::clone(&b));
    mesh_b.connect(target).await.expect("first dial");
    let first = mesh_b.active_generation(target).await.expect("session keyed by the dialed address");
    for _ in 0..3 {
        mesh_b.connect(target).await.unwrap();
    }
    assert_eq!(mesh_b.active_generation(target).await, Some(first), "re-dial must reuse the session");
    assert_eq!(mesh_b.connected_peers().await, vec![target]);
    match recv_until(&mut rx_a, |e| matches!(e, TransportEvent::Connected { .. })).await {
        TransportEvent::Connected { remote, .. } => {
            assert!(remote.is_ipv4(), "acceptor key must be canonical IPv4, got {}", remote)
        }
        _ => unreachable!(),
    }
}
