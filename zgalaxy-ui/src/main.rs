//! ZGALAXY desktop client bootstrap. This file is wiring only: domain logic
//! lives in `app` (state/commands), daemon I/O in `network::client`,
//! persistence in `storage`, OS behavior in `platform` (spec §11).

mod app;
mod network;
mod platform;
mod storage;

slint::include_modules!();

use app::{NetworkState, OverallState, ServiceHealth};
use network::client::DaemonClient;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use storage::Config;

/// Bring a secondary window to the front at its correct size.
/// Slint maps a first-shown window before the layout pass runs, so it
/// briefly appears at a stale/default geometry (measured live: 90x579,
/// 192x166). Showing, hiding and showing again forces the layout to
/// settle — the third state is always the correct preferred size
/// (verified for all six windows: 460 x content height).
fn present(w: &impl slint::ComponentHandle) {
    let _ = w.show();
    if w.window().is_visible() {
        let _ = w.hide();
    }
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
            let latency = p.get("latency").and_then(|v| v.as_i64()).unwrap_or(-1);
            PeerInfo {
                address: s(p, "address").into(),
                role: s(p, "role").into(),
                latency: if latency < 0 {
                    "—".into()
                } else {
                    format!("{} ms", latency).into()
                },
                active: p
                    .get("paths")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().any(|pp| pp.get("active").and_then(|x| x.as_bool()).unwrap_or(false)))
                    .unwrap_or(false),
            }
        })
        .collect()
}

fn fill_details(det: &NetworkDetailsWindow, nets: &[serde_json::Value], nwid: &str) {
    let Some(n) = nets.iter().find(|n| s(n, "id") == nwid || s(n, "nwid") == nwid) else {
        return;
    };
    let state = net_state(n);
    let addr = n
        .get("assignedAddresses")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("");
    det.set_d_name(s(n, "name").into());
    det.set_d_state_emoji(state.emoji().into());
    det.set_d_state_label(state.label().into());
    det.set_d_state_kind(state.kind().into());
    det.set_d_address(if addr.is_empty() { "—".into() } else { addr.into() });
    det.set_d_connected(state == NetworkState::Connected);
}

fn fill_advanced(adv: &AdvancedWindow, nets: &[serde_json::Value], nwid: &str) {
    let Some(n) = nets.iter().find(|n| s(n, "id") == nwid || s(n, "nwid") == nwid) else {
        return;
    };
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
        .map(|a| {
            a.iter()
                .map(|r| {
                    let t = r.get("target").and_then(|x| x.as_str()).unwrap_or("");
                    match r.get("via").and_then(|x| x.as_str()) {
                        Some(v) => format!("{} via {}", t, v),
                        None => t.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let dns = n
        .get("dns")
        .and_then(|d| d.get("servers"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let gb = |k: &str| n.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    adv.set_a_name(s(n, "name").into());
    adv.set_a_nwid(s(n, "id").into());
    adv.set_a_state_emoji(state.emoji().into());
    adv.set_a_state_label(state.label().into());
    adv.set_a_address(if addr.is_empty() { "—".into() } else { addr.into() });
    adv.set_a_device(if s(n, "portDeviceName").is_empty() { "—".into() } else { s(n, "portDeviceName").into() });
    adv.set_a_mac(if s(n, "mac").is_empty() { "—".into() } else { s(n, "mac").into() });
    adv.set_a_type(s(n, "type").into());
    adv.set_a_routes(if routes.is_empty() { "—".into() } else { routes.into() });
    adv.set_a_dns(if dns.is_empty() { "—".into() } else { dns.into() });
    adv.set_a_allow_managed(gb("allowManaged"));
    adv.set_a_allow_global(gb("allowGlobal"));
    adv.set_a_allow_default(gb("allowDefault"));
    adv.set_a_allow_dns(gb("allowDNS"));
}

fn main() {
    let config = Config::load();

    let main_w = MainWindow::new().unwrap();
    // Tray is the app's home, but it must be optional (Preferences §6.6).
    // Without it the main window becomes the app: closing it quits.
    let tray = if config.show_tray_icon {
        Some(AppTray::new().unwrap())
    } else {
        None
    };
    let join_dlg = JoinDialog::new().unwrap();
    let det = NetworkDetailsWindow::new().unwrap();
    let adv = AdvancedWindow::new().unwrap();
    let prefs = PreferencesWindow::new().unwrap();
    let about = AboutWindow::new().unwrap();

    about.set_about_version(env!("CARGO_PKG_VERSION").into());
    prefs.set_about_version(env!("CARGO_PKG_VERSION").into());
    prefs.set_p_start_with_system(config.start_with_system);
    main_w.set_app_version(env!("CARGO_PKG_VERSION").into());
    prefs.set_p_start_background(config.start_in_background);
    prefs.set_p_show_tray(config.show_tray_icon);
    prefs.set_p_notify_status(config.notify_status);

    let client = Arc::new(DaemonClient::from_env());
    prefs.set_adv_endpoint(client.endpoint().into());

    let nets_json: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let advanced_selected: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let poll_now = Arc::new(AtomicBool::new(false));

    let det_weak = det.as_weak();
    let sel = Arc::clone(&advanced_selected);
    let cache = Arc::clone(&nets_json);
    main_w.on_open_details(move |nwid| {
        let nwid = nwid.trim().to_string();
        if nwid.is_empty() {
            return;
        }
        *sel.lock().unwrap() = Some(nwid.clone());
        let det = det_weak.unwrap();
        fill_details(&det, &cache.lock().unwrap(), &nwid);
        present(&det);
    });

    adv.on_copy_address(|text| platform::copy_to_clipboard(&text));

    let join_show = join_dlg.as_weak();
    main_w.on_open_join(move || {
        if let Some(d) = join_show.upgrade() {
            d.set_error_text("".into());
            d.set_join_input("".into());
            present(&d);
            d.invoke_focus_input();
        }
    });

    let now_flag = Arc::clone(&poll_now);
    main_w.on_retry(move || {
        now_flag.store(true, Ordering::Relaxed);
    });

    main_w.on_copy_text(|text| platform::copy_to_clipboard(&text));
    let c = Arc::clone(&client);
    let leave_flag = Arc::clone(&poll_now);
    main_w.on_leave_network(move |nwid| {
        let nwid = nwid.trim().to_string();
        if !nwid.is_empty() {
            app::commands::leave(Arc::clone(&c), nwid, Some(Arc::clone(&leave_flag)));
        }
    });

    // --- JoinDialog ---
    let c = Arc::clone(&client);
    let dlg_close = join_dlg.as_weak();
    let join_flag = Arc::clone(&poll_now);
    join_dlg.on_request_join(move |nwid| {
        let nwid = nwid.trim().to_string();
        if nwid.is_empty() {
            return;
        }
        if !app::commands::is_valid_nwid(&nwid) {
            if let Some(d) = dlg_close.upgrade() {
                d.set_error_text(
                    "Invalid Network ID — it is 16 characters (letters 0-9, a-f), like 93afae5963d77bcd".into(),
                );
            }
            return;
        }
        if let Some(d) = dlg_close.upgrade() {
            d.set_error_text("".into());
            d.set_join_input("".into());
            let _ = d.hide();
        }
        app::commands::join(Arc::clone(&c), nwid.to_lowercase(), Some(Arc::clone(&join_flag)));
    });
    let dlg_close2 = join_dlg.as_weak();
    join_dlg.on_request_close(move || {
        if let Some(d) = dlg_close2.upgrade() {
            let _ = d.hide();
        }
    });

    // --- NetworkDetailsWindow ---
    let adv_show_weak = adv.as_weak();
    det.on_copy_address(|text| platform::copy_to_clipboard(&text));
    let c = Arc::clone(&client);
    let det_close = det.as_weak();
    let sel_det = Arc::clone(&advanced_selected);
    let det_flag = Arc::clone(&poll_now);
    det.on_request_disconnect(move || {
        if let Some(nwid) = sel_det.lock().unwrap().clone() {
            app::commands::leave(Arc::clone(&c), nwid, Some(Arc::clone(&det_flag)));
        }
        if let Some(d) = det_close.upgrade() {
            let _ = d.hide();
        }
    });
    let det_adv = det.as_weak();
    let cache_det = Arc::clone(&nets_json);
    let sel_adv = Arc::clone(&advanced_selected);
    det.on_open_advanced(move || {
        if let Some(nwid) = sel_adv.lock().unwrap().clone() {
            if let Some(a) = det_adv.upgrade() {
                let _ = a.hide();
            }
            if let Some(adv) = adv_show_weak.upgrade() {
                fill_advanced(&adv, &cache_det.lock().unwrap(), &nwid);
                present(&adv);
            }
        }
    });
    let det_close2 = det.as_weak();
    det.on_request_close(move || {
        if let Some(d) = det_close2.upgrade() {
            let _ = d.hide();
        }
    });

    // --- AdvancedWindow ---
    let last_local_edit_ms = Arc::new(AtomicU64::new(0));
    let adv_close = adv.as_weak();
    adv.on_request_close(move || {
        if let Some(a) = adv_close.upgrade() {
            let _ = a.hide();
        }
    });
    let adv_flags = adv.as_weak();
    let client_flags = Arc::clone(&client);
    let sel_flags = Arc::clone(&advanced_selected);
    let adv_flag = Arc::clone(&poll_now);
    let edit_stamp = Arc::clone(&last_local_edit_ms);
    adv.on_advanced_flags_changed(move || {
        let Some(a) = adv_flags.upgrade() else { return };
        let Some(nwid) = sel_flags.lock().unwrap().clone() else { return };
        let body = serde_json::json!({
            "allowManaged": a.get_a_allow_managed(),
            "allowGlobal": a.get_a_allow_global(),
            "allowDefault": a.get_a_allow_default(),
            "allowDNS": a.get_a_allow_dns(),
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

    // --- Preferences ---
    let cfg_store = Arc::new(Mutex::new(config));
    let pw = prefs.as_weak();
    let cfg_ref = Arc::clone(&cfg_store);
    prefs.on_prefs_changed(move || {
        let p = pw.unwrap();
        let c = Config {
            start_with_system: p.get_p_start_with_system(),
            start_in_background: p.get_p_start_background(),
            show_tray_icon: p.get_p_show_tray(),
            notify_status: p.get_p_notify_status(),
        };
        c.save();
        platform::set_autostart(c.start_with_system);
        *cfg_ref.lock().unwrap() = c;
    });
    let prefs_close = prefs.as_weak();
    prefs.on_request_close(move || {
        if let Some(p) = prefs_close.upgrade() {
            let _ = p.hide();
        }
    });

    // --- About ---
    let about_close = about.as_weak();
    about.on_request_close(move || {
        if let Some(a) = about_close.upgrade() {
            let _ = a.hide();
        }
    });

    // --- Tray ---
    let mw_weak = main_w.as_weak();
    if let Some(tray) = &tray { tray.on_open_main(move || {
        if let Some(m) = mw_weak.upgrade() {
            present(&m);
        }
    }); }
    let dlg_from_tray = join_dlg.as_weak();
    if let Some(tray) = &tray { tray.on_open_join(move || {
        if let Some(d) = dlg_from_tray.upgrade() {
            d.set_error_text("".into());
            d.set_join_input("".into());
            present(&d);
            d.invoke_focus_input();
        }
    }); }
    let sel_tray = Arc::clone(&advanced_selected);
    let cache_tray = Arc::clone(&nets_json);
    let det_tray = det.as_weak();
    if let Some(tray) = &tray { tray.on_open_network(move |nwid| {
        let nwid = nwid.trim().to_string();
        if nwid.is_empty() {
            return;
        }
        *sel_tray.lock().unwrap() = Some(nwid.clone());
        if let Some(d) = det_tray.upgrade() {
            fill_details(&d, &cache_tray.lock().unwrap(), &nwid);
            present(&d);
        }
    }); }
    let prefs_show = prefs.as_weak();
    if let Some(tray) = &tray { tray.on_open_prefs(move || {
        if let Some(p) = prefs_show.upgrade() {
            present(&p);
        }
    }); }
    let about_show = about.as_weak();
    if let Some(tray) = &tray { tray.on_open_about(move || {
        if let Some(a) = about_show.upgrade() {
            present(&a);
        }
    }); }
    if let Some(tray) = &tray { tray.on_quit_app(move || {
        slint::quit_event_loop().unwrap();
    }); }

    // --- Background poller (spec §13): query → derive state centrally →
    // reflect on every window through the event loop. Sleep is chunked so
    // Retry can trigger an immediate cycle.
    let weak = main_w.as_weak();
    let tray_weak = tray.as_ref().map(|t| t.as_weak());
    let about_weak = about.as_weak();
    let det_poll = det.as_weak();
    let prefs_poll = prefs.as_weak();
    let adv_poll = adv.as_weak();
    let poll_client = Arc::clone(&client);
    let sel_poll = Arc::clone(&advanced_selected);
    let cache_poll = Arc::clone(&nets_json);
    let notify_cfg = Arc::clone(&cfg_store);
    let flag_poll = Arc::clone(&poll_now);
    let conn_state: Arc<Mutex<Option<OverallState>>> = Arc::new(Mutex::new(None));
    let conn_state_poll = Arc::clone(&conn_state);
    let mut tray_icon_gold_active = false;
    // App icon follows connection state: gold when ready, gray otherwise
    // (the logo's original color) — the tray icon is the state beacon.
    // App icon follows connection state: gold when ready, gray otherwise.
    // Paths are compiled in as &'static str so the (non-Send) Image is only
    // decoded inside the UI-thread closure, on state changes.
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

        if let Some(node) = &result.node_id {
            let node2 = node.clone();
            let _ = about_weak.upgrade_in_event_loop(move |a| {
                a.set_about_node_id(node2.into());
            });
            let node2 = node.clone();
            let svc = if service_ok {
                if online { "Online".to_string() } else { "Offline".to_string() }
            } else {
                "Service unreachable".to_string()
            };
            let diag_peers = peer_infos(&result.peers);
            let _ = prefs_poll.upgrade_in_event_loop(move |p| {
                p.set_about_node_id(node2.into());
                p.set_adv_service(svc.into());
                p.set_diag_peers(slint::ModelRc::new(slint::VecModel::from(diag_peers)));
            });
        }

        if let Some(sel) = sel_poll.lock().unwrap().clone() {
            let snap = result.nets.clone();
            let _ = det_poll.upgrade_in_event_loop(move |d| {
                if d.window().is_visible() {
                    fill_details(&d, &snap, &sel);
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

        if let Some(sel) = sel_poll.lock().unwrap().clone() {
            let last_edit = last_local_edit_ms.load(Ordering::Relaxed);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if now_ms.saturating_sub(last_edit) > 3000 {
                let snap = result.nets.clone();
                let _ = adv_poll.upgrade_in_event_loop(move |a| {
                    fill_advanced(&a, &snap, &sel);
                });
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

    // Window close (spec §17/§21.4): with a tray, closing hides to the tray;
    // without one, closing the main window is the only sane way to quit.
    let close_has_tray = tray.is_some();
    main_w.window().on_close_requested(move || {
        if !close_has_tray {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // Never leave the app unreachable: background start without a tray icon
    // would be an invisible process with no handle — show the window instead.
    if !config.start_in_background || tray.is_none() {
        let _ = main_w.show();
    }
    main_w.run().unwrap();
}
