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
use hmac::{Hmac, Mac};
use sha2::Sha256;
type HmacSha256 = Hmac<Sha256>;

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

    /// Generate the next unique network ID using cryptographically secure 24-bit random entropy.
    async fn next_network_id(&self) -> String {
        let nets = self.networks.read().await;
        loop {
            let mut postfix = rand::random::<u32>() & 0x00ff_ffff;
            if postfix == 0 {
                postfix = 1;
            }
            let nwid = format!("{}{:06x}", self.controller_address, postfix);
            if !nets.contains_key(&nwid) {
                return nwid;
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
                let mut postfix = rand::random::<u32>() & 0x00ff_ffff;
                if postfix == 0 {
                    postfix = 1;
                }
                id = format!("{}{:06x}", self.controller_address, postfix);
            }
            id
        } else {
            nwid
        };
        let mut net_config = net_config;
        net_config.id = nwid.clone();
        net_config.nwid = nwid.clone();

        // Persist to disk in FileDB format
        let net_file = self.db_path.join("network").join(format!("{}.json", nwid));
        let serialized = serde_json::to_string_pretty(&net_config)?;
        fs::write(&net_file, serialized).await?;

        // Update in-memory state.
        let mut nets = self.networks.write().await;
        nets.insert(nwid.clone(), net_config.clone());
        drop(nets);

        // Reconcile and propagate IP assignments to all existing authorized members
        let mut members = self.members.write().await;
        if let Some(nwid_members) = members.get_mut(&nwid) {
            for (mem_id, member) in nwid_members.iter_mut() {
                if member.authorized && !member.no_auto_assign_ips {
                    let prev_ips = member.ip_assignments.clone();
                    Self::reconcile_derived_v6(&net_config, &nwid, mem_id, &mut member.ip_assignments);
                    if member.ip_assignments != prev_ips {
                        member.revision += 1;
                        let member_dir = self.db_path.join("network").join(&nwid).join("member");
                        let member_file = member_dir.join(format!("{}.json", mem_id));
                        if let Ok(ser) = serde_json::to_string_pretty(member) {
                            let _ = fs::write(&member_file, ser).await;
                        }
                    }
                }
            }
        }
        drop(members);

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

    /// Get member details with effective dynamic IPv4 and IPv6 assignments applied.
    pub async fn get_effective_member(&self, nwid: &str, member_id: &str) -> Option<MemberRecord> {
        let mut record = self.get_member(nwid, member_id).await?;
        if record.authorized && !record.no_auto_assign_ips {
            self.auto_assign_ips_for_member(nwid, member_id, &mut record.ip_assignments).await;
        }
        Some(record)
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
            // Auto-assign IPv4/IPv6 if pool/mode is configured and automatic
            // assignment has not been disabled for this member.
            if !record.no_auto_assign_ips {
                self.auto_assign_ips_for_member(nwid, member_id, &mut record.ip_assignments).await;
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
            self.auto_assign_ips_for_member(nwid, member_id, &mut ip_assignments).await;
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
    /// Update member's lastSeen timestamp and physical address across all networks.
    ///
    /// Loopback-sourced paths (127.0.0.0/8, ::1) are never recorded: they are
    /// unreachable from every other node, and recording them would poison the
    /// rendezvous table distributed to peers.
    pub async fn touch_member_last_seen(&self, member_id: &str, physical_address: &str) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let loopback = physical_address.starts_with("127.")
            || physical_address.starts_with("[::1]")
            || physical_address.starts_with("::1");
        let mut members = self.members.write().await;
        for nwid_members in members.values_mut() {
            if let Some(member) = nwid_members.get_mut(member_id) {
                member.last_seen = now;
                member.clock = now;
                if !loopback {
                    member.physical_address = Some(physical_address.to_string());
                }
            }
        }
    }

    /// Compute deterministic 6PLANE IPv6 address for a member in a network (ZeroTier One canonical RFC).
    pub fn compute_6plane_ip(nwid: &str, member_id: &str) -> Option<String> {
        let nwid_u64 = u64::from_str_radix(nwid, 16).ok()?;
        let member_u64 = u64::from_str_radix(member_id, 16).ok()?;
        let folded = ((nwid_u64 ^ (nwid_u64 >> 32)) as u32).to_be_bytes();
        let mem_bytes = member_u64.to_be_bytes();

        let mut octets = [0u8; 16];
        octets[0] = 0xfc;
        octets[1..5].copy_from_slice(&folded);
        octets[5..10].copy_from_slice(&mem_bytes[3..8]);
        octets[15] = 0x01;

        Some(std::net::Ipv6Addr::from(octets).to_string())
    }

    /// Compute deterministic RFC4193 IPv6 address for a member in a network (ZeroTier One canonical RFC).
    pub fn compute_rfc4193_ip(nwid: &str, member_id: &str) -> Option<String> {
        let nwid_u64 = u64::from_str_radix(nwid, 16).ok()?;
        let member_u64 = u64::from_str_radix(member_id, 16).ok()?;
        let nwid_bytes = nwid_u64.to_be_bytes();
        let mem_bytes = member_u64.to_be_bytes();

        let mut octets = [0u8; 16];
        octets[0] = 0xfd;
        octets[1..9].copy_from_slice(&nwid_bytes);
        octets[9] = 0x99;
        octets[10] = 0x93;
        octets[11..16].copy_from_slice(&mem_bytes[3..8]);

        Some(std::net::Ipv6Addr::from(octets).to_string())
    }

    /// Generate a Semantically Opaque, Cryptographically Stable IPv6 Interface Identifier (RFC 7217).
    ///
    /// Implements RFC 7217 Section 3.3:
    ///   RID = F(Prefix, Net_Iface_ID, Network_ID, DAD_Counter, Secret_Key)
    /// where F is HMAC-SHA256 keyed with the controller's persistent identity key.
    ///
    /// Guarantees:
    /// 1. Stability: The same member ID on the same network prefix receives the identical IPv6 address.
    /// 2. Anti-Scanning / Unpredictability: Interface identifiers are pseudorandomly distributed across
    ///    the custom pool, preventing address scanning, correlation attacks, and device fingerprinting.
    /// 3. Zero-Collision (DAD Loop): If a candidate address conflicts with an existing assignment,
    ///    incrementing `dad_counter` yields a distinct, deterministic, pseudorandom candidate.
    pub fn compute_rfc7217_opaque_ip(
        prefix_start: u128,
        prefix_end: u128,
        nwid: &str,
        member_id: &str,
        secret_key: &[u8],
        dad_counter: u32,
    ) -> Option<std::net::Ipv6Addr> {
        if prefix_end < prefix_start {
            return None;
        }

        let range_size = prefix_end.saturating_sub(prefix_start);
        if range_size == 0 {
            return Some(std::net::Ipv6Addr::from(prefix_start));
        }

        let mut mac = HmacSha256::new_from_slice(secret_key).ok()?;
        mac.update(&prefix_start.to_be_bytes());
        mac.update(&prefix_end.to_be_bytes());
        mac.update(nwid.as_bytes());
        mac.update(member_id.as_bytes());
        mac.update(&dad_counter.to_be_bytes());
        let result = mac.finalize().into_bytes();

        // Extract 128-bit pseudorandom integer from the first 16 bytes of HMAC-SHA256
        let mut rid_bytes = [0u8; 16];
        rid_bytes.copy_from_slice(&result[..16]);
        let rid = u128::from_be_bytes(rid_bytes);

        // Map RID uniformly within the [prefix_start, prefix_end] pool bounds
        let offset = rid % (range_size + 1);
        let candidate = prefix_start.checked_add(offset)?;

        Some(std::net::Ipv6Addr::from(candidate))
    }

    /// Assign auto IPv4 and IPv6 addresses for an authorized member, adding newly enabled modes and removing disabled modes.
    pub async fn auto_assign_ips_for_member(&self, nwid: &str, member_id: &str, current_ips: &mut Vec<String>) {
        let Some(net) = self.get_network(nwid).await else { return };

        // Mode flags control AUTO-assignment only (ZeroTier semantics):
        // switching a mode off never deletes addresses an operator assigned
        // by hand. Only the derived 6PLANE / RFC4193 addresses — which are
        // pure functions of (nwid, member) — follow their flags both ways.

        // 1. IPv4 from the assignment pools.
        let is_v4_zt = net.v4_assign_mode.get("zt").and_then(|v| v.as_bool()).unwrap_or(true);
        if is_v4_zt && !current_ips.iter().any(|ip| !ip.contains(':')) {
            if let Some(free_ip) = self.next_free_ip(nwid, &net.ip_assignment_pools).await {
                current_ips.push(free_ip);
            }
        }

        // 2+3. 6PLANE (/80) and RFC4193 (/128) derived addresses.
        Self::reconcile_derived_v6(&net, nwid, member_id, current_ips);

        // 4. Custom IPv6 range with RFC 7217 opaque identifiers (v6AssignMode.zt).
        let is_zt_v6 = net.v6_assign_mode.get("zt").and_then(|v| v.as_bool()).unwrap_or(false);
        if is_zt_v6 {
            let derived = [Self::compute_6plane_ip(nwid, member_id), Self::compute_rfc4193_ip(nwid, member_id)];
            let has_custom_v6 = current_ips
                .iter()
                .any(|ip| ip.contains(':') && !derived.iter().flatten().any(|d| d == ip));
            if !has_custom_v6 {
                if let Some(free_v6) = self.next_free_ipv6(nwid, member_id, &net.ip_assignment_pools).await {
                    current_ips.push(free_v6);
                }
            }
        }
    }

    /// Add or remove the derived 6PLANE / RFC4193 addresses of `member_id`
    /// to match the network's v6AssignMode. Touches nothing else.
    fn reconcile_derived_v6(net: &NetworkConfig, nwid: &str, member_id: &str, ips: &mut Vec<String>) {
        let modes = [
            ("6plane", Self::compute_6plane_ip(nwid, member_id)),
            ("rfc4193", Self::compute_rfc4193_ip(nwid, member_id)),
        ];
        for (mode, derived) in modes {
            let Some(ip) = derived else { continue };
            let enabled = net.v6_assign_mode.get(mode).and_then(|v| v.as_bool()).unwrap_or(false);
            if enabled {
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            } else {
                ips.retain(|x| x != &ip);
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

    /// Find the next free IPv6 address within custom IPv6 assignment pools using
    /// RFC 7217 Semantically Opaque / Cryptographically Stable Interface Identifiers.
    async fn next_free_ipv6(&self, nwid: &str, member_id: &str, pools: &[IpAssignmentPool]) -> Option<String> {
        let members = self.members.read().await;
        let used: HashSet<u128> = members
            .get(nwid)
            .map(|m| {
                m.values()
                    .flat_map(|r| r.ip_assignments.iter())
                    .filter_map(|ip| ip.parse::<std::net::Ipv6Addr>().ok().map(u128::from))
                    .collect()
            })
            .unwrap_or_default();
        drop(members);

        let secret_key = self.identity.secret_key_bytes();

        for pool in pools {
            let start = pool.ip_range_start.parse::<std::net::Ipv6Addr>().ok();
            let end = pool.ip_range_end.parse::<std::net::Ipv6Addr>().ok();
            let (Some(start), Some(end)) = (start, end) else { continue };
            let (start_u128, end_u128) = (u128::from(start), u128::from(end));
            if end_u128 < start_u128 {
                continue;
            }

            // Attempt RFC 7217 Opaque ID generation with DAD collision loop (up to 256 probes)
            for dad_counter in 0..256 {
                if let Some(candidate_ip) = Self::compute_rfc7217_opaque_ip(
                    start_u128,
                    end_u128,
                    nwid,
                    member_id,
                    &secret_key,
                    dad_counter,
                ) {
                    let cand_u128 = u128::from(candidate_ip);
                    if !used.contains(&cand_u128) {
                        return Some(candidate_ip.to_string());
                    }
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

// ---------------------------------------------------------------------------
// Membership tokens (COM replacement for the QUIC era, per docs/PENDING_WORK
// and REFERENCE_ANALYSIS §4): controller-signed, short-TTL proof that a node
// is an authorized member of a network. Verified before a QUIC peer is
// treated as a member; revocation propagates within TTL.
// ---------------------------------------------------------------------------

/// How long a membership token stays valid after issuance.
pub const MEMBERSHIP_TOKEN_TTL_SECS: u64 = 3600;

impl EmbeddedController {
    /// Issue a signed membership token for an AUTHORIZED member, or None when
    /// the member is unknown / not authorized. The token is
    /// base64url(payload).base64url(ed25519-signature).
    pub async fn issue_membership_token(&self, nwid: &str, member_id: &str) -> Option<String> {
        let member = self.members.read().await.get(nwid)?.get(member_id)?.clone();
        if !member.authorized {
            return None;
        }
        let rev = self.networks.read().await.get(nwid)?.revision;
        let iat = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        let payload = serde_json::json!({
            "n": nwid,
            "m": member_id,
            "r": rev,
            "t": iat,
            "e": iat + MEMBERSHIP_TOKEN_TTL_SECS,
            "c": self.controller_address.to_string(),
        });
        let body = serde_json::to_vec(&payload).ok()?;
        let sig = self.identity.sign(&body).ok()?;
        Some(format!(
            "{}.{}",
            b64url(&body),
            b64url(&sig)
        ))
    }

    /// Verify a membership token against this controller's identity.
    /// Returns the token payload when valid for (nwid, member_id).
    pub fn verify_membership_token(
        &self,
        token: &str,
        nwid: &str,
        member_id: &str,
    ) -> Option<serde_json::Value> {
        let mut parts = token.split('.');
        let body_b = b64url_decode(parts.next()?)?;
        let sig_b = b64url_decode(parts.next()?)?;
        if parts.next().is_some() || sig_b.len() != 64 {
            return None;
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_b);
        if !self.identity.verify(&body_b, &sig) {
            return None;
        }
        let payload: serde_json::Value = serde_json::from_slice(&body_b).ok()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        let exp = payload.get("e").and_then(|v| v.as_u64())?;
        let iat = payload.get("t").and_then(|v| v.as_u64())?;
        if now > exp || iat > exp {
            return None;
        }
        if payload.get("n").and_then(|v| v.as_str())? != nwid
            || payload.get("m").and_then(|v| v.as_str())? != member_id
            || payload.get("c").and_then(|v| v.as_str())? != self.controller_address.to_string()
        {
            return None;
        }
        Some(payload)
    }
}

fn b64url(data: &[u8]) -> String {
    // Minimal standard base64 (no padding stripped on encode side matters here
    // because decode below tolerates padding).
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn b64url_decode(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data).ok()
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

    /// Mode flags only control AUTO-assignment: switching them off must not
    /// delete addresses an operator assigned by hand (ZeroTier semantics),
    /// while derived 6PLANE/RFC4193 addresses still follow their flags.
    #[tokio::test]
    async fn test_mode_flags_never_delete_manual_addresses() {
        let controller = test_controller();
        let _ = controller.init().await;
        let net = controller
            .save_network(json!({
                "name": "Manual",
                "private": true,
                "v4AssignMode": {"zt": false},
                "v6AssignMode": {"zt": false, "6plane": false, "rfc4193": true}
            }))
            .await
            .unwrap();
        let nwid = net.id.clone();
        let member = "abcdef0123";
        let manual = vec!["10.50.0.7".to_string(), "fd00:1234::7".to_string(), "fc00::9".to_string()];
        let rec = controller
            .save_member(&nwid, member, json!({"authorized": true, "ipAssignments": manual}))
            .await
            .unwrap();
        for ip in &manual {
            assert!(rec.ip_assignments.contains(ip), "manual {} was deleted: {:?}", ip, rec.ip_assignments);
        }
        let rfc4193 = EmbeddedController::compute_rfc4193_ip(&nwid, member).unwrap();
        assert!(rec.ip_assignments.contains(&rfc4193), "enabled rfc4193 address is derived");

        // Turning rfc4193 off removes exactly the derived address.
        controller
            .save_network(json!({"id": nwid, "v6AssignMode": {"zt": false, "6plane": false, "rfc4193": false}}))
            .await
            .unwrap();
        let rec = controller.get_effective_member(&nwid, member).await.unwrap();
        assert!(!rec.ip_assignments.contains(&rfc4193));
        for ip in &manual {
            assert!(rec.ip_assignments.contains(ip), "manual {} was deleted: {:?}", ip, rec.ip_assignments);
        }
        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
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

    /// Generated network IDs must have 24-bit random entropy and must not be sequential (e.g. 000001, 000002).
    #[tokio::test]
    async fn test_random_non_sequential_network_ids() {
        let controller = test_controller();
        let _ = controller.init().await;

        let net1 = controller.save_network(json!({ "name": "Net1" })).await.unwrap();
        let net2 = controller.save_network(json!({ "name": "Net2" })).await.unwrap();

        assert_eq!(net1.nwid.len(), 16);
        assert_eq!(net2.nwid.len(), 16);
        assert!(net1.nwid.starts_with(&controller.controller_address.to_string()));
        assert!(net2.nwid.starts_with(&controller.controller_address.to_string()));
        assert_ne!(net1.nwid, net2.nwid);

        let suffix1 = &net1.nwid[10..];
        let suffix2 = &net2.nwid[10..];
        // Must not be sequential 000001 and 000002
        assert!(!(suffix1 == "000001" && suffix2 == "000002"), "Network IDs must be randomly generated, not sequential counters");

        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
    }

    /// Test deterministic 6PLANE and RFC4193 IPv6 calculation and auto-assignment.
    #[tokio::test]
    async fn test_ipv6_6plane_and_rfc4193_auto_assignment() {
        let controller = test_controller();
        let _ = controller.init().await;

        // Test 6PLANE mode
        let net_6plane = controller
            .save_network(json!({
                "name": "6PlaneNet",
                "v6AssignMode": { "6plane": true },
                "ipAssignmentPools": [{"ipRangeStart": "10.147.17.10", "ipRangeEnd": "10.147.17.50"}]
            }))
            .await
            .unwrap();
        let nwid_6p = net_6plane.nwid;

        let member_6p = controller
            .save_member(&nwid_6p, "069ae38092", json!({ "authorized": true }))
            .await
            .unwrap();
        assert!(member_6p.authorized);
        // Must contain both IPv4 and 6PLANE IPv6
        assert!(member_6p.ip_assignments.iter().any(|ip| ip.starts_with("10.147.17.")));
        assert!(member_6p.ip_assignments.iter().any(|ip| ip.starts_with("fc")));

        // Test RFC4193 mode
        let net_rfc = controller
            .save_network(json!({
                "name": "RFC4193Net",
                "v6AssignMode": { "rfc4193": true },
                "ipAssignmentPools": [{"ipRangeStart": "10.200.1.10", "ipRangeEnd": "10.200.1.50"}]
            }))
            .await
            .unwrap();
        let nwid_rfc = net_rfc.nwid;

        let member_rfc = controller
            .save_member(&nwid_rfc, "069ae38092", json!({ "authorized": true }))
            .await
            .unwrap();
        assert!(member_rfc.authorized);
        // Must contain both IPv4 and RFC4193 IPv6 (starts with fd)
        assert!(member_rfc.ip_assignments.iter().any(|ip| ip.starts_with("10.200.1.")));
        assert!(member_rfc.ip_assignments.iter().any(|ip| ip.starts_with("fd")));

        // Exact Canonical ZeroTier Vector Verification
        let user_nwid = "ef313fb5c9f6817a";
        let user_member = "5b07896437";
        let calc_rfc = EmbeddedController::compute_rfc4193_ip(user_nwid, user_member).unwrap();
        let calc_6p = EmbeddedController::compute_6plane_ip(user_nwid, user_member).unwrap();
        assert_eq!(calc_rfc, "fdef:313f:b5c9:f681:7a99:935b:789:6437");
        assert_eq!(calc_6p, "fc26:c7be:cf5b:789:6437::1");

        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
    }

    /// Test RFC 7217 Opaque Interface Identifier stability, anti-scanning unpredictability, and DAD collision resolution.
    #[tokio::test]
    async fn test_rfc7217_opaque_ipv6_stability_and_unpredictability() {
        let controller = test_controller();
        let _ = controller.init().await;

        let start_ip: std::net::Ipv6Addr = "2001:db8:acad::1".parse().unwrap();
        let end_ip: std::net::Ipv6Addr = "2001:db8:acad::ffff:ffff".parse().unwrap();
        let start_u128 = u128::from(start_ip);
        let end_u128 = u128::from(end_ip);
        let secret_key = controller.identity.secret_key_bytes();
        let nwid = "ef313fb5c9f6817a";

        // 1. Stability: same node receives identical IPv6 address across multiple runs
        let ip_node_a_run1 = EmbeddedController::compute_rfc7217_opaque_ip(
            start_u128, end_u128, nwid, "5b07896437", &secret_key, 0,
        ).unwrap();
        let ip_node_a_run2 = EmbeddedController::compute_rfc7217_opaque_ip(
            start_u128, end_u128, nwid, "5b07896437", &secret_key, 0,
        ).unwrap();
        assert_eq!(ip_node_a_run1, ip_node_a_run2, "RFC 7217 must be 100% deterministic and stable for same node");

        // 2. Unpredictability / Anti-Scanning: different nodes receive pseudorandom, non-sequential addresses
        let ip_node_b = EmbeddedController::compute_rfc7217_opaque_ip(
            start_u128, end_u128, nwid, "5b07896438", &secret_key, 0,
        ).unwrap();
        let ip_node_c = EmbeddedController::compute_rfc7217_opaque_ip(
            start_u128, end_u128, nwid, "5b07896439", &secret_key, 0,
        ).unwrap();
        assert_ne!(ip_node_a_run1, ip_node_b);
        assert_ne!(ip_node_b, ip_node_c);
        let diff_ab = (u128::from(ip_node_a_run1) as i128 - u128::from(ip_node_b) as i128).abs();
        assert!(diff_ab > 1, "RFC 7217 identifiers must be semantically opaque, not linear sequences");

        // 3. Duplicate Address Detection (DAD): incrementing dad_counter generates a fresh candidate
        let ip_node_a_dad1 = EmbeddedController::compute_rfc7217_opaque_ip(
            start_u128, end_u128, nwid, "5b07896437", &secret_key, 1,
        ).unwrap();
        assert_ne!(ip_node_a_run1, ip_node_a_dad1, "DAD loop must produce distinct candidate on collision");

        // 4. Test within controller auto-assignment workflow
        let net_custom = controller
            .save_network(json!({
                "name": "CustomV6Net",
                "v6AssignMode": { "zt": true },
                "ipAssignmentPools": [{
                    "ipRangeStart": "2001:db8:1234::10",
                    "ipRangeEnd": "2001:db8:1234::ffff"
                }]
            }))
            .await
            .unwrap();

        let member = controller
            .save_member(&net_custom.nwid, "5b07896437", json!({ "authorized": true }))
            .await
            .unwrap();
        assert!(member.authorized);
        assert!(member.ip_assignments.iter().any(|ip| ip.starts_with("2001:db8:1234:")));

        let _ = std::fs::remove_dir_all(controller.db_path.parent().unwrap());
    }
}

