//! ZGALAXY Desktop Client — Rust Core (P0 skeleton).
//!
//! Architecture per the UI specification:
//!   Slint UI  ← state/commands →  Rust Core  → local daemon REST API
//! No network logic lives in the UI; all I/O happens on a background thread
//! and results are pushed into the UI via the slint event loop.

slint::include_modules!();

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Connection to the local zgalaxy-rs daemon (local REST API contract).
struct DaemonClient {
    base: String,
    token: String,
}

impl DaemonClient {
    fn from_env() -> Self {
        let port = std::env::var("ZGALAXY_PORT").unwrap_or_else(|_| "9993".into());
        let token = std::env::var("ZGALAXY_TOKEN")
            .ok()
            .or_else(|| std::fs::read_to_string("/var/lib/zerotier-one/authtoken.secret").ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        DaemonClient {
            base: format!("http://127.0.0.1:{}", port),
            token,
        }
    }

    fn get(&self, path: &str) -> Option<serde_json::Value> {
        ureq::get(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &self.token)
            .timeout(Duration::from_secs(3))
            .call()
            .ok()?
            .into_json()
            .ok()
    }

    fn post(&self, path: &str) -> Option<serde_json::Value> {
        ureq::post(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &self.token)
            .timeout(Duration::from_secs(5))
            .call()
            .ok()?
            .into_json()
            .ok()
    }

    fn delete(&self, path: &str) -> Option<serde_json::Value> {
        ureq::delete(&format!("{}{}", self.base, path))
            .set("X-ZT1-Auth", &self.token)
            .timeout(Duration::from_secs(5))
            .call()
            .ok()?
            .into_json()
            .ok()
    }
}

fn main() {
    let app = MainWindow::new().unwrap();
    let client = Arc::new(DaemonClient::from_env());
    let busy: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    // --- Commands ---
    let weak = app.as_weak();
    let client_join = Arc::clone(&client);
    let busy_join = Arc::clone(&busy);
    app.on_join_network(move |nwid| {
        let nwid = nwid.trim().to_string();
        if nwid.is_empty() {
            return;
        }
        *busy_join.lock().unwrap() = true;
        let client = Arc::clone(&client_join);
        std::thread::spawn(move || {
            let _ = client.post(&format!("/network/{}", nwid.to_lowercase()));
            std::thread::sleep(Duration::from_millis(400));
        });
        if let Some(app) = weak.upgrade() {
            app.set_join_input("".into());
        }
    });

    let weak = app.as_weak();
    let client_leave = Arc::clone(&client);
    app.on_leave_network(move |nwid| {
        let _ = client_leave.delete(&format!("/network/{}", nwid));
        if let Some(app) = weak.upgrade() {
            app.set_busy(true);
        }
        std::thread::sleep(Duration::from_millis(200));
    });

    let client_refresh = Arc::clone(&client);
    app.on_refresh(move || {
        let _ = client_refresh.get("/status");
    });

    // --- Background poller: daemon → app state (every 2s) ---
    let weak = app.as_weak();
    let poll_client = Arc::clone(&client);
    std::thread::spawn(move || loop {
        let status_ok = poll_client.get("/status");
        let networks = poll_client.get("/network");
        if let Some(ui) = weak.upgrade() {
            match (&status_ok, &networks) {
                (Some(st), Some(nets)) => {
                    let online = st.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
                    ui.set_service_state(if online { "Online".into() } else { "Offline".into() });
                    ui.set_node_address(
                        st.get("address").and_then(|v| v.as_str()).unwrap_or("?").into(),
                    );
                    let mut list = Vec::new();
                    if let Some(arr) = nets.as_array() {
                        for n in arr {
                            let status = n
                                .get("status")
                                .and_then(|v| v.as_str())
                                .unwrap_or("REQUESTING_CONFIGURATION");
                            let addr = n
                                .get("assignedAddresses")
                                .and_then(|v| v.as_array())
                                .and_then(|a| a.first())
                                .and_then(|v| v.as_str())
                                .unwrap_or("no address yet");
                            list.push(NetworkInfo {
                                name: n.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed").into(),
                                nwid: n.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                                status: status.into(),
                                address: addr.into(),
                                ready: status == "OK",
                            });
                        }
                    }
                    let any_ready = list.iter().any(|n| n.ready);
                    let is_empty = list.is_empty();
                    ui.set_networks(slint::ModelRc::new(slint::VecModel::from(list)));
                    if is_empty {
                        ui.set_status_line("Not connected".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(0x9a, 0x9a, 0x9a));
                    } else if any_ready {
                        ui.set_status_line("Ready to play".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(0x1a, 0x7f, 0x37));
                    } else {
                        ui.set_status_line("Connecting…".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(0xb8, 0x86, 0x0b));
                    }
                    ui.set_busy(false);
                }
                _ => {
                    ui.set_service_state("Service unreachable".into());
                    ui.set_status_line("Connection problem".into());
                    ui.set_status_color(slint::Color::from_rgb_u8(0xc6, 0x28, 0x28));
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    });

    app.run().unwrap();
}
