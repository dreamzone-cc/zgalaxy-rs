use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{info, debug};

use crate::control::{unix_millis, ControlMessage};
use crate::identity::Address;
use crate::mesh::MeshTransport;
use crate::peer::{parse_path_address, PeerManager, PeerRole};
use crate::transport::UdpTransport;

/// A keepalive probe to a path without a live session gets this long to dial.
const PROBE_DIAL_TIMEOUT: Duration = Duration::from_secs(3);

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
    mesh: Arc<RwLock<Option<Arc<dyn MeshTransport>>>>,
}

impl NatTraversalEngine {
    pub fn new(peer_manager: PeerManager) -> Self {
        NatTraversalEngine {
            peer_manager,
            path_candidates: Arc::new(RwLock::new(HashMap::new())),
            keepalive_interval: Duration::from_secs(25), // 25s keepalive for stateful NAT firewalls
            transport: Arc::new(RwLock::new(None)),
            mesh: Arc::new(RwLock::new(None)),
        }
    }

    /// Attach the active mesh transport (QUIC today, GTP later) so probes
    /// ride its reliable control channel.
    pub async fn set_mesh_transport(&self, mesh: Arc<dyn MeshTransport>) {
        *self.mesh.write().await = Some(mesh);
    }

    /// Attach the active UDP transport to enable transmitting real keepalive probes (legacy)
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
        let mesh_opt = self.mesh.read().await.clone();
        let transport_opt = self.transport.read().await.clone();

        let mut paths: Vec<(Address, SocketAddr)> = Vec::new();
        for peer in &peers {
            for path in &peer.paths {
                if let Some(endpoint) = parse_path_address(&path.address) {
                    if !paths.iter().any(|(_, e)| *e == endpoint) {
                        paths.push((peer.address, endpoint));
                    }
                }
            }
        }

        if let Some(mesh) = mesh_opt {
            // Live sessions are already probed by the RTT prober and kept up
            // by transport keep-alives; only re-try the others, all at once
            // and bounded, so one dead path never delays the rest.
            let live = mesh.connected_peers().await;
            let now = unix_millis();
            let probes = paths.into_iter().filter(|(_, e)| !live.contains(e)).map(|(node, endpoint)| {
                let mesh = Arc::clone(&mesh);
                async move {
                    debug!("[ZGALAXY NAT KEEPALIVE] Probing path {} for peer {}", endpoint, node);
                    let ping = ControlMessage::Ping { nonce: rand::random::<u64>(), sent_ms: now };
                    let _ = tokio::time::timeout(PROBE_DIAL_TIMEOUT, mesh.send_control(endpoint, &ping)).await;
                }
            });
            futures::future::join_all(probes).await;
        } else if let Some(tp) = transport_opt {
            for (node, endpoint) in paths {
                debug!("[ZGALAXY NAT KEEPALIVE] Probing path {} for peer {}", endpoint, node);
                let _ = tp.send_echo(endpoint).await;
            }
        }
    }
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
