use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use crate::identity::Address;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerRole {
    Planet,
    Moon,
    Leaf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPath {
    pub address: SocketAddr,
    pub last_send: u64,
    pub last_receive: u64,
    pub latency_ms: i32,
    pub preferred: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub address: Address,
    pub role: PeerRole,
    pub version: String,
    pub paths: Vec<PeerPath>,
    pub latency_ms: i32,
    pub last_contact: u64,
}

/// Peer connection and path state manager
#[derive(Clone, Default)]
pub struct PeerManager {
    peers: Arc<RwLock<HashMap<Address, Peer>>>,
}

impl PeerManager {
    pub fn new() -> Self {
        PeerManager {
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_or_update_peer(&self, address: Address, role: PeerRole, endpoint: SocketAddr, latency_ms: i32) {
        let mut peers = self.peers.write().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let peer = peers.entry(address).or_insert_with(|| Peer {
            address,
            role,
            version: "1.3.0".to_string(),
            paths: Vec::new(),
            latency_ms,
            last_contact: now,
        });

        peer.latency_ms = latency_ms;
        peer.last_contact = now;

        if let Some(path) = peer.paths.iter_mut().find(|p| p.address == endpoint) {
            path.last_receive = now;
            path.latency_ms = latency_ms;
        } else {
            peer.paths.push(PeerPath {
                address: endpoint,
                last_send: now,
                last_receive: now,
                latency_ms,
                preferred: peer.paths.is_empty(),
            });
        }
    }

    pub async fn get_peer(&self, address: &Address) -> Option<Peer> {
        let peers = self.peers.read().await;
        peers.get(address).cloned()
    }

    pub async fn list_peers(&self) -> Vec<Peer> {
        let peers = self.peers.read().await;
        peers.values().cloned().collect()
    }
}
