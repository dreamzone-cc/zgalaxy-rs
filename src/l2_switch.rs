//! Layer-2 learning switch for the mesh data plane (ب2).
//!
//! Classic switch semantics over the QUIC mesh:
//! - **Learn**: the SOURCE MAC of every inbound frame is bound to the physical
//!   endpoint that delivered it (standard backward learning).
//! - **Forward**: an outbound frame whose DESTINATION MAC is known is unicast
//!   to exactly one peer; broadcast/multicast and unknown destinations are
//!   flooded to all connected peers.
//!
//! This replaces the "send everything to everyone" pattern: game unicast
//! traffic reaches only its owner, cutting per-frame fan-out from O(N) to O(1).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Ethernet header size (dst 6 + src 6 + ethertype 2).
pub const ETH_HEADER_LEN: usize = 14;

#[derive(Debug, Clone)]
pub struct SwitchEntry {
    pub endpoint: SocketAddr,
    pub last_seen: Instant,
}

/// MAC forwarding table shared by the TUN reader and the QUIC event loop.
#[derive(Clone, Default)]
pub struct L2Switch {
    table: Arc<RwLock<HashMap<[u8; 6], SwitchEntry>>>,
}

impl L2Switch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Backward learning: bind a source MAC to the endpoint that delivered it.
    /// Roaming peers (same MAC, new endpoint) silently move the entry.
    pub async fn learn(&self, mac: [u8; 6], endpoint: SocketAddr) {
        self.table.write().await.insert(
            mac,
            SwitchEntry { endpoint, last_seen: Instant::now() },
        );
    }

    /// Resolve the delivery endpoint for a destination MAC.
    pub async fn resolve(&self, mac: &[u8; 6]) -> Option<SocketAddr> {
        self.table.read().await.get(mac).map(|e| e.endpoint)
    }

    pub async fn entry_count(&self) -> usize {
        self.table.read().await.len()
    }

    /// Evict entries not refreshed within `max_age` (returns evicted count).
    pub async fn evict_stale(&self, max_age: Duration) -> usize {
        let mut table = self.table.write().await;
        let before = table.len();
        table.retain(|_, e| e.last_seen.elapsed() < max_age);
        before - table.len()
    }

    /// Parse `(dst_mac, src_mac)` from a raw Ethernet frame.
    pub fn parse_ethernet(frame: &[u8]) -> Option<([u8; 6], [u8; 6])> {
        if frame.len() < ETH_HEADER_LEN {
            return None;
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&frame[0..6]);
        src.copy_from_slice(&frame[6..12]);
        Some((dst, src))
    }

    /// FF:FF:FF:FF:FF:FF.
    pub fn is_broadcast(mac: &[u8; 6]) -> bool {
        *mac == [0xff; 6]
    }

    /// Multicast MACs have the low bit of the first octet set (excluding
    /// broadcast, which callers normally test first).
    pub fn is_multicast(mac: &[u8; 6]) -> bool {
        !Self::is_broadcast(mac) && (mac[0] & 0x01) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B_MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0x10, 0x20, 0x30];
    const C_MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0xaa, 0xbb, 0xcc];

    #[tokio::test]
    async fn test_learn_and_unicast_resolve() {
        let sw = L2Switch::new();
        let ep_b: SocketAddr = "172.18.0.4:9993".parse().unwrap();

        assert!(sw.resolve(&B_MAC).await.is_none(), "unknown MAC must miss");

        sw.learn(B_MAC, ep_b).await;
        assert_eq!(sw.resolve(&B_MAC).await, Some(ep_b));
        assert_eq!(sw.entry_count().await, 1);
    }

    #[tokio::test]
    async fn test_roaming_peer_moves_entry() {
        let sw = L2Switch::new();
        let ep_old: SocketAddr = "172.18.0.4:1111".parse().unwrap();
        let ep_new: SocketAddr = "172.18.0.4:2222".parse().unwrap();

        sw.learn(B_MAC, ep_old).await;
        sw.learn(B_MAC, ep_new).await; // same MAC re-appeared elsewhere
        assert_eq!(sw.resolve(&B_MAC).await, Some(ep_new), "freshest wins");
        assert_eq!(sw.entry_count().await, 1);
    }

    #[tokio::test]
    async fn test_two_hosts_two_entries() {
        let sw = L2Switch::new();
        sw.learn(B_MAC, "172.18.0.4:9993".parse().unwrap()).await;
        sw.learn(C_MAC, "172.18.0.5:9993".parse().unwrap()).await;
        assert_eq!(sw.entry_count().await, 2);
    }

    #[tokio::test]
    async fn test_evict_stale() {
        let sw = L2Switch::new();
        sw.learn(B_MAC, "172.18.0.4:9993".parse().unwrap()).await;
        // Nothing expires immediately.
        assert_eq!(sw.evict_stale(Duration::from_secs(60)).await, 0);
        // Everything expires with a zero age limit.
        assert_eq!(sw.evict_stale(Duration::from_secs(0)).await, 1);
        assert_eq!(sw.entry_count().await, 0);
    }

    #[test]
    fn test_parse_ethernet() {
        let mut frame = vec![0u8; 60];
        frame[0..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]); // dst
        frame[6..12].copy_from_slice(&[7, 8, 9, 10, 11, 12]); // src
        let (dst, src) = L2Switch::parse_ethernet(&frame).unwrap();
        assert_eq!(dst, [1, 2, 3, 4, 5, 6]);
        assert_eq!(src, [7, 8, 9, 10, 11, 12]);

        assert!(L2Switch::parse_ethernet(&[0u8; 10]).is_none(), "runt frame");
    }

    #[test]
    fn test_broadcast_and_multicast_detection() {
        assert!(L2Switch::is_broadcast(&[0xff; 6]));
        assert!(!L2Switch::is_broadcast(&B_MAC));
        assert!(L2Switch::is_multicast(&[0x01, 0x00, 0x5e, 0, 0, 1])); // IPv4 mcast
        assert!(L2Switch::is_multicast(&[0x33, 0x33, 0, 0, 0, 1])); // IPv6 mcast
        assert!(!L2Switch::is_multicast(&B_MAC)); // locally administered unicast
    }
}
