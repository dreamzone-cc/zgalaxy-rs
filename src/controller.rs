use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};
use serde_json::{json, Value};
use tracing::{info, warn, error};
use anyhow::{bail, Context, Result};

use crate::identity::{Address, Identity};

/// Route definition within a ZeroTier network configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkRoute {
    pub target: String,
    pub via: Option<String>,
}

/// IPv4 / IPv6 Assignment Pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpAssignmentPool {
    #[serde(rename = "ipRangeStart")]
    pub ip_range_start: String,
    #[serde(rename = "ipRangeEnd")]
    pub ip_range_end: String,
}

/// ZeroTier Network Configuration (Embedded Controller Model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub id: String,
    pub nwid: String,
    pub name: String,
    pub private: bool,
    #[serde(rename = "creationTime")]
    pub creation_time: u64,
    pub revision: u64,
    pub mtu: u32,
    #[serde(rename = "multicastLimit")]
    pub multicast_limit: u32,
    #[serde(rename = "enableBroadcast")]
    pub enable_broadcast: bool,
    pub routes: Vec<NetworkRoute>,
    #[serde(rename = "ipAssignmentPools")]
    pub ip_assignment_pools: Vec<IpAssignmentPool>,
    #[serde(rename = "v4AssignMode")]
    pub v4_assign_mode: Value,
    #[serde(rename = "v6AssignMode")]
    pub v6_assign_mode: Value,
    pub rules: Vec<Value>,
    pub capabilities: Vec<Value>,
    pub tags: Vec<Value>,
}

/// Network Member Status and Authorization Record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    pub id: String,
    pub nwid: String,
    pub objtype: String,
    pub authorized: bool,
    #[serde(rename = "activeBridge")]
    pub active_bridge: bool,
    #[serde(rename = "ipAssignments")]
    pub ip_assignments: Vec<String>,
    pub revision: u64,
    #[serde(rename = "creationTime")]
    pub creation_time: u64,
    #[serde(rename = "lastAuthorizedTime")]
    pub last_authorized_time: u64,
    #[serde(rename = "lastDeauthorizedTime")]
    pub last_deauthorized_time: u64,
    pub identity: Option<String>,
}

/// Embedded ZeroTier Network Controller Engine (100% Pure Rust AGPL-3.0 Clean-Room).
/// Replaces legacy C++ nonfree/controller with a high-performance, memory-safe implementation.
#[derive(Clone)]
pub struct EmbeddedController {
    controller_address: Address,
    identity: Identity,
    db_path: PathBuf,
    networks: Arc<RwLock<HashMap<String, NetworkConfig>>>,
    members: Arc<RwLock<HashMap<String, HashMap<String, MemberRecord>>>>,
}

impl EmbeddedController {
    pub fn new(identity: Identity, base_dir: PathBuf) -> Self {
        let db_path = base_dir.join("controller.d");
        let controller_address = identity.address;

        EmbeddedController {
            controller_address,
            identity,
            db_path,
            networks: Arc::new(RwLock::new(HashMap::new())),
            members: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Initialize controller and load existing networks and members from disk (FileDB format).
    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.db_path).await?;
        let net_dir = self.db_path.join("network");
        fs::create_dir_all(&net_dir).await?;

        info!("[ZGALAXY CONTROLLER] Initializing embedded controller at {:?}", self.db_path);

        // Scan and load existing network JSON files
        let mut dir_entries = match fs::read_dir(&net_dir).await {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };

        let mut loaded_nets = 0;
        let mut loaded_members = 0;

        while let Ok(Some(entry)) = dir_entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(net) = serde_json::from_str::<NetworkConfig>(&content) {
                        let nwid = net.nwid.clone();
                        self.networks.write().await.insert(nwid.clone(), net);
                        loaded_nets += 1;

                        // Load members for this network
                        let member_dir = net_dir.join(&nwid).join("member");
                        if member_dir.exists() {
                            if let Ok(mut mem_entries) = fs::read_dir(&member_dir).await {
                                let mut net_members = HashMap::new();
                                while let Ok(Some(m_entry)) = mem_entries.next_entry().await {
                                    let m_path = m_entry.path();
                                    if m_path.extension().and_then(|e| e.to_str()) == Some("json") {
                                        if let Ok(m_content) = fs::read_to_string(&m_path).await {
                                            if let Ok(member) = serde_json::from_str::<MemberRecord>(&m_content) {
                                                net_members.insert(member.id.clone(), member);
                                                loaded_members += 1;
                                            }
                                        }
                                    }
                                }
                                self.members.write().await.insert(nwid, net_members);
                            }
                        }
                    }
                }
            }
        }

        info!(
            "[ZGALAXY CONTROLLER READY] Loaded {} networks and {} member records from disk.",
            loaded_nets, loaded_members
        );
        Ok(())
    }

    /// List all 16-hex Network IDs hosted by this controller.
    pub async fn list_networks(&self) -> Vec<String> {
        let nets = self.networks.read().await;
        nets.keys().cloned().collect()
    }

    /// Get complete network configuration.
    pub async fn get_network(&self, nwid: &str) -> Option<NetworkConfig> {
        let nets = self.networks.read().await;
        nets.get(nwid).cloned()
    }

    /// Create or update a network configuration.
    pub async fn save_network(&self, mut config: Value) -> Result<NetworkConfig> {
        let nwid = if let Some(id_val) = config.get("id").or_else(|| config.get("nwid")) {
            let id_str = id_val.as_str().unwrap_or("").to_string();
            if id_str.contains("______") || id_str.is_empty() || id_str.len() != 16 {
                let count = self.networks.read().await.len() + 1;
                format!("{}{:06x}", self.controller_address, count)
            } else {
                id_str
            }
        } else {
            let count = self.networks.read().await.len() + 1;
            format!("{}{:06x}", self.controller_address, count)
        };

        if nwid.len() != 16 {
            bail!("Network ID must be exactly 16 hexadecimal characters");
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

        config["id"] = json!(nwid);
        config["nwid"] = json!(nwid);
        if config.get("creationTime").is_none() {
            config["creationTime"] = json!(now);
        }
        if config.get("revision").is_none() {
            config["revision"] = json!(1);
        }

        let net_config: NetworkConfig = serde_json::from_value(config)
            .context("Failed to deserialize NetworkConfig JSON")?;

        // Persist to disk in FileDB format
        let net_file = self.db_path.join("network").join(format!("{}.json", nwid));
        let serialized = serde_json::to_string_pretty(&net_config)?;
        fs::write(&net_file, serialized).await?;

        // Update in-memory state
        let mut nets = self.networks.write().await;
        nets.insert(nwid.clone(), net_config.clone());

        info!("[ZGALAXY CONTROLLER] Saved network '{}' ({})", net_config.name, nwid);
        Ok(net_config)
    }

    /// Delete a network and its member records.
    pub async fn delete_network(&self, nwid: &str) -> Result<bool> {
        let mut nets = self.networks.write().await;
        if nets.remove(nwid).is_some() {
            let net_file = self.db_path.join("network").join(format!("{}.json", nwid));
            let net_dir = self.db_path.join("network").join(nwid);
            let _ = fs::remove_file(net_file).await;
            let _ = fs::remove_dir_all(net_dir).await;

            let mut members = self.members.write().await;
            members.remove(nwid);

            info!("[ZGALAXY CONTROLLER] Deleted network {}", nwid);
            return Ok(true);
        }
        Ok(false)
    }

    /// List members of a network (returns mapping of MemberID -> Revision).
    pub async fn list_members(&self, nwid: &str) -> HashMap<String, u64> {
        let members = self.members.read().await;
        let mut result = HashMap::new();
        if let Some(nwid_members) = members.get(nwid) {
            for (id, mem) in nwid_members {
                result.insert(id.clone(), mem.revision);
            }
        }
        result
    }

    /// Get member details.
    pub async fn get_member(&self, nwid: &str, member_id: &str) -> Option<MemberRecord> {
        let members = self.members.read().await;
        members.get(nwid).and_then(|m| m.get(member_id)).cloned()
    }

    /// Authorize, deauthorize, or update a network member.
    pub async fn save_member(&self, nwid: &str, member_id: &str, mut member_val: Value) -> Result<MemberRecord> {
        if member_id.len() != 10 {
            bail!("Member ID must be 10 hexadecimal characters (ZeroTier Address)");
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

        member_val["id"] = json!(member_id);
        member_val["nwid"] = json!(nwid);
        member_val["objtype"] = json!("member");
        if member_val.get("creationTime").is_none() {
            member_val["creationTime"] = json!(now);
        }

        let mut record: MemberRecord = serde_json::from_value(member_val)
            .context("Failed to deserialize MemberRecord JSON")?;

        record.revision += 1;
        if record.authorized {
            record.last_authorized_time = now;
            // Auto-assign IP if pool is configured and no IPs are assigned
            if record.ip_assignments.is_empty() {
                if let Some(net) = self.get_network(nwid).await {
                    if let Some(pool) = net.ip_assignment_pools.first() {
                        record.ip_assignments.push(pool.ip_range_start.clone());
                    }
                }
            }
        } else {
            record.last_deauthorized_time = now;
        }

        // Persist to disk under controller.d/network/<nwid>/member/<memberId>.json
        let member_dir = self.db_path.join("network").join(nwid).join("member");
        fs::create_dir_all(&member_dir).await?;
        let member_file = member_dir.join(format!("{}.json", member_id));
        let serialized = serde_json::to_string_pretty(&record)?;
        fs::write(&member_file, serialized).await?;

        // Update in-memory state
        let mut members = self.members.write().await;
        let nwid_map = members.entry(nwid.to_string()).or_insert_with(HashMap::new);
        nwid_map.insert(member_id.to_string(), record.clone());

        info!("[ZGALAXY CONTROLLER] Member {} updated in network {} (authorized: {})", member_id, nwid, record.authorized);
        Ok(record)
    }

    /// Delete member record from controller.
    pub async fn delete_member(&self, nwid: &str, member_id: &str) -> Result<bool> {
        let mut members = self.members.write().await;
        if let Some(nwid_map) = members.get_mut(nwid) {
            if nwid_map.remove(member_id).is_some() {
                let member_file = self.db_path.join("network").join(nwid).join("member").join(format!("{}.json", member_id));
                let _ = fs::remove_file(member_file).await;
                info!("[ZGALAXY CONTROLLER] Member {} deleted from network {}", member_id, nwid);
                return Ok(true);
            }
        }
        Ok(false)
    }
}
