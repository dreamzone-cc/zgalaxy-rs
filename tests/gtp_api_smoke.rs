//! Phase 7 (docs/GTP-INTEGRATION.md §5): smoke test of the REAL GTP-rs API
//! that `GtpMesh` will be built on. Nothing here goes through zgalaxy's
//! transport abstraction yet; the point is to pin down, before writing the
//! adapter, exactly what GTP-rs gives us:
//!
//! - bind / connect / accept on loopback with the secure (X25519) handshake,
//! - Unreliable for L2 frames and its real payload ceiling,
//! - ReliableOrdered carrying zgalaxy control messages, in order,
//! - ReliableUnordered for frames above the Unreliable ceiling (2800 MTU),
//! - both directions on one session, and one endpoint dialing AND accepting
//!   (every mesh node does both on a single port),
//! - what a peer observes when the other side closes.
//!
//! Run: `cargo test --features gtp --test gtp_api_smoke -- --nocapture`

#![cfg(feature = "gtp")]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use gtp::{AsyncGtpConnection, ConnectionId, GtpEndpoint, MessageClass, OrderedGroupId, PriorityTier, TransportError};
use zgalaxy_rs::control::{self, ControlMessage};

const CONTROL_GROUP: u16 = 1;
const STEP: Duration = Duration::from_secs(5);

async fn endpoint() -> (GtpEndpoint, SocketAddr) {
    let ep = GtpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.expect("bind");
    let addr = ep.local_addr().unwrap();
    (ep, addr)
}

/// Dial `server` from `client` and return (client side, server side).
async fn pair(client: &GtpEndpoint, server: &GtpEndpoint, server_addr: SocketAddr, cid: u64) -> (AsyncGtpConnection, AsyncGtpConnection) {
    let (dialed, accepted) = tokio::join!(
        tokio::time::timeout(STEP, client.connect(ConnectionId(cid), server_addr, true)),
        tokio::time::timeout(STEP, server.accept()),
    );
    let dialed = dialed.expect("connect timed out").expect("secure handshake failed");
    let accepted = accepted.expect("accept timed out").expect("accept channel closed");
    assert_eq!(accepted.connection_id(), ConnectionId(cid), "CID is chosen by the dialer");
    (dialed, accepted)
}

async fn recv(conn: &mut AsyncGtpConnection) -> gtp::ReceivedMessage {
    tokio::time::timeout(STEP, conn.recv()).await.expect("recv timed out").expect("connection channel closed")
}

/// Largest payload `send_unreliable` accepts (binary search on PayloadTooLarge).
async fn unreliable_ceiling(conn: &AsyncGtpConnection) -> usize {
    let (mut ok, mut too_big) = (1usize, 65_536usize);
    while too_big - ok > 1 {
        let mid = (ok + too_big) / 2;
        match conn.send_unreliable(vec![0u8; mid], PriorityTier::P2WorldState).await {
            Ok(_) => ok = mid,
            Err(TransportError::PayloadTooLarge { .. }) => too_big = mid,
            Err(e) => panic!("unexpected error probing size {}: {:?}", mid, e),
        }
    }
    ok
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gtp_api_matches_what_gtp_mesh_needs() {
    let (a, a_addr) = endpoint().await;
    let (b, _b_addr) = endpoint().await;
    let (b_to_a, mut a_from_b) = pair(&b, &a, a_addr, 0x5A47_0000_0000_0001).await;
    let mut b_to_a = b_to_a;

    // The accepted side reports the dialer's socket as its peer.
    assert_eq!(a_from_b.peer_addr().await, b.local_addr().unwrap());

    // --- L2 frame, Unreliable, dialer → acceptor.
    let small = (0..64u8).collect::<Vec<u8>>();
    b_to_a.send_unreliable(small.clone(), PriorityTier::P2WorldState).await.unwrap();
    let m = recv(&mut a_from_b).await;
    assert!(matches!(m.class, MessageClass::Unreliable));
    assert_eq!(m.payload, small);

    // --- And acceptor → dialer on the same session.
    a_from_b.send_unreliable(b"pong".to_vec(), PriorityTier::P2WorldState).await.unwrap();
    assert_eq!(recv(&mut b_to_a).await.payload, b"pong");

    // --- Unreliable ceiling: read from the connection, never assumed.
    let ceiling = unreliable_ceiling(&b_to_a).await;
    eprintln!("[gtp smoke] Unreliable payload ceiling = {} bytes", ceiling);
    assert!(ceiling >= 1000, "ceiling unexpectedly small: {}", ceiling);
    assert!(ceiling < 2814, "a full 2800-MTU frame fits Unreliable; the stream fallback would be unnecessary");
    // Drain the probe datagrams (best effort: some may be dropped).
    while tokio::time::timeout(Duration::from_millis(200), a_from_b.recv()).await.is_ok() {}

    // --- Control messages: zgalaxy's wire encoding over ReliableOrdered,
    // strictly in order. GTP-rs reserves OrderedGroupId(0) for UNORDERED
    // delivery (it arrives as ReliableUnordered), so control uses group 1.
    let n = 50u64;
    for i in 0..n {
        let msg = ControlMessage::Ping { nonce: i, sent_ms: 0 };
        b_to_a
            .send_reliable_ordered(OrderedGroupId(CONTROL_GROUP), control::encode(&msg).unwrap(), PriorityTier::P0Control)
            .await
            .unwrap();
    }
    for i in 0..n {
        let m = recv(&mut a_from_b).await;
        assert!(
            matches!(m.class, MessageClass::ReliableOrdered { group_id, .. } if group_id == OrderedGroupId(CONTROL_GROUP)),
            "class {:?}",
            m.class
        );
        match control::decode(&m.payload).unwrap() {
            ControlMessage::Ping { nonce, .. } => assert_eq!(nonce, i, "control messages arrive in order"),
            other => panic!("unexpected control message {:?}", other),
        }
    }

    // --- A full 2800-MTU Ethernet frame (2814 bytes) over ReliableUnordered:
    // the planned fallback for frames above the Unreliable ceiling.
    let big = (0..2814u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>();
    let sent = Instant::now();
    let r = b_to_a.send_reliable_unordered(big.clone(), PriorityTier::P2WorldState).await;
    eprintln!("[gtp smoke] ReliableUnordered 2814 B send -> {:?}", r.as_ref().map(|_| ()));
    r.expect("ReliableUnordered must fragment a 2814-byte frame");
    let m = recv(&mut a_from_b).await;
    eprintln!("[gtp smoke] 2814 B frame delivered in {:?}", sent.elapsed());
    assert!(matches!(m.class, MessageClass::ReliableUnordered));
    assert_eq!(m.payload, big);

    // --- The mesh frame limit (64 KiB) over ReliableUnordered.
    let max = vec![0xA5u8; 64 * 1024];
    match b_to_a.send_reliable_unordered(max.clone(), PriorityTier::P2WorldState).await {
        Ok(_) => {
            assert_eq!(recv(&mut a_from_b).await.payload.len(), max.len());
            eprintln!("[gtp smoke] 64 KiB reliable message: delivered");
        }
        Err(e) => eprintln!("[gtp smoke] 64 KiB reliable message: refused ({:?})", e),
    }

    // --- One endpoint both accepts (from B above) and dials (to C).
    let (c, c_addr) = endpoint().await;
    let (mut a_to_c, mut c_from_a) = pair(&a, &c, c_addr, 0x5A47_0000_0000_0002).await;
    a_to_c.send_unreliable(b"from-a".to_vec(), PriorityTier::P2WorldState).await.unwrap();
    assert_eq!(recv(&mut c_from_a).await.payload, b"from-a");
    c_from_a.send_unreliable(b"from-c".to_vec(), PriorityTier::P2WorldState).await.unwrap();
    assert_eq!(recv(&mut a_to_c).await.payload, b"from-c");
    // The B session is unaffected by the second one.
    b_to_a.send_unreliable(b"still-here".to_vec(), PriorityTier::P2WorldState).await.unwrap();
    assert_eq!(recv(&mut a_from_b).await.payload, b"still-here");

    // --- Close: what does the other side observe, and how fast?
    b_to_a.graceful_close(0, "smoke-done").await.unwrap();
    let closed_at = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match a_from_b.recv().await {
                None => return "recv() -> None",
                Some(_) => continue,
            }
        }
    })
    .await;
    eprintln!(
        "[gtp smoke] after peer graceful_close: {} (after {:?})",
        outcome.unwrap_or("still open after 20s"),
        closed_at.elapsed()
    );
    let events = a_from_b.drain_events().await;
    eprintln!("[gtp smoke] acceptor control events: {:?}", events);
}

/// Dropping the handles is NOT a close: GTP-rs's endpoint tasks keep the
/// session alive (and answering keepalives) after both the
/// `AsyncGtpConnection` and the `GtpEndpoint` are dropped. `GtpMesh::close`
/// must therefore call `graceful_close` explicitly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_handles_does_not_close_the_session() {
    let (a, a_addr) = endpoint().await;
    let (b, _) = endpoint().await;
    let (b_to_a, mut a_from_b) = pair(&b, &a, a_addr, 0x5A47_0000_0000_0003).await;
    drop(b_to_a);
    drop(b);
    // Well past several keepalive intervals (1 s), short of the idle timeout.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let since = Duration::from_micros(a_from_b.query_metrics().await.since_last_rx.expect("peer was heard from").as_micros());
    eprintln!("[gtp smoke] 4s after dropping B's handles, A last heard from B {:?} ago", since);
    assert!(since < Duration::from_secs(2), "B went silent: GTP-rs now closes on drop; simplify GtpMesh::close");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), a_from_b.recv()).await.is_err(),
        "the session is still open"
    );
}

/// A peer that disappears without a CLOSE (crash, network loss) must still
/// end the session, or `TransportEvent::Disconnected` would never fire.
/// `connect` uses `GtpConfig::competitive_fps()`: 15 s idle timeout. The
/// peer runs on its own runtime; shutting that down kills every task, which
/// is what a crash looks like from the outside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashed_peer_is_detected_by_idle_timeout() {
    let (a, a_addr) = endpoint().await;
    let (cid_tx, cid_rx) = tokio::sync::oneshot::channel();
    let (crash_tx, crash_rx) = std::sync::mpsc::channel::<()>();
    let peer = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        rt.block_on(async {
            let b = GtpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
            let conn = b.connect(ConnectionId(0x5A47_0000_0000_0004), a_addr, true).await.unwrap();
            conn.send_unreliable(b"hello".to_vec(), PriorityTier::P2WorldState).await.unwrap();
            cid_tx.send(()).unwrap();
            // Keep everything alive until told to crash.
            tokio::task::spawn_blocking(move || crash_rx.recv()).await.unwrap().unwrap();
            std::mem::forget((b, conn));
        });
        rt.shutdown_background();
    });
    let mut a_from_b = tokio::time::timeout(STEP, a.accept()).await.unwrap().unwrap();
    cid_rx.await.unwrap();
    assert_eq!(recv(&mut a_from_b).await.payload, b"hello");

    crash_tx.send(()).unwrap();
    peer.join().unwrap();
    let crashed = Instant::now();
    let ended = tokio::time::timeout(Duration::from_secs(25), async {
        while a_from_b.recv().await.is_some() {}
    })
    .await;
    let after = crashed.elapsed();
    eprintln!("[gtp smoke] crashed peer detected after {:?}", after);
    assert!(ended.is_ok(), "a crashed peer must end the session within the idle timeout");
    assert!(after >= Duration::from_secs(10), "ended suspiciously early ({:?}): not the idle timeout", after);
}
