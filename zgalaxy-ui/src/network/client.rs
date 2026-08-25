//! Local daemon REST client (spec §12): the only network-speaking layer.
//! Token comes from env or the daemon's secret file and never touches UI
//! files or logs.

use std::time::Duration;

pub struct DaemonClient {
    base: String,
    token: String,
}

impl DaemonClient {
    pub fn from_env() -> Self {
        let port = std::env::var("ZGALAXY_PORT").unwrap_or_else(|_| "9993".into());
        DaemonClient {
            base: format!("http://127.0.0.1:{}", port),
            token: Self::load_token(),
        }
    }

    fn load_token() -> String {
        let home_token = std::env::var("HOME").ok().and_then(|h| {
            std::fs::read_to_string(format!("{}/.zeroTierOneAuthToken", h)).ok()
                .or_else(|| std::fs::read_to_string(format!("{}/.config/zgalaxy-ui/authtoken.secret", h)).ok())
        });
        std::env::var("ZGALAXY_TOKEN")
            .ok()
            .or_else(|| std::fs::read_to_string("/var/lib/zerotier-one/authtoken.secret").ok())
            .or(home_token)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    fn current_token(&self) -> String {
        if !self.token.is_empty() {
            self.token.clone()
        } else {
            Self::load_token()
        }
    }

    fn get(&self, path: &str) -> Option<serde_json::Value> {
        let token = self.current_token();
        ureq::get(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &token)
            .timeout(Duration::from_secs(3))
            .call()
            .ok()?
            .into_json()
            .ok()
    }

    fn post(&self, path: &str) -> Option<serde_json::Value> {
        let token = self.current_token();
        ureq::post(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &token)
            .timeout(Duration::from_secs(5))
            .call()
            .ok()?
            .into_json()
            .ok()
    }

    fn post_json(&self, path: &str, body: &serde_json::Value) -> Option<serde_json::Value> {
        let token = self.current_token();
        ureq::post(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &token)
            .set("Content-Type", "application/json")
            .timeout(Duration::from_secs(5))
            .send_json(body.clone())
            .ok()?
            .into_json()
            .ok()
    }

    fn delete(&self, path: &str) -> Option<serde_json::Value> {
        let token = self.current_token();
        ureq::delete(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &token)
            .timeout(Duration::from_secs(5))
            .call()
            .ok()?
            .into_json()
            .ok()
    }
}

/// One full poll cycle result (plain data, Send — safe to hand to the UI
/// thread through the event loop).
pub struct PollResult {
    pub service_online: Option<bool>,
    pub node_id: Option<String>,
    pub nets: Vec<serde_json::Value>,
    pub peers: Vec<serde_json::Value>,
}

impl DaemonClient {
    pub fn poll(&self) -> PollResult {
        let status = self.get("/status");
        let networks = self.get("/network");
        let peers = self.get("/peer");
        PollResult {
            service_online: status.as_ref().and_then(|v| v.get("online")).and_then(|v| v.as_bool()),
            node_id: status
                .as_ref()
                .and_then(|v| v.get("address"))
                .and_then(|v| v.as_str())
                .map(String::from),
            nets: networks
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            peers: peers
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.base
    }

    pub fn post_network(&self, nwid: &str) {
        let _ = self.post(&format!("/network/{}", nwid));
    }

    pub fn delete_network(&self, nwid: &str) {
        let _ = self.delete(&format!("/network/{}", nwid));
    }

    pub fn post_network_settings(&self, nwid: &str, body: &serde_json::Value) {
        let _ = self.post_json(&format!("/network/{}", nwid), body);
    }
}
