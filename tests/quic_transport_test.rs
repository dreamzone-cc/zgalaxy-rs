//! Integration tests for the QUIC transport layer.
//!
//! Two local endpoints establish a real QUIC (TLS 1.3) session and exchange:
//! - a reliable control message over a bi-stream,
//! - a small data frame over a QUIC Datagram,
//! - a 2800-MTU frame over the uni-stream fallback (too big for a datagram).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use zgalaxy_rs::mesh::MeshTransport;
use zgalaxy_rs::quic::{control::ControlMessage, QuicEvent, QuicMesh, QuicTransport, MAX_FRAME_SIZE};

async fn recv_until(
    rx: &mut tokio::sync::mpsc::Receiver<QuicEvent>,
    mut pred: impl FnMut(&QuicEvent) -> bool,
) -> QuicEvent {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for QUIC event")
            .expect("event channel closed");
        if pred(&ev) {
            return ev;
        }
    }
}

async fn node() -> (Arc<QuicTransport>, tokio::sync::mpsc::Receiver<QuicEvent>) {
    let t = Arc::new(QuicTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let runner = Arc::clone(&t);
    tokio::spawn(async move { runner.run(tx).await });
    // run() installs the event sink asynchronously.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (t, rx)
}

#[tokio::test]
async fn quic_handshake_control_and_frames() {
    let (a, mut rx_a) = node().await;
    let (b, mut rx_b) = node().await;
    let a_addr: SocketAddr = a.local_addr().unwrap();

    // Sending a frame never dials: without a session it fails fast.
    assert!(b.send_frame(a_addr, Bytes::from_static(b"x")).await.is_err());

    b.connect(a_addr).await.expect("B should connect to A");

    // Both sides see Connected before any traffic.
    recv_until(&mut rx_b, |e| matches!(e, QuicEvent::Connected { .. })).await;
    recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Connected { .. })).await;

    // --- Reliable control path.
    b.send_control(a_addr, &ControlMessage::NetworkConfigRequest { nwid: "0123456789abcdef".into(), token: None })
        .await
        .expect("control send failed");
    let ev = recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Control { .. })).await;
    match ev {
        QuicEvent::Control { remote, message: ControlMessage::NetworkConfigRequest { nwid, .. } } => {
            assert_eq!(remote.ip(), a_addr.ip());
            assert_eq!(nwid, "0123456789abcdef");
        }
        other => panic!("unexpected event: {:?}", other),
    }

    // --- Small frame: one QUIC datagram.
    let small = Bytes::from(vec![0xFFu8; 64]);
    b.send_frame(a_addr, small.clone()).await.expect("datagram send failed");
    let ev = recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Frame { .. })).await;
    match ev {
        QuicEvent::Frame { data, .. } => assert_eq!(data, small),
        other => panic!("unexpected event: {:?}", other),
    }

    // --- Full 2800-MTU Ethernet frame: too big for a datagram, must arrive
    // intact over the stream fallback instead of being dropped.
    let big = Bytes::from((0..2814u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
    b.send_frame(a_addr, big.clone()).await.expect("large frame send failed");
    let ev = recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Frame { data, .. } if data.len() == big.len())).await;
    match ev {
        QuicEvent::Frame { data, .. } => assert_eq!(data, big),
        other => panic!("unexpected event: {:?}", other),
    }

    // --- Beyond the mesh frame limit: rejected, not truncated.
    let huge = Bytes::from(vec![0u8; MAX_FRAME_SIZE + 1]);
    assert!(b.send_frame(a_addr, huge).await.is_err());
}

#[tokio::test]
async fn mesh_adapter_reports_sessions_and_close() {
    let (a, _rx_a) = node().await;
    let (b, mut rx_b) = node().await;
    let a_addr = a.local_addr().unwrap();
    let mesh_b = QuicMesh(Arc::clone(&b));

    assert!(mesh_b.capabilities().interface_mtu() >= 1280, "IPv6 needs ≥ 1280");
    assert!(mesh_b.active_generation(a_addr).await.is_none());

    mesh_b.connect(a_addr).await.unwrap();
    let generation = match recv_until(&mut rx_b, |e| matches!(e, QuicEvent::Connected { .. })).await {
        QuicEvent::Connected { generation, .. } => generation,
        _ => unreachable!(),
    };
    assert_eq!(mesh_b.active_generation(a_addr).await, Some(generation));
    assert_eq!(mesh_b.connected_peers().await, vec![a_addr]);

    mesh_b.close(a_addr).await;
    assert!(mesh_b.connected_peers().await.is_empty());
    let ev = recv_until(&mut rx_b, |e| matches!(e, QuicEvent::Disconnected { .. })).await;
    match ev {
        QuicEvent::Disconnected { generation: g, .. } => assert_eq!(g, generation),
        _ => unreachable!(),
    }
}

/// Regression: on a dual-stack `[::]` socket an IPv4 peer is reported as
/// `::ffff:a.b.c.d` but dialed as `a.b.c.d`. Session keys must agree, or
/// every dial misses the live session and replaces it (reconnect churn).
#[tokio::test]
async fn dual_stack_dials_reuse_the_session() {
    let Ok(a) = QuicTransport::bind("[::]:0".parse().unwrap()) else {
        eprintln!("skipping: no IPv6 dual-stack socket available");
        return;
    };
    let a = Arc::new(a);
    let (tx_a, _rx_a) = tokio::sync::mpsc::channel(64);
    let ra = Arc::clone(&a);
    tokio::spawn(async move { ra.run(tx_a).await });
    let b = Arc::new(QuicTransport::bind("[::]:0".parse().unwrap()).unwrap());
    let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(64);
    let rb = Arc::clone(&b);
    tokio::spawn(async move { rb.run(tx_b).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let target: SocketAddr = format!("127.0.0.1:{}", a.local_addr().unwrap().port()).parse().unwrap();
    let mesh_b = QuicMesh(Arc::clone(&b));
    mesh_b.connect(target).await.expect("first dial");
    let first = mesh_b.active_generation(target).await.expect("session keyed by the dialed address");
    for _ in 0..3 {
        mesh_b.connect(target).await.expect("re-dial");
    }
    assert_eq!(mesh_b.active_generation(target).await, Some(first), "re-dial must reuse the session");
    assert_eq!(mesh_b.connected_peers().await, vec![target], "events/keys use the canonical IPv4 form");
    match recv_until(&mut rx_b, |e| matches!(e, QuicEvent::Connected { .. })).await {
        QuicEvent::Connected { remote, .. } => assert_eq!(remote, target),
        _ => unreachable!(),
    }
}

/// Shutdown: `close_all` must reach the peer at once, not after its 15 s
/// idle timeout (that is what made clients recover slowly from a
/// controller restart).
#[tokio::test]
async fn close_all_reaches_the_peer_immediately() {
    let (a, mut rx_a) = node().await;
    let (b, _rx_b) = node().await;
    let a_addr = a.local_addr().unwrap();
    b.connect(a_addr).await.unwrap();
    recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Connected { .. })).await;

    let t = std::time::Instant::now();
    b.close_all().await;
    assert!(QuicMesh(Arc::clone(&b)).connected_peers().await.is_empty());
    recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Disconnected { .. })).await;
    assert!(t.elapsed() < Duration::from_secs(2), "peer saw the shutdown after {:?}", t.elapsed());
}
