use std::collections::HashMap;
use std::net::Ipv4Addr;
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
            mac: "fe:12:34:56:78:9a".to_string(),
            mtu: 2800,
            broadcast_enabled: true,
            assigned_addresses: vec!["10.147.17.100/24".to_string()],
            routes: vec![ManagedRoute {
                target: "10.147.17.0/24".to_string(),
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
