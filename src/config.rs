use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use serde::{Serialize, Deserialize};
use serde_json::Value;
use tracing::{info, warn};
use anyhow::Result;

/// Local node configuration loaded from `local.conf` and `networks.d/`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalConfig {
    pub port: u16,
    pub allow_management_from: Vec<String>,
    pub interface_blacklist: Vec<String>,
    pub interface_whitelist: Vec<String>,
    pub auto_join_networks: Vec<String>,
    pub physical: HashMap<String, Value>,
    pub settings: HashMap<String, Value>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        LocalConfig {
            port: 9993,
            allow_management_from: vec!["127.0.0.1".to_string(), "::1".to_string()],
            interface_blacklist: Vec::new(),
            interface_whitelist: Vec::new(),
            auto_join_networks: Vec::new(),
            physical: HashMap::new(),
            settings: HashMap::new(),
        }
    }
}

impl LocalConfig {
    /// Load `local.conf` and scan `networks.d/` directory for auto-join networks.
    pub async fn load(working_dir: &Path) -> Self {
        let mut cfg = LocalConfig::default();
        let conf_file = working_dir.join("local.conf");

        if conf_file.exists() {
            if let Ok(content) = fs::read_to_string(&conf_file).await {
                if let Ok(parsed) = serde_json::from_str::<LocalConfig>(&content) {
                    cfg = parsed;
                    info!("[ZGALAXY CONFIG] Loaded local.conf from {:?}", conf_file);
                }
            }
        }

        // Scan networks.d/ directory for configured networks
        let networks_dir = working_dir.join("networks.d");
        if networks_dir.exists() {
            if let Ok(mut entries) = fs::read_dir(&networks_dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("conf") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            let clean = stem.trim().to_lowercase();
                            if clean.len() == 16 && !cfg.auto_join_networks.contains(&clean) {
                                cfg.auto_join_networks.push(clean);
                            }
                        }
                    }
                }
            }
        }

        cfg
    }

    /// Save an auto-joined network into `networks.d/<nwid>.conf`
    pub async fn persist_network_join(working_dir: &Path, nwid: &str) -> Result<()> {
        let networks_dir = working_dir.join("networks.d");
        fs::create_dir_all(&networks_dir).await?;
        let conf_file = networks_dir.join(format!("{}.conf", nwid.trim().to_lowercase()));
        fs::write(&conf_file, b"").await?;
        Ok(())
    }

    /// Remove an auto-joined network from `networks.d/<nwid>.conf`
    pub async fn persist_network_leave(working_dir: &Path, nwid: &str) -> Result<()> {
        let conf_file = working_dir.join("networks.d").join(format!("{}.conf", nwid.trim().to_lowercase()));
        if conf_file.exists() {
            let _ = fs::remove_file(conf_file).await;
        }
        Ok(())
    }
}
