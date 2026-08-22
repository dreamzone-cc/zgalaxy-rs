use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};
use serde_json::{json, Value};
use tracing::info;
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
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub nwid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub private: bool,
    #[serde(rename = "creationTime", default)]
    pub creation_time: u64,
    #[serde(default)]
    pub revision: u64,
    #[serde(default = "default_mtu")]
    pub mtu: u32,
    #[serde(rename = "multicastLimit", default = "default_multicast_limit")]
    pub multicast_limit: u32,
    #[serde(rename = "enableBroadcast", default)]
    pub enable_broadcast: bool,
    #[serde(default)]
    pub routes: Vec<NetworkRoute>,
    #[serde(rename = "ipAssignmentPools", default)]
    pub ip_assignment_pools: Vec<IpAssignmentPool>,
    #[serde(rename = "v4AssignMode", default)]
    pub v4_assign_mode: Value,
    #[serde(rename = "v6AssignMode", default)]
    pub v6_assign_mode: Value,
    #[serde(default)]
    pub rules: Vec<Value>,
    #[serde(default)]
    pub capabilities: Vec<Value>,
    #[serde(default)]
    pub tags: Vec<Value>,
    /// Network-local DNS configuration (ZTNET sends `{"domain": ..., "servers": [...]}`).
    #[serde(default)]
    pub dns: Value,
}

fn default_mtu() -> u32 {
    2800
}

fn default_multicast_limit() -> u32 {
    32
}

fn default_proto_version() -> u32 {
    12
}

fn default_client_version() -> String {
    "1.3.0".to_string()
}

/// Network Member Status and Authorization Record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub nwid: String,
    #[serde(default)]
    pub objtype: String,
    #[serde(default)]
    pub authorized: bool,
    #[serde(rename = "activeBridge", default)]
    pub active_bridge: bool,
    #[serde(rename = "ipAssignments", default)]
    pub ip_assignments: Vec<String>,
    #[serde(rename = "noAutoAssignIps", default)]
    pub no_auto_assign_ips: bool,
    #[serde(default)]
    pub revision: u64,
    #[serde(rename = "creationTime", default)]
    pub creation_time: u64,
    #[serde(rename = "lastAuthorizedTime", default)]
    pub last_authorized_time: u64,
    #[serde(rename = "lastDeauthorizedTime", default)]
    pub last_deauthorized_time: u64,
    #[serde(rename = "lastSeen", default)]
    pub last_seen: u64,
    #[serde(rename = "physicalAddress", default)]
    pub physical_address: Option<String>,
    #[serde(rename = "clientVersion", default = "default_client_version")]
    pub client_version: String,
    #[serde(rename = "protocolVersion", default = "default_proto_version")]
    pub protocol_version: u32,
    #[serde(default)]
    pub clock: u64,
    pub identity: Option<String>,
    /// Member display name (ZTNET renames members with a partial `{name}` payload).
    #[serde(default)]
    pub name: Option<String>,
    /// Member capabilities (ZTNET flow rules).
    #[serde(default)]
    pub capabilities: Vec<Value>,
    /// Member tags (ZTNET sends `[[tagId, value], ...]`).
    #[serde(default)]
    pub tags: Vec<Value>,
}

/// Embedded ZeroTier Network Controller Engine (100% Pure Rust AGPL-3.0 Clean-Room).
/// Replaces legacy C++ nonfree/controller with a high-performance, memory-safe implementation.
#[derive(Clone)]
pub struct EmbeddedController {
    controller_address: Address,
    #[allow(dead_code)]
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

    /// Generate the next unique network ID (controller address + 6-hex counter).
    async fn next_network_id(&self) -> String {
        let nets = self.networks.read().await;
        let mut counter: u64 = 1;
        loop {
            let candidate = format!("{:06x}", counter);
            let nwid = format!("{}{}", self.controller_address, candidate);
            if !nets.contains_key(&nwid) {
                return nwid;
            }
            counter += 1;
            if counter > 0x00ff_ffff {
                return format!("{}{:06x}", self.controller_address, rand::random::<u32>() & 0x00ff_ffff);
            }
        }
    }

    /// Create or update a network configuration.
    ///
    /// Compatibility note: ZTNET posts PARTIAL network payloads (e.g. only
    /// `{"name": "..."}` or `{"v4AssignMode": {...}}`) to update a single
    /// setting. A partial payload must be MERGED over the existing network
    /// configuration, not replace it, otherwise every update would wipe the
    /// other settings back to defaults.
    pub async fn save_network(&self, config: Value) -> Result<NetworkConfig> {
        let (nwid, generated) = if let Some(id_val) = config.get("id").or_else(|| config.get("nwid")) {
            let id_str = id_val.as_str().unwrap_or("").to_string();
            if id_str.contains("______") || id_str.is_empty() || id_str.len() != 16 {
                (self.next_network_id().await, true)
            } else {
                (id_str, false)
            }
        } else {
            (self.next_network_id().await, true)
        };

        if nwid.len() != 16 {
            bail!("Network ID must be exactly 16 hexadecimal characters");
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

        // Merge partial updates over the existing configuration.
        let existing = self.get_network(&nwid).await;
        let mut merged = if let Some(existing_net) = existing {
            let mut base = serde_json::to_value(&existing_net)?;
            if let (Some(base_obj), Some(new_obj)) = (base.as_object_mut(), config.as_object()) {
                for (k, v) in new_obj {
                    base_obj.insert(k.clone(), v.clone());
                }
            }
            base
        } else {
            config.clone()
        };

        merged["id"] = json!(nwid);
        merged["nwid"] = json!(nwid);
        if merged.get("creationTime").is_none() {
            merged["creationTime"] = json!(now);
        }
        // Bump the revision on every save; new networks start at 1.
        let next_revision = merged
            .get("revision")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        merged["revision"] = json!(next_revision);

        let net_config: NetworkConfig = serde_json::from_value(merged)
            .context("Failed to deserialize NetworkConfig JSON")?;

        // If the ID was freshly generated, re-check under the write lock to
        // avoid a duplicate-ID race between concurrent create calls, before
        // anything is persisted to disk or memory.
        let nwid = if generated {
            let nets = self.networks.write().await;
            let mut id = nwid;
            while nets.contains_key(&id) {
                id = format!("{}{:06x}", self.controller_address, rand::random::<u32>() & 0x00ff_ffff);
            }
            id
        } else {
            nwid
        };

        // Persist to disk in FileDB format
        let net_file = self.db_path.join("network").join(format!("{}.json", nwid));
        let serialized = serde_json::to_string_pretty(&net_config)?;
        fs::write(&net_file, serialized).await?;

        // Update in-memory state.
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
    ///
    /// Compatibility note: ZTNET posts PARTIAL member payloads (e.g. only
    /// `{"name": "..."}`, `{"authorized": true}` or `{"ipAssignments": [...]}`).
    /// A partial payload must be MERGED over the existing member record —
    /// otherwise renaming a member would silently reset `authorized` to false.
    pub async fn save_member(&self, nwid: &str, member_id: &str, member_val: Value) -> Result<MemberRecord> {
        if member_id.len() != 10 {
            bail!("Member ID must be 10 hexadecimal characters (ZeroTier Address)");
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

        // Merge partial updates over the existing member record.
        let existing = self.get_member(nwid, member_id).await;
        let mut merged = if let Some(existing_member) = existing {
            let mut base = serde_json::to_value(&existing_member)?;
            if let (Some(base_obj), Some(new_obj)) = (base.as_object_mut(), member_val.as_object()) {
                for (k, v) in new_obj {
                    base_obj.insert(k.clone(), v.clone());
                }
            }
            base
        } else {
            member_val.clone()
        };

        merged["id"] = json!(member_id);
        merged["nwid"] = json!(nwid);
        merged["objtype"] = json!("member");
        if merged.get("creationTime").is_none() {
            merged["creationTime"] = json!(now);
        }

        let mut record: MemberRecord = serde_json::from_value(merged)
            .context("Failed to deserialize MemberRecord JSON")?;

        record.revision += 1;
        if record.authorized {
            record.last_authorized_time = now;
            // Auto-assign IP if pool is configured, no IPs are assigned, and
            // automatic assignment has not been disabled for this member.
            if record.ip_assignments.is_empty() && !record.no_auto_assign_ips {
                if let Some(net) = self.get_network(nwid).await {
                    if let Some(free_ip) = self.next_free_ip(nwid, &net.ip_assignment_pools).await {
                        record.ip_assignments.push(free_ip);
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

    /// Register a network membership/join request from a member node (ZeroTier Wire/Join Protocol).
    /// If the member record does not exist yet, creates it in pending/unauthorized state (or authorized if public).
    pub async fn register_join_request(&self, nwid: &str, member_id: &str, identity_str: Option<String>) -> Result<MemberRecord> {
        if member_id.len() != 10 || !member_id.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("Member ID must be 10 hexadecimal characters");
        }

        // Reject joins for networks this controller does not own — otherwise
        // unauthenticated peers could create unlimited orphan member files.
        if self.get_network(nwid).await.is_none() {
            bail!("Network {} does not exist on this controller", nwid);
        }

        // If member already exists, return current record
        if let Some(existing) = self.get_member(nwid, member_id).await {
            return Ok(existing);
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let net = self.get_network(nwid).await;
        let is_private = net.as_ref().map(|n| n.private).unwrap_or(true);
        let auto_auth = !is_private;

        let mut ip_assignments = Vec::new();
        if auto_auth {
            if let Some(ref n) = net {
                if let Some(free_ip) = self.next_free_ip(nwid, &n.ip_assignment_pools).await {
                    ip_assignments.push(free_ip);
                }
            }
        }

        let record = MemberRecord {
            id: member_id.to_string(),
            nwid: nwid.to_string(),
            objtype: "member".to_string(),
            authorized: auto_auth,
            active_bridge: false,
            ip_assignments,
            no_auto_assign_ips: false,
            revision: 1,
            creation_time: now,
            last_authorized_time: if auto_auth { now } else { 0 },
            last_deauthorized_time: 0,
            last_seen: now,
            physical_address: None,
            client_version: "1.3.0".to_string(),
            protocol_version: 12,
            clock: now,
            identity: identity_str,
            name: None,
            capabilities: Vec::new(),
            tags: Vec::new(),
        };

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

        info!("[ZGALAXY CONTROLLER] Registered new join request for member {} in network {} (authorized: {})", member_id, nwid, record.authorized);
        Ok(record)
    }

    /// Update member's lastSeen timestamp and physical address across all networks.
    pub async fn touch_member_last_seen(&self, member_id: &str, physical_address: &str) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let mut members = self.members.write().await;
        for nwid_members in members.values_mut() {
            if let Some(member) = nwid_members.get_mut(member_id) {
                member.last_seen = now;
                member.clock = now;
                member.physical_address = Some(physical_address.to_string());
            }
        }
    }

    /// Find the next free IPv4 address within the assignment pools, excluding
    /// addresses already assigned to any member of the network (authorized or
    /// not — stale assignments still occupy the address until removed).
    async fn next_free_ip(&self, nwid: &str, pools: &[IpAssignmentPool]) -> Option<String> {
        let members = self.members.read().await;
        let used: HashSet<u32> = members
            .get(nwid)
            .map(|m| {
                m.values()
                    .flat_map(|r| r.ip_assignments.iter())
                    .filter_map(|ip| ip.parse::<Ipv4Addr>().ok().map(u32::from))
                    .collect()
            })
            .unwrap_or_default();
        drop(members);

        for pool in pools {
            let start = pool.ip_range_start.parse::<Ipv4Addr>().ok();
            let end = pool.ip_range_end.parse::<Ipv4Addr>().ok();
            let (Some(start), Some(end)) = (start, end) else { continue };
            let (start_u32, end_u32) = (u32::from(start), u32::from(end));
            if end_u32 < start_u32 {
                continue;
            }
            for candidate in start_u32..=end_u32 {
                if !used.contains(&candidate) {
                    return Some(Ipv4Addr::from(candidate).to_string());
                }
            }
        }
        None
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn test_controller() -> EmbeddedController {
        let identity = Identity::generate();
        let base = temp_dir().join(format!("zgalaxy_ctrl_test_{}_{}", std::process::id(), rand::random::<u32>()));
        EmbeddedController::new(identity, base)
    }

    /// ZTNET integration: partial network payloads must merge, not wipe.
    #[tokio::test]
    async fn test_network_partial_update_merges() {
        let controller = test_controller();
        let _ = controller.init().await;

        let created = controller
            .save_network(json!({
                "name": "MeshNet",
                "private": true,
                "mtu": 2800,
                "routes": [{"target": "10.9.0.0/24", "via": null}],
                "ipAssignmentPools": [{"ipRangeStart": "10.9.0.10", "ipRangeEnd": "10.9.0.20"}],
                "v4AssignMode": {"zt": true}
            }))
            .await
            .unwrap();
        let nwid = created.nwid;

        // ZTNET-style partial update: name only.
        let renamed = controller
            .save_network(json!({ "id": nwid, "name": "RenamedNet" }))
            .await
            .unwrap();
        assert_eq!(renamed.name, "RenamedNet");
        assert_eq!(renamed.mtu, 2800);
        assert_eq!(renamed.routes.len(), 1);
        assert_eq!(renamed.ip_assignment_pools.len(), 1);
        assert!(renamed.private);
        assert_eq!(renamed.v4_assign_mode, json!({"zt": true}));

        // ZTNET-style partial update: v4AssignMode only.
        let updated = controller
            .save_network(json!({ "id": nwid, "v4AssignMode": {"zt": false} }))
            .await
            .unwrap();
        assert_eq!(updated.name, "RenamedNet");
        assert_eq!(updated.mtu, 2800);
        assert_eq!(updated.routes.len(), 1);
        assert_eq!(updated.v4_assign_mode, json!({"zt": false}));

        // ZTNET-style DNS update.
        let with_dns = controller
            .save_network(json!({ "id": nwid, "dns": {"domain": "mesh.local", "servers": ["10.9.0.1"]} }))
            .await
            .unwrap();
        assert_eq!(with_dns.dns, json!({"domain": "mesh.local", "servers": ["10.9.0.1"]}));
        assert_eq!(with_dns.name, "RenamedNet");

        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
    }

    /// ZTNET integration: partial member payloads must merge, not wipe.
    #[tokio::test]
    async fn test_member_partial_update_merges() {
        let controller = test_controller();
        let _ = controller.init().await;

        let net = controller
            .save_network(json!({
                "name": "MeshNet",
                "ipAssignmentPools": [{"ipRangeStart": "10.9.0.10", "ipRangeEnd": "10.9.0.20"}]
            }))
            .await
            .unwrap();
        let nwid = net.nwid;

        // Authorize a member -> auto-assigned IP from the pool.
        let authorized = controller
            .save_member(&nwid, "1234567890", json!({ "authorized": true }))
            .await
            .unwrap();
        assert!(authorized.authorized);
        assert_eq!(authorized.ip_assignments, vec!["10.9.0.10"]);

        // ZTNET-style partial update: rename only -> authorization and IPs preserved.
        let renamed = controller
            .save_member(&nwid, "1234567890", json!({ "name": "Office-Laptop" }))
            .await
            .unwrap();
        assert!(renamed.authorized, "partial update must not de-authorize the member");
        assert_eq!(renamed.ip_assignments, vec!["10.9.0.10"]);
        assert_eq!(renamed.name.as_deref(), Some("Office-Laptop"));

        // ZTNET stash-style update: de-authorize with cleared assignments.
        let stashed = controller
            .save_member(&nwid, "1234567890", json!({ "authorized": false, "ipAssignments": [] }))
            .await
            .unwrap();
        assert!(!stashed.authorized);
        assert!(stashed.ip_assignments.is_empty());
        assert_eq!(stashed.name.as_deref(), Some("Office-Laptop"));

        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
    }

    /// Members with noAutoAssignIps must not receive an automatic IP.
    #[tokio::test]
    async fn test_member_no_auto_assign_ips() {
        let controller = test_controller();
        let _ = controller.init().await;

        let net = controller
            .save_network(json!({
                "name": "MeshNet",
                "ipAssignmentPools": [{"ipRangeStart": "10.9.0.10", "ipRangeEnd": "10.9.0.20"}]
            }))
            .await
            .unwrap();
        let nwid = net.nwid;

        let member = controller
            .save_member(&nwid, "abcdef9876", json!({ "authorized": true, "noAutoAssignIps": true }))
            .await
            .unwrap();
        assert!(member.authorized);
        assert!(member.no_auto_assign_ips);
        assert!(member.ip_assignments.is_empty(), "noAutoAssignIps must suppress auto-assignment");

        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
    }
}
