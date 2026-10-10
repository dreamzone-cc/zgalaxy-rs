//! Per-endpoint identity binding (أ3), independent of the transport.
//!
//! Every session runs the same handshake on its reliable channel:
//! `NodeAnnounce → NodeChallenge → AnnounceProof → AnnounceAccepted`.
//! This table tracks, per remote endpoint:
//! - the challenge we issued and are waiting to see proven,
//! - the node address the remote PROVED (only set after a valid proof),
//! - whether the remote accepted OUR proof (config requests may flow).
//!
//! It used to live inside the QUIC transport; it moved here so a GTP backend
//! reuses it unchanged.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, RwLock};
use tracing::warn;

use crate::control::challenge_material;
use crate::identity::Identity;

#[derive(Default)]
struct Inner {
    /// Outstanding challenges: remote -> (nonce, announced address).
    pending: HashMap<SocketAddr, (u64, String)>,
    /// Proven node address per remote.
    proven: HashMap<SocketAddr, String>,
    /// Remotes that verified OUR announcement.
    accepted_us: HashMap<SocketAddr, String>,
}

#[derive(Clone, Default)]
pub struct PeerSessions {
    inner: Arc<RwLock<Inner>>,
    /// Fires whenever a proof is accepted in either direction.
    changed: Arc<Notify>,
}

fn is_node_address(address: &str) -> bool {
    address.len() == 10 && address.chars().all(|c| c.is_ascii_hexdigit())
}

impl PeerSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything about `remote` (new session or session lost).
    pub async fn reset(&self, remote: SocketAddr) {
        let mut inner = self.inner.write().await;
        inner.pending.remove(&remote);
        inner.proven.remove(&remote);
        inner.accepted_us.remove(&remote);
    }

    /// Issue (or refresh) a challenge for the address a remote announced.
    /// Returns the nonce, or None when the address is malformed.
    pub async fn challenge(&self, remote: SocketAddr, address: &str) -> Option<u64> {
        if !is_node_address(address) {
            warn!("[ZGALAXY MESH] remote {} announced invalid address '{}'", remote, address);
            return None;
        }
        let nonce = rand::random::<u64>();
        self.inner
            .write()
            .await
            .pending
            .insert(remote, (nonce, address.to_string()));
        Some(nonce)
    }

    /// Verify an AnnounceProof against the outstanding challenge: (1) a
    /// challenge exists for `remote`, (2) nonce and announced address match,
    /// (3) public_identity parses and its pubkey derives exactly `address`,
    /// (4) the ed25519 signature over the challenge material validates, and
    /// (5) the remote has not already proven a DIFFERENT address on this
    /// session. On success the address becomes the remote's proven identity.
    pub async fn verify_proof(
        &self,
        remote: SocketAddr,
        nonce: u64,
        address: &str,
        signature_hex: &str,
        public_identity: &str,
    ) -> bool {
        let pending = self.inner.read().await.pending.get(&remote).cloned();
        let Some((expected_nonce, expected_address)) = pending else {
            warn!("[ZGALAXY MESH] proof from {} without outstanding challenge", remote);
            return false;
        };
        if expected_nonce != nonce || expected_address != address {
            warn!("[ZGALAXY MESH] stale/mismatched proof from {}", remote);
            return false;
        }
        let Ok(identity) = Identity::parse(public_identity) else {
            warn!("[ZGALAXY MESH] unparseable public identity from {}", remote);
            return false;
        };
        if identity.address.to_string() != address {
            warn!(
                "[ZGALAXY MESH] identity derives {} but announced {} — rejected",
                identity.address, address
            );
            return false;
        }
        let Some(sig) = hex::decode(signature_hex)
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
        else {
            warn!("[ZGALAXY MESH] malformed announce signature from {}", remote);
            return false;
        };
        if !identity.verify(&challenge_material(nonce, address), &sig) {
            warn!("[ZGALAXY MESH] invalid announce signature from {}", remote);
            return false;
        }

        let mut inner = self.inner.write().await;
        if let Some(existing) = inner.proven.get(&remote) {
            if existing != address {
                warn!(
                    "[ZGALAXY MESH] remote {} re-announced a different address ({} != {}) — rejected",
                    remote, address, existing
                );
                return false;
            }
        }
        inner.pending.remove(&remote);
        inner.proven.insert(remote, address.to_string());
        drop(inner);
        self.changed.notify_waiters();
        true
    }

    /// The node address `remote` proved, if any.
    pub async fn proven_address(&self, remote: SocketAddr) -> Option<String> {
        self.inner.read().await.proven.get(&remote).cloned()
    }

    /// Record that `remote` verified OUR announcement.
    pub async fn mark_accepted_us(&self, remote: SocketAddr, address: &str) {
        self.inner
            .write()
            .await
            .accepted_us
            .insert(remote, address.to_string());
        self.changed.notify_waiters();
    }

    pub async fn has_accepted_us(&self, remote: SocketAddr) -> bool {
        self.inner.read().await.accepted_us.contains_key(&remote)
    }

    /// Wait until `remote` accepted our announcement, or `dur` elapses.
    pub async fn await_accepted_us(&self, remote: SocketAddr, dur: Duration) -> bool {
        self.wait_until(dur, || async move { self.has_accepted_us(remote).await }).await
    }

    /// Wait until the identity handshake finished in BOTH directions with
    /// `remote` (it accepted us and proved itself), or `dur` elapses. A
    /// request sent before that can have its answer dropped as unproven.
    pub async fn await_mutual(&self, remote: SocketAddr, dur: Duration) -> bool {
        self.wait_until(dur, || async move {
            let inner = self.inner.read().await;
            inner.accepted_us.contains_key(&remote) && inner.proven.contains_key(&remote)
        })
        .await
    }

    async fn wait_until<F, Fut>(&self, dur: Duration, mut done: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let wait = async {
            loop {
                // Register interest BEFORE checking, so a notification that
                // lands between the check and the await is not lost.
                let notified = self.changed.notified();
                if done().await {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(dur, wait).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_string(id: &Identity) -> String {
        format!("{}:0:{}", id.address, hex::encode(id.verifying_key.as_bytes()))
    }

    /// أ3 negative/positive matrix: the proof must bind (nonce, address,
    /// pubkey) — anything else is rejected.
    #[tokio::test]
    async fn announce_challenge_proof_matrix() {
        let sessions = PeerSessions::new();
        let legit = Identity::generate();
        let addr = legit.address.to_string();
        let pub_str = public_string(&legit);

        // Positive path.
        let remote: SocketAddr = "203.0.113.9:9993".parse().unwrap();
        let nonce = sessions.challenge(remote, &addr).await.unwrap();
        let sig = legit.sign(&challenge_material(nonce, &addr)).unwrap();
        assert!(sessions.verify_proof(remote, nonce, &addr, &hex::encode(sig), &pub_str).await);
        assert_eq!(sessions.proven_address(remote).await.as_deref(), Some(addr.as_str()));

        // Negative: wrong nonce.
        let remote2: SocketAddr = "203.0.113.10:9993".parse().unwrap();
        let nonce = sessions.challenge(remote2, &addr).await.unwrap();
        let sig = legit.sign(&challenge_material(nonce, &addr)).unwrap();
        assert!(!sessions.verify_proof(remote2, nonce ^ 1, &addr, &hex::encode(sig), &pub_str).await);

        // Negative: address/pubkey mismatch (key derives a different address).
        let remote3: SocketAddr = "203.0.113.11:9993".parse().unwrap();
        let nonce = sessions.challenge(remote3, &addr).await.unwrap();
        let sig = legit.sign(&challenge_material(nonce, &addr)).unwrap();
        assert!(
            !sessions
                .verify_proof(remote3, nonce, &addr, &hex::encode(sig), &pub_str.replace(&addr, "2222222222"))
                .await
        );

        // Negative: signature by a different key.
        let other = Identity::generate();
        let remote4: SocketAddr = "203.0.113.12:9993".parse().unwrap();
        let nonce = sessions.challenge(remote4, &addr).await.unwrap();
        let wrong_sig = other.sign(&challenge_material(nonce, &addr)).unwrap();
        assert!(!sessions.verify_proof(remote4, nonce, &addr, &hex::encode(wrong_sig), &pub_str).await);

        // Negative: proof without an outstanding challenge.
        let remote5: SocketAddr = "203.0.113.13:9993".parse().unwrap();
        let sig = legit.sign(&challenge_material(7, &addr)).unwrap();
        assert!(!sessions.verify_proof(remote5, 7, &addr, &hex::encode(sig), &pub_str).await);

        // Negative: malformed announced address never gets a challenge.
        assert!(sessions.challenge(remote5, "not-an-address").await.is_none());
    }

    #[tokio::test]
    async fn a_session_cannot_switch_identity() {
        let sessions = PeerSessions::new();
        let remote: SocketAddr = "198.51.100.1:9993".parse().unwrap();
        for (i, id) in [Identity::generate(), Identity::generate()].iter().enumerate() {
            let addr = id.address.to_string();
            let nonce = sessions.challenge(remote, &addr).await.unwrap();
            let sig = id.sign(&challenge_material(nonce, &addr)).unwrap();
            let ok = sessions.verify_proof(remote, nonce, &addr, &hex::encode(sig), &public_string(id)).await;
            assert_eq!(ok, i == 0, "only the first proven identity binds the session");
        }
        // A reset (new session) lifts the binding.
        sessions.reset(remote).await;
        assert!(sessions.proven_address(remote).await.is_none());
    }

    #[tokio::test]
    async fn await_accepted_wakes_on_mark_and_times_out_otherwise() {
        let sessions = PeerSessions::new();
        let remote: SocketAddr = "198.51.100.2:9993".parse().unwrap();
        assert!(!sessions.await_accepted_us(remote, Duration::from_millis(30)).await);

        let s2 = sessions.clone();
        let waiter = tokio::spawn(async move { s2.await_accepted_us(remote, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        sessions.mark_accepted_us(remote, "abcdef0123").await;
        assert!(waiter.await.unwrap());
    }
}
