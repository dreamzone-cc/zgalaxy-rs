use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use tokio::fs;
use tokio::sync::mpsc;
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;
use anyhow::{Context, Result};

use zgalaxy_rs::cli::Cli;
use zgalaxy_rs::config::LocalConfig;
use zgalaxy_rs::controller::EmbeddedController;
use zgalaxy_rs::controller_api::{AppState, ControllerServer};
use zgalaxy_rs::identity::Identity;
use zgalaxy_rs::nat::NatTraversalEngine;
use zgalaxy_rs::network::NetworkManager;
use zgalaxy_rs::peer::PeerManager;
use zgalaxy_rs::resolver::DynamicDnsResolver;
use zgalaxy_rs::transport::UdpTransport;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Handle CLI arguments (if invoked as zgalaxy-cli / zerotier-cli / idtool)
    if std::env::args().len() > 1 {
        match Cli::try_parse() {
            Ok(cli) => return cli.execute().await,
            Err(e) => {
                e.print()?;
                return Ok(());
            }
        }
    }

    // Default: Run ZGALAXY Sovereign Daemon
    info!("=======================================================");
    info!("ZGALAXY One — Sovereign Rust Client Daemon v1.3.0");
    info!("100% Memory-Safe, AGPL-3.0 Sovereign Open Source");
    info!("=======================================================");

    let data_dir = PathBuf::from("/var/lib/zerotier-one");
    let fallback_dir = PathBuf::from("./zerotier-var");
    let mut working_dir = fallback_dir.clone();
    if data_dir.exists() {
        // Prefer the system data dir only if it is actually writable.
        if let Ok(meta) = fs::metadata(&data_dir).await {
            if !meta.permissions().readonly() {
                let probe = data_dir.join(".zgalaxy_write_probe");
                match fs::write(&probe, b"").await {
                    Ok(_) => {
                        let _ = fs::remove_file(&probe).await;
                        working_dir = data_dir;
                    }
                    Err(_) => {
                        warn!(
                            "Data directory {:?} exists but is not writable; falling back to {:?}",
                            data_dir, fallback_dir
                        );
                    }
                }
            }
        }
    }
    let _ = fs::create_dir_all(&working_dir).await;

    // Load Local Configuration (local.conf and networks.d/)
    let conf_file = working_dir.join("local.conf");
    let mut local_config = LocalConfig::load(&working_dir).await;

    // Port resolution for ZGALAXY drop-in compatibility:
    // 1. Explicit local.conf port wins.
    // 2. ZGALAXY container: /app/config/zerotier-one.port (relative to CWD).
    // 3. ZT_PORT environment variable (used by the ZGALAXY Dockerfile).
    // 4. Default 9993.
    if !conf_file.exists() {
        let zgalaxy_port_file = PathBuf::from("./config/zerotier-one.port");
        if zgalaxy_port_file.exists() {
            if let Ok(p) = fs::read_to_string(&zgalaxy_port_file).await {
                if let Ok(v) = p.trim().parse::<u16>() {
                    local_config.port = v;
                }
            }
        } else if let Ok(env_port) = std::env::var("ZT_PORT") {
            if let Ok(v) = env_port.parse::<u16>() {
                local_config.port = v;
            }
        }
    }

    // Load or generate identity
    let secret_path = working_dir.join("identity.secret");
    let public_path = working_dir.join("identity.public");
    let identity = if secret_path.exists() {
        let content = fs::read_to_string(&secret_path).await?;
        Identity::parse(&content).context("Failed to parse identity.secret")?
    } else {
        info!("Generating fresh cryptographic identity...");
        let id = Identity::generate();
        let _ = fs::write(&public_path, id.to_public_string()).await;
        let _ = fs::write(&secret_path, id.to_secret_string()?).await;
        restrict_secret_permissions(&secret_path);
        id
    };

    info!("Node Address: {}", identity.address);

    // Load or generate authtoken.secret
    let auth_path = working_dir.join("authtoken.secret");
    let auth_token = if auth_path.exists() {
        fs::read_to_string(&auth_path).await?.trim().to_string()
    } else {
        let token = hex::encode(rand::random::<[u8; 16]>());
        let _ = fs::write(&auth_path, &token).await;
        restrict_secret_permissions(&auth_path);
        token
    };

    // Initialize State Managers
    let peer_manager = PeerManager::new();
    let network_manager = NetworkManager::new();

    // Resolve domain dynamically (Separation of Dynamic Config from Core Build)
    let domain_file = working_dir.join("domain");
    let fallback_domain_file = PathBuf::from("./config/domain");
    let resolved_domain = if domain_file.exists() {
        fs::read_to_string(&domain_file).await.unwrap_or_default().trim().to_string()
    } else if fallback_domain_file.exists() {
        fs::read_to_string(&fallback_domain_file).await.unwrap_or_default().trim().to_string()
    } else {
        std::env::var("ZGALAXY_DOMAIN").unwrap_or_else(|_| "dz.dreamzone.cc".to_string())
    };

    let target_endpoint = format!("{}:{}", if resolved_domain.is_empty() { "dz.dreamzone.cc" } else { &resolved_domain }, local_config.port);
    info!("[ZGALAXY DYNAMIC CONFIG] Dynamic endpoint bound to domain: {}", target_endpoint);

    // Start Native Async Dynamic IP Resolver (Zero Rebuild on IP Change, Multi-Source Decoupled)
    let resolver = Arc::new(DynamicDnsResolver::new(30).with_config_file(working_dir.join("domains.json")));
    let _ = resolver.load_sources(&working_dir, local_config.port).await;
    resolver.clone().start_worker();

    // Initialize NAT Traversal & Hole-Punching Engine
    let nat_engine = Arc::new(NatTraversalEngine::new(peer_manager.clone()));
    nat_engine.clone().start_worker();

    // Initialize Embedded Controller (Pure Rust, FileDB compatible with ZeroTier controller.d/)
    let controller = EmbeddedController::new(identity.clone(), working_dir.clone());
    let _ = controller.init().await;

    // Start Local REST API Controller Plane on the same port as the UDP
    // transport (ZeroTier binds its control plane and wire socket together).
    let api_port = local_config.port;
    let app_state = AppState {
        identity: identity.clone(),
        auth_token,
        peer_manager: peer_manager.clone(),
        network_manager: network_manager.clone(),
        controller: controller.clone(),
        resolver: resolver.clone(),
    };

    tokio::spawn(async move {
        if let Err(e) = ControllerServer::start(app_state, api_port).await {
            error!("[ZGALAXY REST API ERROR] {}", e);
        }
    });

    // Initialize High-Performance UDP Transport Loop & Virtual Adapter Routing
    let (tun_inbound_tx, tun_inbound_rx) = mpsc::channel::<Vec<u8>>(1024);
    let (tun_outbound_tx, mut tun_outbound_rx) = mpsc::channel::<Vec<u8>>(1024);
    let mut tun_outbound_rx = Some(tun_outbound_rx);

    let mut tun_device = zgalaxy_rs::tun::TunDevice::new("zgalaxy0", 2800);
    let _ = tun_device.create_and_bind(None).await;
    let tun_arc = Arc::new(tun_device);
    tun_arc.start_packet_loop(tun_inbound_rx, tun_outbound_tx);

    // Transport selection: "quic" (primary, per architecture constraint) or
    // "udp" (legacy path, kept until the QUIC data plane is feature-complete).
    let transport_mode = local_config
        .settings
        .get("transportMode")
        .and_then(|v| v.as_str())
        .unwrap_or("udp")
        .to_ascii_lowercase();
    let mut quic_started = false;

    if transport_mode == "quic" {
        let bind_addr = std::net::SocketAddr::from(([0, 0, 0, 0], local_config.port));
        match zgalaxy_rs::quic::QuicTransport::bind(bind_addr, identity.address.to_string()) {
            Ok(quic) => {
                quic_started = true;
                let quic = Arc::new(quic);
                let (quic_events_tx, mut quic_events_rx) = mpsc::channel(256);

                let quic_runner = Arc::clone(&quic);
                tokio::spawn(async move {
                    if let Err(e) = quic_runner.run(quic_events_tx).await {
                        error!("[ZGALAXY QUIC] Accept loop terminated: {}", e);
                    }
                });

                // Engine loop: QUIC events → TUN / network manager.
                let nm_for_events = network_manager.clone();
                let tun_tx = tun_inbound_tx.clone();
                let quic_engine = Arc::clone(&quic);
                let ctrl_for_events = controller.clone();
                tokio::spawn(async move {
                    while let Some(event) = quic_events_rx.recv().await {
                        use zgalaxy_rs::quic::QuicEvent;
                        match event {
                            QuicEvent::Datagram { remote, data } => {
                                tracing::debug!("[ZGALAXY QUIC] Frame ({}B) from {}", data.len(), remote);
                                let _ = tun_tx.send(data.to_vec()).await;
                            }
                            QuicEvent::Control { remote, message } => {
                                use zgalaxy_rs::quic::control::ControlMessage;
                                match message {
                                    ControlMessage::NodeAnnounce { address, .. } => {
                                        info!("[ZGALAXY QUIC] Peer {} announced as {}", remote, address);
                                    }
                                    ControlMessage::NetworkConfigRequest { nwid } => {
                                        // This node acts as controller for its own networks.
                                        // Member id is the QUIC remote's address string for now;
                                        // proper node-address binding arrives with the
                                        // membership-token phase.
                                        let member = remote.ip().to_string();
                                        let _ = ctrl_for_events.register_join_request(&nwid, &member, None).await;
                                    }
                                    ControlMessage::NetworkConfigResponse { nwid, config } => {
                                        let authorized = config.get("authorized")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                        let ips: Vec<String> = config
                                            .get("ipAssignments")
                                            .and_then(|v| v.as_array())
                                            .map(|a| {
                                                a.iter()
                                                    .filter_map(|x| x.as_str().map(String::from))
                                                    .collect()
                                            })
                                            .unwrap_or_default();
                                        if let Some(mut net) = nm_for_events.get(&nwid).await {
                                            net.status = if authorized {
                                                zgalaxy_rs::network::NetworkStatus::Ok
                                            } else {
                                                zgalaxy_rs::network::NetworkStatus::AccessDenied
                                            };
                                            if !ips.is_empty() {
                                                net.assigned_addresses = ips;
                                            }
                                            nm_for_events.update_network(net).await;
                                            info!("[ZGALAXY QUIC] Network {} updated (authorized: {})", nwid, authorized);
                                        }
                                    }
                                    ControlMessage::Ping { nonce, sent_ms } => {
                                        let now = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis() as u64;
                                        let _ = quic_engine.send_control(
                                            remote,
                                            &ControlMessage::Pong { nonce, sent_ms, received_ms: now },
                                        ).await;
                                    }
                                    ControlMessage::Pong { .. } => {}
                                }
                            }
                            QuicEvent::Connected { remote } => {
                                info!("[ZGALAXY QUIC] Peer session established: {}", remote);
                            }
                            QuicEvent::Disconnected { remote } => {
                                info!("[ZGALAXY QUIC] Peer session lost: {}", remote);
                            }
                        }
                    }
                });

                // Relay host outbound frames from TUN over QUIC datagrams to
                // every connected peer (proper L2 learning comes with the
                // data-plane phase).
                let quic_for_outbound = Arc::clone(&quic);
                if let Some(mut tun_outbound_rx) = tun_outbound_rx.take() {
                tokio::spawn(async move {
                    while let Some(frame) = tun_outbound_rx.recv().await {
                        let peers = quic_for_outbound.connected_peers().read().await.clone();
                        for (remote, _peer) in peers {
                            let _ = quic_for_outbound
                                .send_frame(remote, bytes::Bytes::from(frame.clone()))
                                .await;
                        }
                    }
                });
                }

                // Periodic network-config sync over reliable control streams.
                let nm_for_sync = network_manager.clone();
                let resolver_for_sync = resolver.clone();
                let quic_for_sync = Arc::clone(&quic);
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                    loop {
                        interval.tick().await;
                        let nets = nm_for_sync.list().await;
                        if nets.is_empty() {
                            continue;
                        }
                        let mut targets = resolver_for_sync.get_all_active_addresses().await;
                        if let Ok(extra) = std::env::var("ZGALAXY_EXTRA_ENDPOINTS") {
                            for ep in extra.split(',') {
                                if let Ok(addr) = ep.trim().parse::<std::net::SocketAddr>() {
                                    targets.push(addr);
                                }
                            }
                        }
                        for net in nets {
                            for target in &targets {
                                let _ = quic_for_sync
                                    .send_control(
                                        *target,
                                        &zgalaxy_rs::quic::control::ControlMessage::NetworkConfigRequest {
                                            nwid: net.nwid.clone(),
                                        },
                                    )
                                    .await;
                            }
                        }
                    }
                });
            }
            Err(e) => {
                error!("[ZGALAXY QUIC] Failed to bind QUIC transport: {} — falling back to UDP", e);
            }
        }
    }

    if transport_mode != "quic" || !quic_started {
    if let Ok(transport) = UdpTransport::bind(local_config.port, identity.clone(), peer_manager.clone(), resolver.clone()).await {
        let transport_arc = Arc::new(transport);
        transport_arc.set_controller(controller.clone()).await;
        transport_arc.set_network_manager(network_manager.clone()).await;
        transport_arc.start_rx_loop(tun_inbound_tx);
        nat_engine.set_transport(transport_arc.clone()).await;

        // Relay host outbound frames captured from TUN into UDP wire transport
        let tp_for_outbound = transport_arc.clone();
        if let Some(mut tun_outbound_rx) = tun_outbound_rx.take() {
        tokio::spawn(async move {
            while let Some(frame) = tun_outbound_rx.recv().await {
                let _ = tp_for_outbound.broadcast_frame(frame).await;
            }
        });
        }

        // Background network join & configuration sync loop
        let nm_for_sync = network_manager.clone();
        let tp_for_sync = transport_arc.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;
                let nets = nm_for_sync.list().await;
                for net in nets {
                    let _ = tp_for_sync.send_network_config_request(&net.nwid).await;
                }
            }
        });
    } else {
        warn!("[ZGALAXY UDP] Could not bind UDP port {}, running in client API mode.", local_config.port);
    }
    } // end legacy UDP transport block

    // Auto-join configured networks from networks.d/
    for nwid in &local_config.auto_join_networks {
        info!("[ZGALAXY AUTO-JOIN] Joining persistent network {}", nwid);
        let _ = network_manager.join(nwid).await;
    }

    info!("ZGALAXY Client Daemon is fully active, listening, and serving.");

    // Keep running until signal
    tokio::signal::ctrl_c().await?;
    info!("Shutting down ZGALAXY Client Daemon cleanly...");
    Ok(())
}

/// Restrict a secret file (identity.secret, authtoken.secret) to owner-only
/// permissions (0600), matching canonical ZeroTier behavior.
#[cfg(unix)]
fn restrict_secret_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn restrict_secret_permissions(_path: &std::path::Path) {}
