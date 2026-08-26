//! User preferences persistence (spec §11.1): storage is separate from UI;
//! the UI only ever sees a plain Config value.

#[derive(Clone)]
pub struct Config {
    pub start_with_system: bool,
    pub start_in_background: bool,
    pub show_tray_icon: bool,
    pub notify_status: bool,
    pub sync_domain: String,
    pub sync_token: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start_with_system: false,
            start_in_background: false,
            show_tray_icon: true,
            notify_status: true,
            sync_domain: String::new(),
            sync_token: String::new(),
        }
    }
}

fn base_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var("APPDATA").map(std::path::PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config"));
    base.unwrap_or_else(|_| std::env::temp_dir()).join("zgalaxy-ui")
}

impl Config {
    pub fn load() -> Self {
        let path = base_dir().join("settings.json");
        let defaults = Config::default();
        let v: serde_json::Value = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let b = |k: &str, d: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(d);
        let s = |k: &str, d: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or(d).to_string();
        Config {
            start_with_system: b("start_with_system", defaults.start_with_system),
            start_in_background: b("start_in_background", defaults.start_in_background),
            show_tray_icon: b("show_tray_icon", defaults.show_tray_icon),
            notify_status: b("notify_status", defaults.notify_status),
            sync_domain: s("sync_domain", &defaults.sync_domain),
            sync_token: s("sync_token", &defaults.sync_token),
        }
    }

    pub fn save(&self) {
        let dir = base_dir();
        let _ = std::fs::create_dir_all(&dir);
        let v = serde_json::json!({
            "start_with_system": self.start_with_system,
            "start_in_background": self.start_in_background,
            "show_tray_icon": self.show_tray_icon,
            "notify_status": self.notify_status,
            "sync_domain": self.sync_domain,
            "sync_token": self.sync_token,
        });
        let _ = std::fs::write(dir.join("settings.json"), v.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_recommendations() {
        let c = Config::default();
        assert!(c.show_tray_icon, "tray-first app shows its icon");
        assert!(c.notify_status);
        assert!(!c.start_with_system);
    }
}
