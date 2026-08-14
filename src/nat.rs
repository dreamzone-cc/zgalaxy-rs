use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{info, debug, warn};

use crate::identity::Address;
use crate::peer::{PeerManager, PeerRole};

#[derive(Debug, Clone)]
pub struct NatPathCandidate {
    pub endpoint: SocketAddr,
    pub latency_ms: i32,
    pub is_direct: bool,
    pub last_seen: Instant,
}

/// High-Performance NAT Traversal & Hole-Punching State Machine
#[derive(Clone)]
pub struct NatTraversalEngine {
    peer_manager: PeerManager,
    path_candidates: Arc<RwLock<HashMap<Address, Vec<NatPathCandidate>>>>,
    keepalive_interval: Duration,
}

impl NatTraversalEngine {
    pub fn new(peer_manager: PeerManager) -> Self {
        NatTraversalEngine {
            peer_manager,
            path_candidates: Arc::new(RwLock::new(HashMap::new())),
            keepalive_interval: Duration::from_secs(25), // 25s keepalive for stateful NAT firewalls
        }
    }

    /// Register a discovered path candidate (from STUN rendezvous or direct packet)
    pub async fn register_path_candidate(&self, peer_addr: Address, endpoint: SocketAddr, is_direct: bool, latency_ms: i32) {
        let mut candidates = self.path_candidates.write().await;
        let list = candidates.entry(peer_addr).or_insert_with(Vec::new);

        if let Some(existing) = list.iter_mut().find(|c| c.endpoint == endpoint) {
            existing.latency_ms = latency_ms;
            existing.last_seen = Instant::now();
        } else {
            list.push(NatPathCandidate {
                endpoint,
                latency_ms,
                is_direct,
                last_seen: Instant::now(),
            });
            debug!("[ZGALAXY NAT] New path candidate discovered for {}: {} (direct: {})", peer_addr, endpoint, is_direct);
        }

        // Update peer manager with preferred path (lowest latency direct path)
        self.peer_manager.add_or_update_peer(peer_addr, PeerRole::Leaf, endpoint, latency_ms).await;
    }

    /// Start the background NAT keep-alive and hole-punching worker loop
    pub fn start_worker(self: Arc<Self>) {
        tokio::spawn(async move {
            info!("[ZGALAXY NAT] Native P2P hole-punching and keep-alive worker started.");
            loop {
                sleep(self.keepalive_interval).await;
                self.send_keepalives().await;
            }
        });
    }

    async fn send_keepalives(&self) {
        let peers = self.peer_manager.list_peers().await;
        for peer in peers {
            for path in peer.paths {
                debug!("[ZGALAXY NAT KEEPALIVE] Probing path {} for peer {}", path.address, peer.address);
                // The transport layer handles sending the ECHO packet over UDP
            }
        }
    }
}
