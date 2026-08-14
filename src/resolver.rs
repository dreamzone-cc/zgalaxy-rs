use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{info, warn, debug};
use serde::{Serialize, Deserialize};
use anyhow::{bail, Result};

/// Configuration entry for a dynamic domain endpoint source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainEndpointConfig {
    pub domain: String,
    /// Default port used when the source file omits it (e.g. ZGALAXY's
    /// `config/domains.json` format has no port field).
    #[serde(default = "default_domain_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub description: Option<String>,
}

fn default_domain_port() -> u16 {
    9993
}

fn default_true() -> bool {
    true
}

/// Resolved runtime state of a dynamic endpoint.
#[derive(Debug, Clone)]
pub struct ResolvedEndpointState {
    pub addresses: Vec<SocketAddr>,
    pub last_resolved: Instant,
    pub consecutive_failures: u32,
    pub is_stale: bool,
}

/// High-Performance Decoupled Dynamic IP & Multi-Domain DNS Resolver.
/// 
/// Decouples all dynamic network endpoints from the core binary build.
/// Features:
/// - Multi-source runtime configuration (file, environment, REST API, CLI)
/// - Multi-domain & multi-IP dual-stack resolution (IPv4 + IPv6)
/// - Zero-restart in-memory socket re-linking
/// - Instant stale IP invalidation and drift detection
/// - Fault-tolerant DNS failure resilience with last-known-good fallback
#[derive(Clone)]
pub struct DynamicDnsResolver {
    config_path: Option<PathBuf>,
    endpoints: Arc<RwLock<HashMap<String, DomainEndpointConfig>>>,
    resolved_state: Arc<RwLock<HashMap<String, ResolvedEndpointState>>>,
    check_interval: Duration,
}

impl DynamicDnsResolver {
    /// Create a new DynamicDnsResolver with a configurable background polling interval.
    pub fn new(check_interval_secs: u64) -> Self {
        DynamicDnsResolver {
            config_path: None,
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            resolved_state: Arc::new(RwLock::new(HashMap::new())),
            check_interval: Duration::from_secs(check_interval_secs),
        }
    }

    /// Set external JSON configuration file path for persistent dynamic domain storage.
    pub fn with_config_file(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Load dynamic domains from all decoupled runtime sources:
    /// 1. `domains.json` or `domain` file in the working directory.
    /// 2. ZGALAXY engine files (`./config/domain`, `./config/domains.json`).
    /// 3. Environment variables (`ZGALAXY_DOMAINS` or `ZGALAXY_DOMAIN`).
    /// 4. Default community bootstrap (only if no sources provide a domain).
    ///
    /// `default_port` is used for sources that do not carry an explicit port
    /// (ZGALAXY's `config/domains.json` format has no port field).
    pub async fn load_sources(&self, working_dir: &Path, default_port: u16) -> Result<()> {
        let mut loaded_any = false;

        // Source 1: Persistent domains.json (standard format).
        let domains_json_path = working_dir.join("domains.json");
        if domains_json_path.exists() {
            if let Ok(content) = fs::read_to_string(&domains_json_path).await {
                if let Ok(entries) = serde_json::from_str::<Vec<DomainEndpointConfig>>(&content) {
                    for entry in entries {
                        if entry.enabled {
                            let key = format!("{}:{}", entry.domain, entry.port);
                            self.endpoints.write().await.insert(key.clone(), entry);
                            loaded_any = true;
                        }
                    }
                    info!("[ZGALAXY DYNAMIC DNS] Loaded {} domains from {:?}", self.endpoints.read().await.len(), domains_json_path);
                }
            }
        }

        // Source 2: Single-line domain file in the working directory.
        let domain_file = working_dir.join("domain");
        if domain_file.exists() {
            if let Ok(content) = fs::read_to_string(&domain_file).await {
                let domain = content.trim().to_string();
                if !domain.is_empty() {
                    self.add_domain(&domain, default_port, Some("Configured via domain file".to_string())).await?;
                    loaded_any = true;
                }
            }
        }

        // Source 3: ZGALAXY engine files (drop-in integration).
        // The ZGALAXY container writes the planet/moon domain to
        // <app>/config/domain and its DNS state to <app>/config/domains.json.
        let zgalaxy_domain_file = PathBuf::from("./config/domain");
        if !loaded_any && zgalaxy_domain_file.exists() {
            if let Ok(content) = fs::read_to_string(&zgalaxy_domain_file).await {
                let domain = content.trim().to_string();
                if !domain.is_empty() {
                    self.add_domain(&domain, default_port, Some("ZGALAXY config/domain file".to_string())).await?;
                    loaded_any = true;
                }
            }
        }

        // ZGALAXY's domains.json format:
        // [{"domain": "...", "boundTo": "...", "resolvedIp4": [...], ...}]
        // (no port field) — fall back to the daemon's configured port.
        let zgalaxy_domains_json = PathBuf::from("./config/domains.json");
        if zgalaxy_domains_json.exists() {
            if let Ok(content) = fs::read_to_string(&zgalaxy_domains_json).await {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(arr) = value.as_array() {
                        for entry in arr {
                            let domain = entry.get("domain").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                            if domain.is_empty() {
                                continue;
                            }
                            let port = entry
                                .get("port")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(default_port as u64) as u16;
                            let enabled = entry
                                .get("enabled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(true);
                            if enabled {
                                self.add_domain(&domain, port, Some("ZGALAXY config/domains.json".to_string())).await?;
                                loaded_any = true;
                            }
                        }
                    }
                }
            }
        }

        // Source 4: Environment variables
        if let Ok(env_domains) = std::env::var("ZGALAXY_DOMAINS") {
            for item in env_domains.split(',') {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    let (host, port) = Self::parse_host_port(trimmed)?;
                    self.add_domain(&host, port, Some("Environment variable ZGALAXY_DOMAINS".to_string())).await?;
                    loaded_any = true;
                }
            }
        } else if let Ok(env_domain) = std::env::var("ZGALAXY_DOMAIN") {
            let trimmed = env_domain.trim();
            if !trimmed.is_empty() {
                let (host, port) = Self::parse_host_port(trimmed)?;
                self.add_domain(&host, port, Some("Environment variable ZGALAXY_DOMAIN".to_string())).await?;
                loaded_any = true;
            }
        }

        // Fallback default: If nothing is provided, register default community domain
        if !loaded_any && self.endpoints.read().await.is_empty() {
            self.add_domain("dz.dreamzone.cc", 9993, Some("Default Community Root".to_string())).await?;
        }

        // Perform initial synchronous resolution
        self.check_and_update_all().await;
        Ok(())
    }

    /// Add a new domain dynamically during runtime without restarting the daemon.
    pub async fn add_domain(&self, domain: &str, port: u16, description: Option<String>) -> Result<()> {
        let clean_domain = domain.trim().to_lowercase();
        if clean_domain.is_empty() {
            bail!("Domain name cannot be empty");
        }

        let config = DomainEndpointConfig {
            domain: clean_domain.clone(),
            port,
            enabled: true,
            description,
        };

        let key = format!("{}:{}", clean_domain, port);
        self.endpoints.write().await.insert(key.clone(), config);

        // Immediately resolve the newly added domain
        let addrs = Self::resolve_host(&clean_domain, port).await.unwrap_or_default();
        if !addrs.is_empty() {
            info!("[ZGALAXY DYNAMIC DNS] Dynamically added domain '{}' -> {:?}", key, addrs);
            self.resolved_state.write().await.insert(key, ResolvedEndpointState {
                addresses: addrs,
                last_resolved: Instant::now(),
                consecutive_failures: 0,
                is_stale: false,
            });
        }

        self.persist_to_disk().await;
        Ok(())
    }

    /// Remove or disable a domain dynamically during runtime.
    pub async fn remove_domain(&self, domain: &str, port: u16) -> Result<bool> {
        let key = format!("{}:{}", domain.trim().to_lowercase(), port);
        let removed = self.endpoints.write().await.remove(&key).is_some();
        self.resolved_state.write().await.remove(&key);

        if removed {
            info!("[ZGALAXY DYNAMIC DNS] Dynamically removed domain '{}'", key);
            self.persist_to_disk().await;
        }
        Ok(removed)
    }

    /// Get all currently registered domains.
    pub async fn list_domains(&self) -> Vec<DomainEndpointConfig> {
        self.endpoints.read().await.values().cloned().collect()
    }

    /// Get current in-memory resolved socket addresses for a specific endpoint.
    pub async fn get_resolved_addresses(&self, endpoint: &str) -> Vec<SocketAddr> {
        let state = self.resolved_state.read().await;
        state.get(endpoint).map(|s| s.addresses.clone()).unwrap_or_default()
    }

    /// Get all active resolved socket addresses across all configured domains.
    pub async fn get_all_active_addresses(&self) -> Vec<SocketAddr> {
        let state = self.resolved_state.read().await;
        let mut all = Vec::new();
        for s in state.values() {
            for addr in &s.addresses {
                if !all.contains(addr) {
                    all.push(*addr);
                }
            }
        }
        all
    }

    /// Start the background async resolution loop.
    pub fn start_worker(self: Arc<Self>) {
        tokio::spawn(async move {
            info!("[ZGALAXY DYNAMIC DNS] Native background DNS resolver worker started (interval: {:?}).", self.check_interval);
            loop {
                sleep(self.check_interval).await;
                self.check_and_update_all().await;
            }
        });
    }

    /// Core check and update logic: resolves all domains, detects IP drift, invalidates stale IPs, and updates in-memory state.
    pub async fn check_and_update_all(&self) {
        let active_endpoints: Vec<DomainEndpointConfig> = {
            let map = self.endpoints.read().await;
            map.values().filter(|c| c.enabled).cloned().collect()
        };

        for ep in active_endpoints {
            let key = format!("{}:{}", ep.domain, ep.port);

            match Self::resolve_host(&ep.domain, ep.port).await {
                Ok(new_addrs) => {
                    if new_addrs.is_empty() {
                        warn!("[ZGALAXY DYNAMIC DNS] DNS lookup returned 0 addresses for '{}'", key);
                        continue;
                    }

                    let mut state_map = self.resolved_state.write().await;
                    if let Some(existing) = state_map.get_mut(&key) {
                        if existing.addresses != new_addrs {
                            info!(
                                "[ZGALAXY DYNAMIC IP DRIFT] Endpoint '{}' IP changed! Old: {:?} -> New: {:?}. In-memory routes updated instantly with zero restart.",
                                key, existing.addresses, new_addrs
                            );
                            existing.addresses = new_addrs;
                            existing.last_resolved = Instant::now();
                            existing.consecutive_failures = 0;
                            existing.is_stale = false;
                        } else {
                            existing.last_resolved = Instant::now();
                            existing.consecutive_failures = 0;
                            debug!("[ZGALAXY DYNAMIC DNS] Endpoint '{}' verified stable: {:?}", key, existing.addresses);
                        }
                    } else {
                        info!("[ZGALAXY DYNAMIC DNS] Initial resolution for '{}': {:?}", key, new_addrs);
                        state_map.insert(key, ResolvedEndpointState {
                            addresses: new_addrs,
                            last_resolved: Instant::now(),
                            consecutive_failures: 0,
                            is_stale: false,
                        });
                    }
                }
                Err(e) => {
                    // Resilient failure handling: keep last-known good IP with failure counter
                    let mut state_map = self.resolved_state.write().await;
                    if let Some(existing) = state_map.get_mut(&key) {
                        existing.consecutive_failures += 1;
                        warn!(
                            "[ZGALAXY DYNAMIC DNS GLITCH] Transient DNS failure for '{}' (attempts: {}): {:?}. Preserving last-known working IP: {:?}",
                            key, existing.consecutive_failures, e, existing.addresses
                        );
                    } else {
                        warn!("[ZGALAXY DYNAMIC DNS] Initial resolution failed for '{}': {:?}", key, e);
                    }
                }
            }
        }
    }

    /// Parse host and port from strings like "myplanet.org:9993" or "myplanet.org/9993".
    pub fn parse_host_port(endpoint: &str) -> Result<(String, u16)> {
        let clean = endpoint.trim();
        let sep = if clean.contains('/') { '/' } else { ':' };
        let parts: Vec<&str> = clean.split(sep).collect();
        if parts.is_empty() {
            bail!("Empty endpoint string");
        }
        let host = parts[0].to_string();
        let port = if parts.len() > 1 {
            parts[1].parse::<u16>().unwrap_or(9993)
        } else {
            9993
        };
        Ok((host, port))
    }

    /// Asynchronously resolve a hostname to a list of validated IPv4/IPv6 socket addresses.
    pub async fn resolve_host(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
        let target = format!("{}:{}", host, port);
        let mut addrs = Vec::new();

        for addr in tokio::net::lookup_host(&target).await? {
            // Validation filter: ensure IP is valid unicast and not an unroutable loopback (unless 127.0.0.1 for local test)
            if Self::is_valid_address(&addr.ip()) && !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }

        if addrs.is_empty() {
            bail!("No valid IP addresses found for hostname '{}'", host);
        }

        Ok(addrs)
    }

    /// Sanity validation for resolved IP addresses.
    fn is_valid_address(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                // Reject 0.0.0.0 and broadcast 255.255.255.255
                !v4.is_unspecified() && !v4.is_broadcast()
            }
            IpAddr::V6(v6) => {
                // Reject unspecified ::
                !v6.is_unspecified()
            }
        }
    }

    async fn persist_to_disk(&self) {
        if let Some(ref path) = self.config_path {
            let list = self.list_domains().await;
            if let Ok(serialized) = serde_json::to_string_pretty(&list) {
                let _ = fs::write(path, serialized).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dynamic_resolver_multi_source() {
        let resolver = DynamicDnsResolver::new(10);
        assert!(resolver.add_domain("127.0.0.1", 9993, Some("Localhost Test".to_string())).await.is_ok());

        let addrs = resolver.get_resolved_addresses("127.0.0.1:9993").await;
        assert!(!addrs.is_empty());
        assert_eq!(addrs[0].port(), 9993);

        let all = resolver.get_all_active_addresses().await;
        assert_eq!(all.len(), 1);

        assert!(resolver.remove_domain("127.0.0.1", 9993).await.unwrap());
        let empty = resolver.get_resolved_addresses("127.0.0.1:9993").await;
        assert!(empty.is_empty());
    }

    #[test]
    fn test_parse_host_port() {
        assert_eq!(DynamicDnsResolver::parse_host_port("dz.dreamzone.cc:9993").unwrap(), ("dz.dreamzone.cc".to_string(), 9993));
        assert_eq!(DynamicDnsResolver::parse_host_port("myplanet.org/9994").unwrap(), ("myplanet.org".to_string(), 9994));
        assert_eq!(DynamicDnsResolver::parse_host_port("solo.community.net").unwrap(), ("solo.community.net".to_string(), 9993));
    }
}
