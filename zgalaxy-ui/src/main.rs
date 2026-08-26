//! ZGALAXY Desktop Client — Unified Single-Window Native Interface.

slint::include_modules!();

mod app;
mod network;
mod platform;
mod storage;

use app::state::{NetworkState, OverallState, ServiceHealth};
use network::client::DaemonClient;
use slint::ComponentHandle;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use storage::Config;

fn present(w: &impl slint::ComponentHandle) {
    let _ = w.show();
}

fn s<'a>(v: &'a serde_json::Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn net_state(n: &serde_json::Value) -> NetworkState {
    NetworkState::from_status(s(n, "status"))
}

fn net_info(n: &serde_json::Value) -> NetworkInfo {
    let state = net_state(n);
    NetworkInfo {
        name: s(n, "name").into(),
        nwid: s(n, "id").into(),
        address: n
            .get("assignedAddresses")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("no address yet")
            .into(),
        state_label: state.label().into(),
        state_emoji: state.emoji().into(),
        state_kind: state.kind().into(),
        ready: state == NetworkState::Connected,
    }
}

fn peer_infos(peers: &[serde_json::Value]) -> Vec<PeerInfo> {
    peers
        .iter()
        .map(|p| {
            let addr = s(p, "address");
            let role = s(p, "role");
            let latency_ms = p.get("latency").and_then(|v| v.as_i64()).unwrap_or(-1);
            let paths = p.get("paths").and_then(|v| v.as_array());
            let active = paths
                .map(|a| a.iter().any(|path| path.get("active").and_then(|x| x.as_bool()).unwrap_or(false)))
                .unwrap_or(false);
            let latency_str = if latency_ms >= 0 {
                format!("{} ms", latency_ms)
            } else {
                "—".to_string()
            };
            PeerInfo {
                address: addr.into(),
                role: role.into(),
                latency: latency_str.into(),
                active,
            }
        })
        .collect()
}

fn fill_details(ui: &MainWindow, snap: &[serde_json::Value], nwid: &str) {
    let clean = nwid.trim().to_lowercase();
    let found = snap.iter().find(|n| s(n, "id").eq_ignore_ascii_case(&clean));
    let Some(n) = found else { return };
    let state = net_state(n);
    let addr = n
        .get("assignedAddresses")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("");
    ui.set_d_nwid(s(n, "id").into());
    ui.set_d_name(s(n, "name").into());
    ui.set_d_state_emoji(state.emoji().into());
    ui.set_d_state_label(state.label().into());
    ui.set_d_state_kind(state.kind().into());
    ui.set_d_address(addr.into());
    ui.set_d_connected(state == NetworkState::Connected);
}

fn fill_advanced(ui: &MainWindow, snap: &[serde_json::Value], nwid: &str) {
    let clean = nwid.trim().to_lowercase();
    let found = snap.iter().find(|n| s(n, "id").eq_ignore_ascii_case(&clean));
    let Some(n) = found else { return };
    let state = net_state(n);
    let addr = n
        .get("assignedAddresses")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let routes = n
        .get("routes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("target").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let dns = n
        .get("dns")
        .and_then(|d| d.get("domain"))
        .and_then(|dom| dom.as_str())
        .unwrap_or("");
    let gb = |k: &str| n.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    ui.set_a_name(s(n, "name").into());
    ui.set_a_nwid(s(n, "id").into());
    ui.set_a_state_emoji(state.emoji().into());
    ui.set_a_state_label(state.label().into());
    ui.set_a_address(if addr.is_empty() { "—".into() } else { addr.into() });
    ui.set_a_device(if s(n, "portDeviceName").is_empty() { "—".into() } else { s(n, "portDeviceName").into() });
    ui.set_a_mac(if s(n, "mac").is_empty() { "—".into() } else { s(n, "mac").into() });
    ui.set_a_type(s(n, "type").into());
    ui.set_a_routes(if routes.is_empty() { "—".into() } else { routes.into() });
    ui.set_a_dns(if dns.is_empty() { "—".into() } else { dns.into() });
    ui.set_a_allow_managed(gb("allowManaged"));
    ui.set_a_allow_global(gb("allowGlobal"));
    ui.set_a_allow_default(gb("allowDefault"));
    ui.set_a_allow_dns(gb("allowDNS"));
}

fn main() {
    let config = Config::load();

    let main_w = MainWindow::new().unwrap();
    let tray = if config.show_tray_icon {
        Some(AppTray::new().unwrap())
    } else {
        None
    };

    // Initial Static / Preferences Setup
    main_w.set_app_version(env!("CARGO_PKG_VERSION").into());
    main_w.set_p_start_with_system(config.start_with_system);
    main_w.set_p_start_background(config.start_in_background);
    main_w.set_p_show_tray(config.show_tray_icon);
    main_w.set_p_notify_status(config.notify_status);
    main_w.set_sync_domain(config.sync_domain.clone().into());
    main_w.set_sync_token(config.sync_token.clone().into());
    if !config.sync_domain.is_empty() && !config.sync_token.is_empty() {
        main_w.set_sync_status_label("Configured (Ready to sync)".into());
        main_w.set_sync_status_emoji("🟢".into());
        main_w.set_sync_status_kind("success".into());
    }

    let client = Arc::new(DaemonClient::from_env());
    main_w.set_adv_endpoint(client.endpoint().into());

    let nets_json: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let selected_nwid: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let poll_now = Arc::new(AtomicBool::new(false));

    // --- Clipboard ---
    main_w.on_copy_text(|text| platform::copy_to_clipboard(&text));

    // --- View Navigation & Dialogs ---
    let mw_join = main_w.as_weak();
    main_w.on_open_join(move || {
        if let Some(ui) = mw_join.upgrade() {
            ui.set_join_error_text("".into());
            ui.set_join_input("".into());
            ui.set_show_join_modal(true);
        }
    });

    let mw_close_join = main_w.as_weak();
    main_w.on_close_join(move || {
        if let Some(ui) = mw_close_join.upgrade() {
            ui.set_show_join_modal(false);
        }
    });

    let c = Arc::clone(&client);
    let join_flag = Arc::clone(&poll_now);
    let mw_req_join = main_w.as_weak();
    main_w.on_request_join(move |nwid| {
        let nwid = nwid.trim().to_string();
        if nwid.is_empty() {
            return;
        }
        if !app::commands::is_valid_nwid(&nwid) {
            if let Some(ui) = mw_req_join.upgrade() {
                ui.set_join_error_text(
                    "Invalid Network ID — must be exactly 16 hex characters (e.g. ef313fb5c9bc842c)".into(),
                );
            }
            return;
        }
        if let Some(ui) = mw_req_join.upgrade() {
            ui.set_join_error_text("".into());
            ui.set_join_input("".into());
            ui.set_show_join_modal(false);
        }
        app::commands::join(Arc::clone(&c), nwid.to_lowercase(), Some(Arc::clone(&join_flag)));
    });

    let mw_det = main_w.as_weak();
    let sel_det = Arc::clone(&selected_nwid);
    let cache_det = Arc::clone(&nets_json);
    main_w.on_open_details(move |nwid| {
        let nwid = nwid.trim().to_string();
        if nwid.is_empty() {
            return;
        }
        *sel_det.lock().unwrap() = Some(nwid.clone());
        if let Some(ui) = mw_det.upgrade() {
            fill_details(&ui, &cache_det.lock().unwrap(), &nwid);
            ui.set_current_view("details".into());
        }
    });

    let mw_adv = main_w.as_weak();
    let sel_adv = Arc::clone(&selected_nwid);
    let cache_adv = Arc::clone(&nets_json);
    main_w.on_open_advanced(move || {
        if let Some(nwid) = sel_adv.lock().unwrap().clone() {
            if let Some(ui) = mw_adv.upgrade() {
                fill_advanced(&ui, &cache_adv.lock().unwrap(), &nwid);
                ui.set_current_view("advanced".into());
            }
        }
    });

    let mw_back_main = main_w.as_weak();
    main_w.on_back_to_main(move || {
        if let Some(ui) = mw_back_main.upgrade() {
            ui.set_current_view("main".into());
        }
    });

    let mw_back_det = main_w.as_weak();
    main_w.on_back_to_details(move || {
        if let Some(ui) = mw_back_det.upgrade() {
            ui.set_current_view("details".into());
        }
    });

    let mw_prefs = main_w.as_weak();
    main_w.on_open_prefs(move || {
        if let Some(ui) = mw_prefs.upgrade() {
            ui.set_current_view("preferences".into());
        }
    });

    let mw_about = main_w.as_weak();
    main_w.on_open_about(move || {
        if let Some(ui) = mw_about.upgrade() {
            ui.set_current_view("about".into());
        }
    });

    let now_flag = Arc::clone(&poll_now);
    main_w.on_retry(move || {
        now_flag.store(true, Ordering::Relaxed);
    });

    let c = Arc::clone(&client);
    let leave_flag = Arc::clone(&poll_now);
    let mw_leave = main_w.as_weak();
    main_w.on_leave_network(move |nwid| {
        let nwid = nwid.trim().to_string();
        if !nwid.is_empty() {
            app::commands::leave(Arc::clone(&c), nwid, Some(Arc::clone(&leave_flag)));
            if let Some(ui) = mw_leave.upgrade() {
                ui.set_current_view("main".into());
            }
        }
    });

    let c = Arc::clone(&client);
    let disc_flag = Arc::clone(&poll_now);
    let sel_disc = Arc::clone(&selected_nwid);
    let mw_disc = main_w.as_weak();
    main_w.on_disconnect_current(move || {
        if let Some(nwid) = sel_disc.lock().unwrap().clone() {
            app::commands::leave(Arc::clone(&c), nwid, Some(Arc::clone(&disc_flag)));
            if let Some(ui) = mw_disc.upgrade() {
                ui.set_current_view("main".into());
            }
        }
    });

    let last_local_edit_ms = Arc::new(AtomicU64::new(0));
    let mw_flags = main_w.as_weak();
    let client_flags = Arc::clone(&client);
    let sel_flags = Arc::clone(&selected_nwid);
    let adv_flag = Arc::clone(&poll_now);
    let edit_stamp = Arc::clone(&last_local_edit_ms);
    main_w.on_advanced_flags_changed(move || {
        let Some(ui) = mw_flags.upgrade() else { return };
        let Some(nwid) = sel_flags.lock().unwrap().clone() else { return };
        let body = serde_json::json!({
            "allowManaged": ui.get_a_allow_managed(),
            "allowGlobal": ui.get_a_allow_global(),
            "allowDefault": ui.get_a_allow_default(),
            "allowDNS": ui.get_a_allow_dns(),
        });
        edit_stamp.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            Ordering::Relaxed,
        );
        app::commands::update_flags(Arc::clone(&client_flags), nwid, body, Some(Arc::clone(&adv_flag)));
    });

    let cfg_store = Arc::new(Mutex::new(config.clone()));
    let pw = main_w.as_weak();
    let cfg_ref = Arc::clone(&cfg_store);
    main_w.on_prefs_changed(move || {
        let Some(p) = pw.upgrade() else { return };
        let mut cur = cfg_ref.lock().unwrap().clone();
        cur.start_with_system = p.get_p_start_with_system();
        cur.start_in_background = p.get_p_start_background();
        cur.show_tray_icon = p.get_p_show_tray();
        cur.notify_status = p.get_p_notify_status();
        cur.sync_domain = p.get_sync_domain().trim().to_string();
        cur.sync_token = p.get_sync_token().trim().to_string();
        cur.save();
        platform::set_autostart(cur.start_with_system);
        *cfg_ref.lock().unwrap() = cur;
    });

    let mw_sync = main_w.as_weak();
    let cfg_sync_ref = Arc::clone(&cfg_store);
    main_w.on_sync_now(move |domain, token| {
        let domain = domain.trim().to_string();
        let token = token.trim().to_string();
        if let Some(ui) = mw_sync.upgrade() {
            if domain.is_empty() || token.is_empty() {
                ui.set_sync_status_label("Domain and Token are required".into());
                ui.set_sync_status_emoji("⚠️".into());
                ui.set_sync_status_kind("warning".into());
                return;
            }
            ui.set_sync_busy(true);
            ui.set_sync_status_label("Connecting & verifying token…".into());
            ui.set_sync_status_emoji("🟡".into());
            ui.set_sync_status_kind("warning".into());

            // Save configured sync domain & token
            let mut cur = cfg_sync_ref.lock().unwrap().clone();
            cur.sync_domain = domain.clone();
            cur.sync_token = token.clone();
            cur.save();
            *cfg_sync_ref.lock().unwrap() = cur;
        }

        let mw_cb = mw_sync.clone();
        std::thread::spawn(move || {
            let base_url = if domain.starts_with("http://") || domain.starts_with("https://") {
                domain.clone()
            } else {
                format!("http://{}:3000", domain)
            };
            let manifest_url = format!("{}/api/v1/sync/manifest", base_url.trim_end_matches('/'));

            let resp = ureq::get(&manifest_url)
                .set("X-ZGALAXY-Signature", &token)
                .timeout(std::time::Duration::from_secs(10))
                .call();

            let (label, emoji, kind, topology, now_str) = match resp {
                Ok(response) => {
                    if let Ok(val) = response.into_json::<serde_json::Value>() {
                        let planets_cnt = val.get("planets").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(1);
                        let moons_cnt = val.get("moons").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                        let topo = format!("{} Planet · {} Moons", planets_cnt, moons_cnt);
                        ("Synced & Verified".to_string(), "🟢".to_string(), "success".to_string(), topo, "Just now".to_string())
                    } else {
                        ("Connected (Malformed Response)".to_string(), "⚠️".to_string(), "warning".to_string(), "1 Planet · 0 Moons".to_string(), "Just now".to_string())
                    }
                }
                Err(e) => {
                    let err_str = match e {
                        ureq::Error::Status(code, _) => format!("Rejected (HTTP {})", code),
                        ureq::Error::Transport(t) => format!("Connection failed: {}", t.kind()),
                    };
                    (err_str, "🔴".to_string(), "error".to_string(), "—".to_string(), "Failed".to_string())
                }
            };

            slint::invoke_from_event_loop(move || {
                if let Some(ui) = mw_cb.upgrade() {
                    ui.set_sync_busy(false);
                    ui.set_sync_status_label(label.into());
                    ui.set_sync_status_emoji(emoji.into());
                    ui.set_sync_status_kind(kind.into());
                    ui.set_sync_topology_info(topology.into());
                    ui.set_sync_last_time(now_str.into());
                }
            }).ok();
        });
    });

    let mw_remove_sync = main_w.as_weak();
    let cfg_remove_ref = Arc::clone(&cfg_store);
    main_w.on_remove_sync_provider(move || {
        if let Some(ui) = mw_remove_sync.upgrade() {
            ui.set_sync_domain("".into());
            ui.set_sync_token("".into());
            ui.set_sync_status_label("Not configured (Local)".into());
            ui.set_sync_status_emoji("⚪".into());
            ui.set_sync_status_kind("neutral".into());
            ui.set_sync_last_time("Never".into());
            ui.set_sync_topology_info("1 Planet · 0 Moons".into());

            let mut cur = cfg_remove_ref.lock().unwrap().clone();
            cur.sync_domain.clear();
            cur.sync_token.clear();
            cur.save();
            *cfg_remove_ref.lock().unwrap() = cur;
        }
    });

    // --- System Tray Actions ---
    let mw_tray_open = main_w.as_weak();
    if let Some(tray) = &tray {
        tray.on_open_main(move || {
            if let Some(m) = mw_tray_open.upgrade() {
                m.set_current_view("main".into());
                m.set_show_join_modal(false);
                present(&m);
            }
        });
    }

    let mw_tray_join = main_w.as_weak();
    if let Some(tray) = &tray {
        tray.on_open_join(move || {
            if let Some(m) = mw_tray_join.upgrade() {
                m.set_current_view("main".into());
                m.set_join_error_text("".into());
                m.set_join_input("".into());
                m.set_show_join_modal(true);
                present(&m);
            }
        });
    }

    let sel_tray = Arc::clone(&selected_nwid);
    let cache_tray = Arc::clone(&nets_json);
    let mw_tray_net = main_w.as_weak();
    if let Some(tray) = &tray {
        tray.on_open_network(move |nwid| {
            let nwid = nwid.trim().to_string();
            if nwid.is_empty() {
                return;
            }
            *sel_tray.lock().unwrap() = Some(nwid.clone());
            if let Some(m) = mw_tray_net.upgrade() {
                fill_details(&m, &cache_tray.lock().unwrap(), &nwid);
                m.set_current_view("details".into());
                present(&m);
            }
        });
    }

    let mw_tray_prefs = main_w.as_weak();
    if let Some(tray) = &tray {
        tray.on_open_prefs(move || {
            if let Some(m) = mw_tray_prefs.upgrade() {
                m.set_current_view("preferences".into());
                present(&m);
            }
        });
    }

    let mw_tray_about = main_w.as_weak();
    if let Some(tray) = &tray {
        tray.on_open_about(move || {
            if let Some(m) = mw_tray_about.upgrade() {
                m.set_current_view("about".into());
                present(&m);
            }
        });
    }

    if let Some(tray) = &tray {
        tray.on_quit_app(move || {
            slint::quit_event_loop().unwrap();
        });
    }

    // --- Background Polling Worker ---
    let weak = main_w.as_weak();
    let tray_weak = tray.as_ref().map(|t| t.as_weak());
    let poll_client = Arc::clone(&client);
    let sel_poll = Arc::clone(&selected_nwid);
    let cache_poll = Arc::clone(&nets_json);
    let notify_cfg = Arc::clone(&cfg_store);
    let flag_poll = Arc::clone(&poll_now);
    let conn_state: Arc<Mutex<Option<OverallState>>> = Arc::new(Mutex::new(None));

    let conn_state_poll = Arc::clone(&conn_state);
    let mut tray_icon_gold_active = true;
    let tray_icon_gold_path: &'static str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/assets/app-icon-gold.png");
    let tray_icon_red_path: &'static str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/assets/app-icon-red.png");

    std::thread::spawn(move || loop {
        let result = poll_client.poll();

        *cache_poll.lock().unwrap() = result.nets.clone();

        let service_health: Option<ServiceHealth> = match result.service_online {
            Some(true) => Some(ServiceHealth::Online),
            Some(false) => Some(ServiceHealth::Offline),
            None => None,
        };
        let states: Vec<NetworkState> = result.nets.iter().map(net_state).collect();
        let overall = OverallState::derive(service_health, &states);

        let main_infos: Vec<NetworkInfo> = result.nets.iter().map(net_info).collect();
        let tray_infos: Vec<NetworkInfo> = main_infos
            .iter()
            .map(|n| NetworkInfo {
                name: n.name.clone(),
                nwid: n.nwid.clone(),
                address: "".into(),
                state_label: n.state_label.clone(),
                state_emoji: n.state_emoji.clone(),
                state_kind: n.state_kind.clone(),
                ready: n.ready,
            })
            .collect();
        let service_ok = service_health.is_some();
        let online = matches!(service_health, Some(ServiceHealth::Online));
        let node_id = result.node_id.clone().unwrap_or_else(|| "?".into());
        let emoji = overall.emoji().into();
        let label = overall.label().into();
        let kind = overall.kind().into();
        let diag_peers = peer_infos(&result.peers);
        let node_id_copy = node_id.clone();
        let svc_copy = if service_ok {
            if online { "Online".to_string() } else { "Offline".to_string() }
        } else {
            "Service unreachable".to_string()
        };

        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_service_ok(service_ok);
            ui.set_service_state(
                if service_ok {
                    if online { "Online".into() } else { "Offline".into() }
                } else {
                    "Service unreachable".into()
                },
            );
            ui.set_node_address(node_id.into());
            ui.set_status_emoji(emoji);
            ui.set_status_kind(kind);
            ui.set_status_line(label);
            ui.set_networks(slint::ModelRc::new(slint::VecModel::from(main_infos)));
            ui.set_about_node_id(node_id_copy.into());
            ui.set_adv_service(svc_copy.into());
            ui.set_diag_peers(slint::ModelRc::new(slint::VecModel::from(diag_peers)));
        });

        let status_text = format!("{} {}", overall.emoji(), overall.label());
        let want_gold = overall == OverallState::ReadyToPlay;
        if want_gold != tray_icon_gold_active {
            tray_icon_gold_active = want_gold;
            let path: &'static str = if want_gold {
                tray_icon_gold_path
            } else {
                tray_icon_red_path
            };
            if let Some(tray_weak) = &tray_weak {
                let _ = tray_weak.upgrade_in_event_loop(move |t| {
                    if let Ok(img) = slint::Image::load_from_path(std::path::Path::new(path)) {
                        t.global::<AppTrayState>().set_icon(img);
                    }
                });
            }
        }
        if let Some(tray_weak) = &tray_weak {
            let _ = tray_weak.upgrade_in_event_loop(move |t| {
                t.set_status_text(status_text.into());
                t.set_networks(slint::ModelRc::new(slint::VecModel::from(tray_infos)));
            });
        }

        if let Some(sel) = sel_poll.lock().unwrap().clone() {
            let snap = result.nets.clone();
            let last_edit = last_local_edit_ms.load(Ordering::Relaxed);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if ui.get_current_view() == "details" {
                    fill_details(&ui, &snap, &sel);
                } else if ui.get_current_view() == "advanced" && now_ms.saturating_sub(last_edit) > 3000 {
                    fill_advanced(&ui, &snap, &sel);
                }
            });
        }

        {
            let mut last = conn_state_poll.lock().unwrap();
            if *last != Some(overall) {
                let first_sample = last.is_none();
                *last = Some(overall);
                drop(last);
                if !first_sample && notify_cfg.lock().unwrap().notify_status {
                    platform::send_notification("ZGALAXY", overall.label());
                }
            }
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let now = std::time::Instant::now();
            if now >= deadline || flag_poll.swap(false, Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100).min(deadline - now));
        }
    });

    // Window Close Behavior:
    // With tray -> hides the single MainWindow completely to system tray
    // Without tray -> quits the event loop
    let close_has_tray = tray.is_some();
    main_w.window().on_close_requested(move || {
        if !close_has_tray {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // Start Behavior
    if !config.start_in_background || tray.is_none() {
        let _ = main_w.show();
    }
    main_w.run().unwrap();
}
