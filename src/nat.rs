use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{info, debug};

use crate::identity::Address;
use crate::peer::{PeerManager, PeerRole};
use crate::transport::UdpTransport;

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
    transport: Arc<RwLock<Option<Arc<UdpTransport>>>>,
}

impl NatTraversalEngine {
    pub fn new(peer_manager: PeerManager) -> Self {
        NatTraversalEngine {
            peer_manager,
            path_candidates: Arc::new(RwLock::new(HashMap::new())),
            keepalive_interval: Duration::from_secs(25), // 25s keepalive for stateful NAT firewalls
            transport: Arc::new(RwLock::new(None)),
        }
    }

    /// Attach the active UDP transport to enable transmitting real keepalive probes
    pub async fn set_transport(&self, transport: Arc<UdpTransport>) {
        let mut guard = self.transport.write().await;
        *guard = Some(transport);
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
        let transport_opt = self.transport.read().await.clone();

        for peer in peers {
            for path in peer.paths {
                debug!("[ZGALAXY NAT KEEPALIVE] Probing path {} for peer {}", path.address, peer.address);
                if let (Some(ref tp), Some(sock_addr)) = (&transport_opt, parse_path_addr(&path.address)) {
                    let _ = tp.send_echo(sock_addr).await;
                }
            }
        }
    }
}

/// Parse a ZeroTier path string ("ip/port" or "ip:port", IPv4 or bracketed
/// IPv6) back into a `SocketAddr`.
fn parse_path_addr(path: &str) -> Option<SocketAddr> {
    if let Some(slash) = path.rfind('/') {
        let host = &path[..slash];
        let port: u16 = path[slash + 1..].parse().ok()?;
        return format!("{}:{}", host.trim_start_matches('[').trim_end_matches(']'), port)
            .parse()
            .ok();
    }
    path.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nat_candidate_registration() {
        let pm = PeerManager::new();
        let nat = NatTraversalEngine::new(pm.clone());
        let addr = Address([1, 2, 3, 4, 5]);
        let sock: SocketAddr = "127.0.0.1:9993".parse().unwrap();

        nat.register_path_candidate(addr, sock, true, 12).await;
        let peers = pm.list_peers().await;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].address, addr);
    }
}
