use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::identity::Address;
use serde::{Serialize, Deserialize};

/// Role of a peer in the world topology. Serialized as the uppercase strings
/// used by the canonical ZeroTier service API ("LEAF", "MOON", "PLANET").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PeerRole {
    Planet,
    Moon,
    Leaf,
}

/// A physical network path to a peer.
///
/// JSON shape matches the ZeroTier `/peer` endpoint expected by ZTNET:
/// `address` is a `"ip/port"` string, timestamps are camelCase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPath {
    /// Canonical ZeroTier path string, e.g. `"192.0.2.10/9993"`.
    pub address: String,
    #[serde(rename = "lastSend")]
    pub last_send: u64,
    #[serde(rename = "lastReceive")]
    pub last_receive: u64,
    #[serde(default)]
    pub latency_ms: i32,
    #[serde(default)]
    pub preferred: bool,
    #[serde(rename = "trustedPathId", default)]
    pub trusted_path_id: u64,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub expired: bool,
    #[serde(default)]
    pub fixed: bool,
}

fn default_true() -> bool {
    true
}

fn default_one() -> i32 {
    1
}

fn default_three() -> i32 {
    3
}

fn default_zero() -> i32 {
    0
}

fn default_version_str() -> String {
    "1.3.0".to_string()
}

/// A connected peer. JSON shape matches the ZeroTier `/peer` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub address: Address,
    pub role: PeerRole,
    #[serde(default = "default_version_str")]
    pub version: String,
    #[serde(rename = "versionMajor", default = "default_one")]
    pub version_major: i32,
    #[serde(rename = "versionMinor", default = "default_three")]
    pub version_minor: i32,
    #[serde(rename = "versionRev", default = "default_zero")]
    pub version_rev: i32,
    #[serde(default)]
    pub paths: Vec<PeerPath>,
    /// Top-level latency in milliseconds (ZTNET reads `latency`).
    #[serde(rename = "latency")]
    pub latency_ms: i32,
    #[serde(rename = "lastContact", default)]
    pub last_contact: u64,
    #[serde(rename = "physicalAddress", default)]
    pub physical_address: Option<String>,
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

        // Canonical ZeroTier path notation: "ip/port".
        let path_str = format!("{}/{}", endpoint.ip(), endpoint.port());

        let peer = peers.entry(address).or_insert_with(|| Peer {
            address,
            role,
            version: "1.3.0".to_string(),
            version_major: 1,
            version_minor: 3,
            version_rev: 0,
            paths: Vec::new(),
            latency_ms,
            last_contact: now,
            physical_address: None,
        });

        peer.latency_ms = latency_ms;
        peer.last_contact = now;

        if let Some(path) = peer.paths.iter_mut().find(|p| p.address == path_str) {
            path.last_receive = now;
            path.latency_ms = latency_ms;
        } else {
            peer.paths.push(PeerPath {
                address: path_str,
                last_send: now,
                last_receive: now,
                latency_ms,
                preferred: peer.paths.is_empty(),
                trusted_path_id: 0,
                active: true,
                expired: false,
                fixed: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_peer_json_shape_matches_ztnet() {
        let pm = PeerManager::new();
        let addr = Address([0x12, 0x34, 0x56, 0x78, 0x9a]);
        let endpoint: SocketAddr = "192.0.2.25:9993".parse().unwrap();
        pm.add_or_update_peer(addr, PeerRole::Leaf, endpoint, 14).await;

        let peers = pm.list_peers().await;
        let json = serde_json::to_value(&peers[0]).unwrap();

        // ZTNET reads: role (uppercase), latency, paths[].address ("ip/port"),
        // lastSend/lastReceive (camelCase), physicalAddress.
        assert_eq!(json["role"], "LEAF");
        assert_eq!(json["latency"], 14);
        assert_eq!(json["address"], "123456789a");
        assert_eq!(json["paths"][0]["address"], "192.0.2.25/9993");
        assert!(json["paths"][0]["lastSend"].is_number());
        assert!(json["paths"][0]["lastReceive"].is_number());
        assert_eq!(json["paths"][0]["preferred"], true);
        assert_eq!(json["paths"][0]["trustedPathId"], 0);
        assert_eq!(json["paths"][0]["active"], true);
        assert!(json["physicalAddress"].is_null() || json["physicalAddress"].is_string());
    }
}
