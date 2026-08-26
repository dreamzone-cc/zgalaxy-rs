use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};
use anyhow::{bail, Context, Result};
use tracing::{info, warn, error, debug};
use crate::identity::Identity;
use crate::peer::{PeerManager, PeerRole};
use crate::world::{World, WORLD_TYPE_PLANET, WORLD_TYPE_MOON};

/// Sync Token type declaration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenType {
    Single,
    Group,
    Contract,
}

/// Cryptographically signed capability token payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub ver: u32,
    pub iss: String,
    pub sub: String,
    pub token_type: TokenType,
    #[serde(default)]
    pub max_devices: u32,
    #[serde(default)]
    pub scope: Vec<String>,
    pub iat: u64,
    pub exp: u64,
    pub jti: String,
    #[serde(default)]
    pub sig: String,
}

/// Information about a single Planet or Moon topology asset
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyItem {
    pub id: String,
    pub world_type: u8,
    pub filename: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub timestamp: u64,
    pub download_url: String,
}

/// Server Manifest returned by ZGALAXY provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub issued_at: u64,
    pub planets: Vec<TopologyItem>,
    pub moons: Vec<TopologyItem>,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_sec: u64,
    #[serde(default)]
    pub signature: Option<String>,
}

fn default_refresh_interval() -> u64 {
    3600
}

/// Configuration for Dynamic Sync Provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub server_url: String,
    pub token: String,
    #[serde(default = "default_sync_interval_mins")]
    pub sync_interval_minutes: u64,
    #[serde(default = "default_true")]
    pub allow_auto_rollback: bool,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_true() -> bool {
    true
}

fn default_sync_interval_mins() -> u64 {
    60
}

fn default_max_retries() -> u32 {
    3
}

/// Status of the dynamic synchronization process
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    Idle,
    Syncing,
    Synced,
    Unchanged,
    Failed(String),
    RolledBack(String),
}

/// Summary report after executing a synchronization cycle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSummary {
    pub status: SyncStatus,
    pub planets_synced: usize,
    pub moons_synced: usize,
    pub etag: Option<String>,
    pub error: Option<String>,
    pub timestamp: u64,
}

/// Dynamic Planet & Satellite Synchronization Engine Client
#[derive(Clone)]
pub struct SyncProviderClient {
    config: SyncProviderConfig,
    http_client: reqwest::Client,
    last_etag: Arc<RwLock<Option<String>>>,
    last_summary: Arc<RwLock<Option<SyncSummary>>>,
}

impl SyncProviderClient {
    pub fn new(config: SyncProviderConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        SyncProviderClient {
            config,
            http_client,
            last_etag: Arc::new(RwLock::new(None)),
            last_summary: Arc::new(RwLock::new(None)),
        }
    }

    /// Generate SHA-256 device fingerprint from node identity
    pub fn compute_device_fingerprint(identity: &Identity) -> String {
        let mut hasher = Sha256::new();
        hasher.update(identity.address.as_bytes());
        hasher.update(identity.verifying_key.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Extract clean endpoint base URL
    pub fn get_manifest_url(&self) -> String {
        let base = self.config.server_url.trim_end_matches('/');
        if base.ends_with("/api/v1/sync") {
            format!("{}/manifest", base)
        } else if base.ends_with("/api/v1") {
            format!("{}/sync/manifest", base)
        } else {
            format!("{}/api/v1/sync/manifest", base)
        }
    }

    /// Fetch server manifest with conditional ETag and token authentication
    pub async fn fetch_manifest(
        &self,
        identity: &Identity,
    ) -> Result<(Option<Manifest>, Option<String>)> {
        let url = self.get_manifest_url();
        let fingerprint = Self::compute_device_fingerprint(identity);
        let node_id = identity.address.to_string();

        let mut req = self.http_client.get(&url)
            .header("X-ZGALAXY-Signature", &self.config.token)
            .header("X-Node-ID", &node_id)
            .header("X-Device-Fingerprint", &fingerprint);

        let cached_etag = self.last_etag.read().await.clone();
        if let Some(etag) = cached_etag {
            req = req.header("If-None-Match", etag);
        }

        let resp = req.send().await
            .with_context(|| format!("Failed to connect to ZGALAXY sync provider at {}", url))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            debug!("[ZGALAXY SYNC] Manifest unchanged (304 Not Modified)");
            return Ok((None, None));
        }

        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_else(|_| "Unknown HTTP error".to_string());
            bail!("Sync provider rejected request with status {}: {}", status, error_text);
        }

        let new_etag = resp.headers()
            .get("ETag")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());

        let manifest: Manifest = resp.json().await
            .with_context(|| "Failed to parse ZGALAXY manifest JSON response")?;

        Ok((Some(manifest), new_etag))
    }

    /// Execute atomic 5-stage sync cycle for all planet and moon assets
    pub async fn execute_sync(
        &self,
        working_dir: &Path,
        identity: &Identity,
        peer_manager: Option<&PeerManager>,
    ) -> Result<SyncSummary> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

        // 1. Fetch Manifest
        let (manifest_opt, new_etag) = match self.fetch_manifest(identity).await {
            Ok(res) => res,
            Err(e) => {
                let summary = SyncSummary {
                    status: SyncStatus::Failed(e.to_string()),
                    planets_synced: 0,
                    moons_synced: 0,
                    etag: None,
                    error: Some(e.to_string()),
                    timestamp: now,
                };
                *self.last_summary.write().await = Some(summary.clone());
                return Err(e);
            }
        };

        let manifest = match manifest_opt {
            Some(m) => m,
            None => {
                let summary = SyncSummary {
                    status: SyncStatus::Unchanged,
                    planets_synced: 0,
                    moons_synced: 0,
                    etag: self.last_etag.read().await.clone(),
                    error: None,
                    timestamp: now,
                };
                *self.last_summary.write().await = Some(summary.clone());
                return Ok(summary);
            }
        };

        // Prepare working directories: staging and backups
        let staging_dir = working_dir.join("staging");
        let backups_dir = working_dir.join("backups");
        let moons_dir = working_dir.join("moons.d");

        fs::create_dir_all(&staging_dir).await?;
        fs::create_dir_all(&backups_dir).await?;
        fs::create_dir_all(&moons_dir).await?;

        let mut planets_synced = 0;
        let mut moons_synced = 0;

        // 2. Synchronize Planets
        for item in &manifest.planets {
            match self.sync_single_asset(working_dir, &staging_dir, &backups_dir, item, WORLD_TYPE_PLANET, peer_manager).await {
                Ok(true) => planets_synced += 1,
                Ok(false) => debug!("[ZGALAXY SYNC] Planet {} already up-to-date", item.id),
                Err(e) => {
                    error!("[ZGALAXY SYNC] Failed to sync planet {}: {:?}", item.id, e);
                    if self.config.allow_auto_rollback {
                        let _ = self.rollback_planet(working_dir, &backups_dir).await;
                    }
                    bail!("Planet synchronization failed for {}: {:?}", item.id, e);
                }
            }
        }

        // 3. Synchronize Moons
        for item in &manifest.moons {
            match self.sync_single_asset(working_dir, &staging_dir, &backups_dir, item, WORLD_TYPE_MOON, peer_manager).await {
                Ok(true) => moons_synced += 1,
                Ok(false) => debug!("[ZGALAXY SYNC] Moon {} already up-to-date", item.id),
                Err(e) => {
                    error!("[ZGALAXY SYNC] Failed to sync moon {}: {:?}", item.id, e);
                    bail!("Moon synchronization failed for {}: {:?}", item.id, e);
                }
            }
        }

        // Update ETag and summary
        if let Some(etag) = new_etag.clone() {
            *self.last_etag.write().await = Some(etag);
        }

        let summary = SyncSummary {
            status: SyncStatus::Synced,
            planets_synced,
            moons_synced,
            etag: new_etag,
            error: None,
            timestamp: now,
        };
        *self.last_summary.write().await = Some(summary.clone());
        info!("[ZGALAXY SYNC] Successfully synchronized {} planets, {} moons", planets_synced, moons_synced);

        Ok(summary)
    }

    /// Synchronize and validate a single binary asset (planet or moon)
    async fn sync_single_asset(
        &self,
        working_dir: &Path,
        staging_dir: &Path,
        backups_dir: &Path,
        item: &TopologyItem,
        expected_world_type: u8,
        peer_manager: Option<&PeerManager>,
    ) -> Result<bool> {
        let target_path = if expected_world_type == WORLD_TYPE_PLANET {
            working_dir.join("planet")
        } else {
            working_dir.join("moons.d").join(&item.filename)
        };

        // Check if existing target already matches expected sha256
        if target_path.exists() {
            if let Ok(existing_bytes) = fs::read(&target_path).await {
                let mut hasher = Sha256::new();
                hasher.update(&existing_bytes);
                let current_hash = hex::encode(hasher.finalize());
                if current_hash.eq_ignore_ascii_case(&item.sha256) {
                    return Ok(false); // Already latest version
                }
            }
        }

        // Stage 1: Download to staging
        let download_url = if item.download_url.starts_with("http://") || item.download_url.starts_with("https://") {
            item.download_url.clone()
        } else {
            let base = self.config.server_url.trim_end_matches('/');
            format!("{}/{}", base, item.download_url.trim_start_matches('/'))
        };

        let temp_staged_path = staging_dir.join(format!("{}.tmp", item.filename));
        let resp = self.http_client.get(&download_url)
            .header("X-ZGALAXY-Signature", &self.config.token)
            .send().await
            .with_context(|| format!("Failed to download topology file from {}", download_url))?;

        if !resp.status().is_success() {
            bail!("Download of {} failed with status {}", item.filename, resp.status());
        }

        let bytes = resp.bytes().await?;

        // Stage 2: Cryptographic Checksum Validation
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let downloaded_hash = hex::encode(hasher.finalize());
        if !downloaded_hash.eq_ignore_ascii_case(&item.sha256) {
            bail!("SHA-256 checksum mismatch for {}: expected {}, got {}", item.filename, item.sha256, downloaded_hash);
        }

        // Stage 3: ZeroTier / ZGALAXY Binary Structure Validation
        let parsed_world = World::parse_binary(&bytes)
            .with_context(|| format!("Failed binary structure validation for {}", item.filename))?;

        if parsed_world.roots.is_empty() {
            bail!("Parsed topology for {} has no valid roots — rejecting", item.filename);
        }

        fs::write(&temp_staged_path, &bytes).await?;

        // Stage 4: Backup existing file
        if target_path.exists() {
            let backup_file = backups_dir.join(format!("{}.bak", item.filename));
            let _ = fs::copy(&target_path, &backup_file).await;
        }

        // Stage 5: Atomic Swap
        fs::rename(&temp_staged_path, &target_path).await
            .with_context(|| format!("Atomic rename failed for {}", item.filename))?;

        // Stage 6: Inject newly discovered roots into PeerManager in-memory
        if let Some(pm) = peer_manager {
            let role = if expected_world_type == WORLD_TYPE_PLANET {
                PeerRole::Planet
            } else {
                PeerRole::Moon
            };

            for root in &parsed_world.roots {
                for ep_str in &root.stable_endpoints {
                    let cleaned = ep_str.replace('/', ":");
                    if let Ok(sock) = cleaned.parse() {
                        pm.add_or_update_peer(root.identity, role, sock, 10).await;
                    }
                }
            }
        }

        Ok(true)
    }

    /// Rollback planet to last known good backup
    pub async fn rollback_planet(&self, working_dir: &Path, backups_dir: &Path) -> Result<()> {
        let backup_file = backups_dir.join("planet.bak");
        let target_file = working_dir.join("planet");

        if backup_file.exists() {
            fs::copy(&backup_file, &target_file).await?;
            warn!("[ZGALAXY SYNC] Self-healing rollback restored planet from {:?}", backup_file);
            Ok(())
        } else {
            bail!("No backup planet available for rollback");
        }
    }

    pub async fn get_last_summary(&self) -> Option<SyncSummary> {
        self.last_summary.read().await.clone()
    }
}
