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
    /// Controller-signed membership token from the last NetworkConfig
    /// (COM replacement). Presented with every config request.
    #[serde(default)]
    pub membership_token: Option<String>,
    #[serde(default = "default_true")]
    pub allow_managed: bool,
    #[serde(default)]
    pub allow_global: bool,
    #[serde(default)]
    pub allow_default: bool,
    #[serde(default)]
    pub allow_dns: bool,
}

fn default_true() -> bool {
    true
}

/// Partial update of user-controllable network flags (ZeroTier local-service
/// semantics: absent keys are left untouched). Keys are ZeroTier-style
/// camelCase (allowManaged, allowGlobal, allowDefault, allowDNS).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSettingsUpdate {
    #[serde(default)]
    pub allow_managed: Option<bool>,
    #[serde(default)]
    pub allow_global: Option<bool>,
    #[serde(default)]
    pub allow_default: Option<bool>,
    #[serde(default, rename = "allowDNS")]
    pub allow_dns: Option<bool>,
}

impl NetworkSettingsUpdate {
    pub fn from_json(v: &serde_json::Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.allow_managed.is_none()
            && self.allow_global.is_none()
            && self.allow_default.is_none()
            && self.allow_dns.is_none()
    }
}

/// Network membership and routing manager
#[derive(Clone, Default)]
pub struct NetworkManager {
    networks: Arc<RwLock<HashMap<String, Network>>>,
    /// This node's ZGALAXY address — mixed into per-network MAC derivation so
    /// every node in a network gets a distinct, stable MAC.
    node_address: Arc<RwLock<Option<String>>>,
}

impl NetworkManager {
    pub fn new() -> Self {
        NetworkManager {
            networks: Arc::new(RwLock::new(HashMap::new())),
            node_address: Arc::new(RwLock::new(None)),
        }
    }

    /// Teach the manager its own node address (called once at daemon start).
    pub async fn set_node_address(&self, address: String) {
        *self.node_address.write().await = Some(address);
    }

    pub async fn join(&self, nwid: &str) -> Result<Network> {
        let clean_nwid = nwid.trim().to_lowercase();
        if clean_nwid.len() != 16 || !clean_nwid.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!(
                "Invalid Network ID: expected 16 hex chars, got {:?}",
                clean_nwid
            );
        }

        // Fabricated addresses/routes before the controller answers were a
        // deep-audit finding: a network must report REQUESTING_CONFIGURATION
        // with NO managed addresses until a real NetworkConfig arrives.
        let node = self.node_address.read().await.clone().unwrap_or_default();
        let mut networks = self.networks.write().await;
        let net = networks.entry(clean_nwid.clone()).or_insert_with(|| Network {
            nwid: clean_nwid.clone(),
            name: format!("ZT-{}", clean_nwid),
            status: NetworkStatus::RequestingConfiguration,
            type_name: "PRIVATE".to_string(),
            mac: Self::derive_mac(&clean_nwid, &node),
            mtu: 2800,
            broadcast_enabled: true,
            assigned_addresses: Vec::new(),
            routes: Vec::new(),
            port_device_name: format!("zt-{}", &clean_nwid[..6]),
            membership_token: None,
            allow_managed: true,
            allow_global: false,
            allow_default: false,
            allow_dns: false,
        });

        Ok(net.clone())
    }

    pub async fn update_network(&self, mut net: Network) {
        if net.status == NetworkStatus::Ok {
            if let Some(ref node) = *self.node_address.read().await {
                if let Some(v6_6p) = crate::controller::EmbeddedController::compute_6plane_ip(&net.nwid, node) {
                    if !net.assigned_addresses.contains(&v6_6p) {
                        net.assigned_addresses.push(v6_6p);
                    }
                }
                if let Some(v6_rfc) = crate::controller::EmbeddedController::compute_rfc4193_ip(&net.nwid, node) {
                    if !net.assigned_addresses.contains(&v6_rfc) {
                        net.assigned_addresses.push(v6_rfc);
                    }
                }
            }
        }
        let mut networks = self.networks.write().await;
        networks.insert(net.nwid.clone(), net);
    }

    /// Apply a partial settings update to a joined network. Returns the
    /// updated network, or None when not a member.
    pub async fn update_settings(
        &self,
        nwid: &str,
        update: NetworkSettingsUpdate,
    ) -> Option<Network> {
        let clean_nwid = nwid.trim().to_lowercase();
        let mut networks = self.networks.write().await;
        let net = networks.get_mut(&clean_nwid)?;
        if let Some(v) = update.allow_managed {
            net.allow_managed = v;
        }
        if let Some(v) = update.allow_global {
            net.allow_global = v;
        }
        if let Some(v) = update.allow_default {
            net.allow_default = v;
        }
        if let Some(v) = update.allow_dns {
            net.allow_dns = v;
        }
        Some(net.clone())
    }

    pub async fn leave(&self, nwid: &str) -> Result<bool> {
        let clean_nwid = nwid.trim().to_lowercase();
        let mut networks = self.networks.write().await;
        Ok(networks.remove(&clean_nwid).is_some())
    }

    /// Derive a locally-administered unicast MAC from (node address, nwid).
    /// Distinct per node within a network and stable across restarts —
    /// matches ZeroTier's MAC::fromAddress semantics.
    fn derive_mac(nwid: &str, node_address: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(node_address.as_bytes());
        hasher.update(b"|");
        hasher.update(nwid.as_bytes());
        let digest = hasher.finalize();
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&digest[..6]);
        mac[0] = (mac[0] & 0xfe) | 0x02; // locally administered, unicast
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        )
    }

    /// Derive a unique 10.x.x.x IPv4 address from the network ID's tail bytes.
    #[allow(dead_code)]
    fn derive_ipv4(nwid: &str) -> String {
        let b = hex::decode(nwid).unwrap_or_default();
        let a = b.get(12).copied().unwrap_or(1);
        let c = b.get(13).copied().unwrap_or(2);
        let d = b.get(14).copied().unwrap_or(3);
        format!("10.{}.{}.{}/24", 128 + (a % 126), c, d)
    }

    /// Derive the route network address (subnet) for the derived IPv4 address.
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn join_reports_requesting_configuration_without_fabricated_addresses() {
        let nm = NetworkManager::new();
        nm.set_node_address("a1b2c3d4e5".into()).await;
        let net = nm.join("0123456789abcdef").await.unwrap();
        assert_eq!(net.status, NetworkStatus::RequestingConfiguration);
        assert!(net.assigned_addresses.is_empty(), "join must not invent addresses");
        assert!(net.routes.is_empty(), "join must not invent routes");
        assert!(!nm.join("not-hex!!").await.is_ok());
        assert!(nm.join("1234").await.is_err());
    }

    #[tokio::test]
    async fn macs_are_deterministic_and_per_node_distinct() {
        let nm1 = NetworkManager::new();
        nm1.set_node_address("aaaaaaaaaa".into()).await;
        let nm2 = NetworkManager::new();
        nm2.set_node_address("bbbbbbbbbb".into()).await;
        let nwid = "0123456789abcdef";

        let m1a = nm1.join(nwid).await.unwrap().mac;
        let m1b = nm1.join(nwid).await.unwrap().mac; // stable
        assert_eq!(m1a, m1b);
        let m2 = nm2.join(nwid).await.unwrap().mac; // distinct per node
        assert_ne!(m1a, m2);
        // locally administered unicast byte
        let first = u8::from_str_radix(&m1a[..2], 16).unwrap();
        assert_eq!(first & 0x03, 0x02);
    }

    #[tokio::test]
    async fn settings_update_applies_partial_flags_and_requires_membership() {
        let nm = NetworkManager::new();
        nm.set_node_address("a1b2c3d4e5".into()).await;
        let nwid = "0123456789abcdef";
        let net = nm.join(nwid).await.unwrap();
        assert!(net.allow_managed);
        assert!(!net.allow_global);
        assert!(!net.allow_default);
        assert!(!net.allow_dns);

        let u = NetworkSettingsUpdate {
            allow_default: Some(true),
            ..Default::default()
        };
        assert!(!u.is_empty());
        let net = nm.update_settings(nwid, u).await.unwrap();
        assert!(net.allow_managed, "untouched key must stay");
        assert!(net.allow_default);

        let empty = NetworkSettingsUpdate::default();
        assert!(empty.is_empty());
        let net = nm.update_settings(nwid, empty).await.unwrap();
        assert!(net.allow_managed && net.allow_default);

        assert!(
            nm.update_settings("ffffffffffffffff", NetworkSettingsUpdate::default())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn settings_update_parses_zt_style_keys_only() {
        let v = serde_json::json!({"allowManaged": false, "bogus": 7});
        let u = NetworkSettingsUpdate::from_json(&v);
        assert_eq!(u.allow_managed, Some(false));
        assert_eq!(u.allow_global, None);

        let garbage = serde_json::json!({"allowManaged": "not-a-bool"});
        let u = NetworkSettingsUpdate::from_json(&garbage);
        assert_eq!(u.allow_managed, None, "bad types fall back to defaults");
    }
}
