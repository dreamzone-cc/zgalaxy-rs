use std::collections::HashMap;
use std::path::Path;
use tokio::fs;
use serde::{Serialize, Deserialize};
use serde_json::Value;
use tracing::info;
use anyhow::Result;

/// Local node configuration loaded from `local.conf` and `networks.d/`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_management_from")]
    pub allow_management_from: Vec<String>,
    #[serde(default)]
    pub controller_enabled: bool,
    #[serde(default)]
    pub interface_blacklist: Vec<String>,
    #[serde(default)]
    pub interface_whitelist: Vec<String>,
    #[serde(default)]
    pub auto_join_networks: Vec<String>,
    #[serde(default)]
    pub sync_provider: Option<crate::sync_provider::SyncProviderConfig>,
    #[serde(default)]
    pub physical: HashMap<String, Value>,
    #[serde(default)]
    pub settings: HashMap<String, Value>,
}

fn default_port() -> u16 {
    9993
}

fn default_management_from() -> Vec<String> {
    vec!["127.0.0.1".to_string(), "::1".to_string()]
}

impl Default for LocalConfig {
    fn default() -> Self {
        LocalConfig {
            port: 9993,
            allow_management_from: vec!["127.0.0.1".to_string(), "::1".to_string()],
            controller_enabled: false,
            interface_blacklist: Vec::new(),
            interface_whitelist: Vec::new(),
            auto_join_networks: Vec::new(),
            sync_provider: None,
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

        // Canonical ZeroTier settings.controller or settings.allowController
        if let Some(ctrl) = cfg.settings.get("controller").or_else(|| cfg.settings.get("allowController")).and_then(|v| v.as_bool()) {
            cfg.controller_enabled = ctrl;
        }

        // Environment variables: ZGALAXY_CONTROLLER=1, ZGALAXY_MODE=controller, ZEROTIER_CONTROLLER=1
        if let Ok(env_ctrl) = std::env::var("ZGALAXY_CONTROLLER") {
            cfg.controller_enabled = env_ctrl == "1" || env_ctrl.eq_ignore_ascii_case("true");
        } else if let Ok(env_mode) = std::env::var("ZGALAXY_MODE") {
            cfg.controller_enabled = env_mode.eq_ignore_ascii_case("controller");
        } else if let Ok(env_zt_ctrl) = std::env::var("ZEROTIER_CONTROLLER") {
            cfg.controller_enabled = env_zt_ctrl == "1" || env_zt_ctrl.eq_ignore_ascii_case("true");
        }

        // Canonical ZeroTier local.conf carries the port under
        // settings.primaryPort — honor it when the top-level field is absent.
        if let Some(pp) = cfg.settings.get("primaryPort").and_then(|v| v.as_u64()) {
            if let Ok(port) = u16::try_from(pp) {
                cfg.port = port;
            }
        }

        // Canonical ZeroTier local.conf likewise carries the management
        // allow-list under settings.allowManagementFrom. The top-level serde
        // field only matches non-standard files, so mirror the primaryPort
        // handling: lift the list out of `settings` when present. Without
        // this, the guard silently falls back to loopback-only and refuses
        // legitimate callers such as ztnet (observed live on the production
        // container: "management access from 172.31.255.3 refused").
        if let Some(list) = cfg
            .settings
            .get("allowManagementFrom")
            .and_then(|v| v.as_array())
        {
            let entries: Vec<String> = list
                .iter()
                .filter_map(|x| x.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from))
                .collect();
            if !entries.is_empty() {
                cfg.allow_management_from = entries;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canonical_zerotier_local_conf_lifts_allow_management_from_and_port() {
        let dir = std::env::temp_dir().join(format!("zgrs-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Exactly the shape used on the production container.
        std::fs::write(
            dir.join("local.conf"),
            r#"{"settings":{"primaryPort":9993,"portMappingEnabled":true,"softwareUpdate":"disable","allowManagementFrom":["172.31.255.0/29"],"allowTcpFallbackRelay":true}}"#,
        )
        .unwrap();

        let cfg = LocalConfig::load(&dir).await;
        assert_eq!(cfg.port, 9993, "primaryPort must lift out of settings");
        assert!(
            cfg.allow_management_from.contains(&"172.31.255.0/29".to_string()),
            "settings.allowManagementFrom must be honored, got {:?}",
            cfg.allow_management_from
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_local_conf_defaults_to_loopback_only() {
        let dir = std::env::temp_dir().join(format!("zgrs-cfg-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = LocalConfig::load(&dir).await;
        assert!(cfg.allow_management_from.contains(&"127.0.0.1".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn empty_settings_list_keeps_existing_values() {
        let dir = std::env::temp_dir().join(format!("zgrs-cfg-blank-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("local.conf"),
            r#"{"settings":{"allowManagementFrom":[]}}"#,
        )
        .unwrap();
        let cfg = LocalConfig::load(&dir).await;
        assert!(
            cfg.allow_management_from.contains(&"127.0.0.1".to_string()),
            "an empty list must not wipe the safe default"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
