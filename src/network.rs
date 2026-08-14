use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};
use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkStatus {
    Ok,
    AccessDenied,
    NotFound,
    PortError,
    RequestingConfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedRoute {
    pub target: String,
    pub via: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    pub nwid: String,
    pub name: String,
    pub status: NetworkStatus,
    pub type_name: String,
    pub mac: String,
    pub mtu: u32,
    pub broadcast_enabled: bool,
    pub assigned_addresses: Vec<String>,
    pub routes: Vec<ManagedRoute>,
    pub port_device_name: String,
}

/// Network membership and routing manager
#[derive(Clone, Default)]
pub struct NetworkManager {
    networks: Arc<RwLock<HashMap<String, Network>>>,
}

impl NetworkManager {
    pub fn new() -> Self {
        NetworkManager {
            networks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn join(&self, nwid: &str) -> Result<Network> {
        let clean_nwid = nwid.trim().to_lowercase();
        if clean_nwid.len() != 16 {
            bail!("Invalid Network ID length: expected 16 hex chars, got {}", clean_nwid.len());
        }

        let mut networks = self.networks.write().await;
        let net = networks.entry(clean_nwid.clone()).or_insert_with(|| Network {
            nwid: clean_nwid.clone(),
            name: format!("ZT-{}", clean_nwid),
            status: NetworkStatus::Ok,
            type_name: "PRIVATE".to_string(),
            mac: Self::derive_mac(&clean_nwid),
            mtu: 2800,
            broadcast_enabled: true,
            assigned_addresses: vec![Self::derive_ipv4(&clean_nwid)],
            routes: vec![ManagedRoute {
                target: Self::derive_route_network(&clean_nwid),
                via: None,
            }],
            port_device_name: format!("zt-{}", &clean_nwid[..6]),
        });

        Ok(net.clone())
    }

    pub async fn leave(&self, nwid: &str) -> Result<bool> {
        let clean_nwid = nwid.trim().to_lowercase();
        let mut networks = self.networks.write().await;
        Ok(networks.remove(&clean_nwid).is_some())
    }

    /// Derive a locally-administered unicast MAC address from the network ID.
    fn derive_mac(nwid: &str) -> String {
        let b = hex::decode(nwid).unwrap_or_default();
        let mut mac = [0u8; 6];
        for (i, byte) in mac.iter_mut().enumerate() {
            *byte = b.get(i + 4).copied().unwrap_or(0);
        }
        mac[0] = (mac[0] & 0xfe) | 0x02;
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        )
    }

    /// Derive a unique 10.x.x.x IPv4 address from the network ID's tail bytes.
    fn derive_ipv4(nwid: &str) -> String {
        let b = hex::decode(nwid).unwrap_or_default();
        let a = b.get(12).copied().unwrap_or(1);
        let c = b.get(13).copied().unwrap_or(2);
        let d = b.get(14).copied().unwrap_or(3);
        format!("10.{}.{}.{}/24", 128 + (a % 126), c, d)
    }

    /// Derive the route network address (subnet) for the derived IPv4 address.
    fn derive_route_network(nwid: &str) -> String {
        let b = hex::decode(nwid).unwrap_or_default();
        let a = b.get(12).copied().unwrap_or(1);
        let c = b.get(13).copied().unwrap_or(2);
        format!("10.{}.{}.0/24", 128 + (a % 126), c)
    }

    pub async fn list(&self) -> Vec<Network> {
        let networks = self.networks.read().await;
        networks.values().cloned().collect()
    }

    pub async fn get(&self, nwid: &str) -> Option<Network> {
        let clean_nwid = nwid.trim().to_lowercase();
        let networks = self.networks.read().await;
        networks.get(&clean_nwid).cloned()
    }
}
