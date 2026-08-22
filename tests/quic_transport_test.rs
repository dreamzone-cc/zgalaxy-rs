//! Integration tests for the QUIC transport layer.
//!
//! Two local endpoints establish a real QUIC (TLS 1.3) session and exchange:
//! - a reliable control message over a bi-stream (NodeAnnounce),
//! - an unreliable data frame over a QUIC Datagram.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use zgalaxy_rs::quic::{control::ControlMessage, QuicEvent, QuicTransport};

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

#[tokio::test]
async fn quic_handshake_control_and_datagram() {
    // Node A: server side.
    let a = Arc::new(
        QuicTransport::bind("127.0.0.1:0".parse().unwrap(), "aaaa000001".into(), "aaaa000001:0:aa".into()).unwrap(),
    );
    let a_addr: SocketAddr = a.local_addr().unwrap();

    // Node B: client side (separate endpoint so it can also accept streams).
    let b = std::sync::Arc::new(
        QuicTransport::bind("127.0.0.1:0".parse().unwrap(), "bbbb000002".into(), "bbbb000002:0:bb".into()).unwrap(),
    );

    let (tx_a, mut rx_a) = tokio::sync::mpsc::channel(64);
    let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(64);

    let a_runner = Arc::clone(&a);
    tokio::spawn(async move { a_runner.run(tx_a).await });
    // Node B also runs its accept loop so A's announce stream reaches it.
    let b_runner = Arc::clone(&b);
    tokio::spawn(async move { b_runner.run(tx_b).await });

    // B connects to A (retry briefly — B's accept loop must install the
    // event sink before outbound connections are allowed).
    let mut connected = false;
    for _ in 0..50 {
        match b.connect(a_addr).await {
            Ok(_) => {
                connected = true;
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(connected, "B should connect to A");

    // --- Reliable control path: B sends a NetworkConfigRequest on a stream.
    b.send_control(
        a_addr,
        &ControlMessage::NetworkConfigRequest { nwid: "0123456789abcdef".into(), token: None },
    )
    .await
    .expect("control send failed");

    let ev = recv_until(&mut rx_a, |e| {
        matches!(e, QuicEvent::Control { message: ControlMessage::NetworkConfigRequest { .. }, .. })
    })
    .await;
    match ev {
        QuicEvent::Control { remote, message } => {
            assert_eq!(remote.ip(), a_addr.ip());
            match message {
                ControlMessage::NetworkConfigRequest { nwid, .. } => {
                    assert_eq!(nwid, "0123456789abcdef");
                }
                other => panic!("unexpected control message: {:?}", other),
            }
        }
        other => panic!("unexpected event: {:?}", other),
    }

    // --- Unreliable data path: B sends an Ethernet-sized frame as a datagram.
    let frame: Bytes = Bytes::from(vec![0xFFu8; 64]);
    b.send_frame(a_addr, frame.clone()).await.expect("datagram send failed");

    let ev = recv_until(&mut rx_a, |e| matches!(e, QuicEvent::Datagram { .. })).await;
    match ev {
        QuicEvent::Datagram { data, .. } => assert_eq!(data, frame),
        other => panic!("unexpected event: {:?}", other),
    }

    // --- Node announce: A's accept loop should have received B's announce
    // (delivered when the connection was established).
    // B receives A's announce over its own accept loop.
    let ev = recv_until(&mut rx_b, |e| {
        matches!(e, QuicEvent::Control { message: ControlMessage::NodeAnnounce { .. }, .. })
    })
    .await;
    match ev {
        QuicEvent::Control { message: ControlMessage::NodeAnnounce { address, .. }, .. } => {
            assert_eq!(address, "aaaa000001");
        }
        other => panic!("unexpected event: {:?}", other),
    }

    // --- Oversized datagrams must be rejected, not silently truncated.
    let big = Bytes::from(vec![0u8; zgalaxy_rs::quic::MAX_DATAGRAM_SIZE + 1]);
    assert!(b.send_frame(a_addr, big).await.is_err());
}
