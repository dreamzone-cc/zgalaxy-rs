//! Facts about GTP-rs v1.3 server pinning that the controller-pinning design
//! depends on (docs/GTP-INTEGRATION.md §4.7). Like `gtp_api_smoke.rs`: pin
//! the real behaviour down before building on it.
//!
//! Run: `cargo test --features gtp --test gtp_pinning_facts -- --nocapture`

#![cfg(feature = "gtp")]

use std::net::SocketAddr;
use std::time::Duration;

use gtp::{ConnectionId, GtpEndpoint, PriorityTier};
use zgalaxy_rs::identity::Identity;

const STEP: Duration = Duration::from_secs(5);

async fn bind() -> (GtpEndpoint, SocketAddr) {
    let ep = GtpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = ep.local_addr().unwrap();
    (ep, addr)
}

/// Dial and, when it succeeds, prove the session carries data.
async fn dial(client: &GtpEndpoint, server: &GtpEndpoint, server_addr: SocketAddr, cid: u64) -> Result<(), String> {
    let (dialed, accepted) = tokio::join!(
        tokio::time::timeout(STEP, client.connect(ConnectionId(cid), server_addr, true)),
        tokio::time::timeout(Duration::from_secs(2), server.accept()),
    );
    let conn = match dialed {
        Err(_) => return Err("connect timed out".into()),
        Ok(Err(e)) => return Err(format!("{:?}", e)),
        Ok(Ok(c)) => c,
    };
    let mut accepted = accepted.map_err(|_| "dial ok but accept timed out".to_string())?.ok_or("accept closed")?;
    conn.send_reliable_unordered(b"ping".to_vec(), PriorityTier::P0Control).await.map_err(|e| format!("{:?}", e))?;
    let m = tokio::time::timeout(STEP, accepted.recv()).await.map_err(|_| "no data".to_string())?.ok_or("closed")?;
    assert_eq!(m.payload, b"ping");
    Ok(())
}

/// The pin can be DERIVED from a zgalaxy identity: the ed25519 identity key
/// converts to an X25519 static key (Edwards → Montgomery, as libsodium's
/// crypto_sign_ed25519_*_to_curve25519). The controller feeds the secret
/// half to GTP-rs; a client computes the same public half from the
/// controller's public identity alone, which it verifies against the
/// address in the network id. No extra key, no extra signature.
#[test]
fn identity_key_converts_to_the_gtp_static_key() {
    for _ in 0..4 {
        let id = Identity::generate();
        let seed = id.signing_key.as_ref().unwrap().to_scalar_bytes();
        let server_side = gtp::crypto::StaticIdentity::from_seed(seed).public_key();
        let client_side = id.verifying_key.to_montgomery().to_bytes();
        assert_eq!(server_side, client_side);
        // From the public identity STRING only (what a client receives).
        let public = Identity::parse(&id.to_public_string()).unwrap();
        assert_eq!(public.verifying_key.to_montgomery().to_bytes(), server_side);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinning_semantics() {
    let id = Identity::generate();
    let seed = id.signing_key.as_ref().unwrap().to_scalar_bytes();
    let pin = id.verifying_key.to_montgomery().to_bytes();

    let identified = GtpEndpoint::bind_with_static_identity("127.0.0.1:0".parse().unwrap(), seed).await.unwrap();
    let identified_addr = identified.local_addr().unwrap();
    assert_eq!(identified.static_identity_public(), Some(pin));
    let (anon_server, anon_addr) = bind().await;

    // 1. A pinned client reaches the identified server.
    let mut pinned = GtpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    pinned.set_trusted_server_static(pin);
    let r = dial(&pinned, &identified, identified_addr, 1).await;
    eprintln!("[pin facts] pinned client  -> identified server: {:?}", r);
    assert!(r.is_ok());

    // 2. An UNPINNED client cannot reach the identified server.
    let (unpinned, _) = bind().await;
    let r = dial(&unpinned, &identified, identified_addr, 2).await;
    eprintln!("[pin facts] unpinned client -> identified server: {:?}", r);
    assert!(r.is_err(), "identified servers accept only pinned clients");

    // 3. A wrong pin is refused.
    let mut wrong = GtpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    wrong.set_trusted_server_static(Identity::generate().verifying_key.to_montgomery().to_bytes());
    let r = dial(&wrong, &identified, identified_addr, 3).await;
    eprintln!("[pin facts] wrong pin       -> identified server: {:?}", r);
    assert!(r.is_err());

    // 4. The pin is per ENDPOINT: the pinned endpoint cannot reach any
    // other (anonymous) peer.
    let r = dial(&pinned, &anon_server, anon_addr, 4).await;
    eprintln!("[pin facts] pinned client  -> anonymous server: {:?}", r);
    assert!(r.is_err(), "the pin applies to every connect() of the endpoint");

    // 5. The identified server can still DIAL anonymous peers (its own
    // connect() is unpinned).
    let r = dial(&identified, &anon_server, anon_addr, 5).await;
    eprintln!("[pin facts] identified server dials anonymous peer: {:?}", r);
    assert!(r.is_ok());
}
