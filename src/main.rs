use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use clap::Parser;
use tokio::fs;
use tokio::sync::mpsc;
use tracing::{info, warn, error};
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

/// Presence bookkeeping cadence for datagram-driven refreshes (ب3).
const PEER_PRESENCE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// RTT probe cadence over QUIC control streams (ب3).
const RTT_PROBE_INTERVAL_SECS: u64 = 10;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging. RUST_LOG is honored when set (e.g.
    // RUST_LOG=info,zgalaxy_rs=debug); without it everything info+ is logged.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Handle CLI arguments (if invoked as zgalaxy-cli / zerotier-cli / idtool).
    // The same binary is symlinked under several names in containers
    // (zerotier-one, zerotier-cli, zerotier-idtool, mkmoonworld) — route by
    // argv0 so legacy invocations keep working, e.g.
    //   zerotier-idtool generate identity.secret identity.public
    //   mkmoonworld moon.json
    let argv0 = std::env::args().next().unwrap_or_default();
    let bin_name = argv0.rsplit('/').next().unwrap_or(&argv0).to_string();
    let mut cli_args: Vec<String> = std::env::args().skip(1).collect();

    // ZeroTier daemon flags: -d (fork to background) and -p<port> (port
    // override with highest precedence, e.g. `zerotier-one -p9994 -d`).
    let mut daemonize = false;
    let mut port_override: Option<u16> = None;
    cli_args.retain(|a| {
        if a == "-d" {
            daemonize = true;
            false
        } else if a.starts_with("-p")
            && a.len() > 2
            && a[2..].chars().all(|c| c.is_ascii_digit())
        {
            port_override = a[2..].parse().ok();
            false
        } else {
            true
        }
    });

    if daemonize {
        // Re-spawn without -d in a new process group (detached, stdio null)
        // and exit — matching the ZeroTier `-d` daemonize contract that the
        // ZGALAXY container entrypoint relies on. The port flag must be
        // re-passed to the child: it was consumed from argv above.
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe().context("cannot resolve own executable")?;
        let mut child_args = cli_args.clone();
        if let Some(p) = port_override {
            child_args.push(format!("-p{}", p));
        }
        std::process::Command::new(exe)
            .args(&child_args)
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("failed to daemonize")?;
        return Ok(());
    }

    if !cli_args.is_empty() {
        let mut parse_args: Vec<String> = Vec::with_capacity(cli_args.len() + 2);
        if bin_name.contains("mkmoonworld") {
            parse_args.push("idtool".to_string());
            parse_args.push("mkmoonworld".to_string());
        } else if bin_name.contains("idtool") {
            parse_args.push("idtool".to_string());
        }
        parse_args.extend(cli_args);
        match Cli::try_parse_from(std::iter::once(argv0).chain(parse_args)) {
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

    let data_dir = std::env::var("ZGALAXY_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/zerotier-one"));
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

    // Command-line `-p<port>` wins over every other source (ZeroTier
    // daemon semantics; the ZGALAXY entrypoint passes it explicitly).
    if let Some(p) = port_override {
        local_config.port = p;
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
    network_manager
        .set_node_address(identity.address.to_string())
        .await;

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

    // Initialize NAT Traversal & Hole-Punching Engine.
    // The raw-UDP keepalive worker is only meaningful for the legacy UDP
    // transport; in QUIC mode keep-alives are built into the connection
    // (transport.keep_alive_interval), so the worker starts there instead.
    let nat_engine = Arc::new(NatTraversalEngine::new(peer_manager.clone()));

    // Initialize Embedded Controller (Pure Rust, FileDB compatible with ZeroTier controller.d/)
    let controller = EmbeddedController::new(identity.clone(), working_dir.clone());
    let _ = controller.init().await;

    // Start Local REST API Controller Plane on the same port as the UDP
    // transport (ZeroTier binds its control plane and wire socket together).
    let api_port = local_config.port;
    let app_state = AppState {
        identity: identity.clone(),
        auth_token,
        allow_management_from: local_config.allow_management_from.clone(),
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

    // Transport selection: "quic" (primary, per architecture constraint) or
    // "udp" (legacy path, kept until the QUIC data plane is feature-complete).
    let transport_mode = local_config
        .settings
        .get("transportMode")
        .and_then(|v| v.as_str())
        .unwrap_or("udp")
        .to_ascii_lowercase();

    // QUIC datagrams cannot exceed ~1200 bytes on the wire; a larger adapter
    // MTU would produce frames that get silently dropped instead of sent.
    // TAP frames carry a 14-byte Ethernet header, leaving 1186 for payload.
    let tun_mtu: u32 = if transport_mode == "quic" { 1186 } else { 2800 };

    // Initialize High-Performance UDP Transport Loop & Virtual Adapter Routing
    let (tun_inbound_tx, tun_inbound_rx) = mpsc::channel::<Vec<u8>>(1024);
    let (tun_outbound_tx, tun_outbound_rx) = mpsc::channel::<Vec<u8>>(1024);
    let mut tun_outbound_rx = Some(tun_outbound_rx);

    let mut tun_device = zgalaxy_rs::tun::TunDevice::new("zgalaxy0", tun_mtu);
    let _ = tun_device.create_and_bind(None).await;
    let tun_arc = Arc::new(tun_device);
    tun_arc.start_packet_loop(tun_inbound_rx, tun_outbound_tx);

    let mut quic_started = false;

    if transport_mode == "quic" {
        let bind_addr = std::net::SocketAddr::from(([0, 0, 0, 0], local_config.port));
        match zgalaxy_rs::quic::QuicTransport::bind(
                        bind_addr,
                        identity.address.to_string(),
                        identity.to_public_string(),
                    ) {
            Ok(quic) => {
                quic_started = true;
                let quic = Arc::new(quic);
                let (quic_events_tx, mut quic_events_rx) = mpsc::channel(1024);

                let quic_runner = Arc::clone(&quic);
                tokio::spawn(async move {
                    if let Err(e) = quic_runner.run(quic_events_tx).await {
                        error!("[ZGALAXY QUIC] Accept loop terminated: {}", e);
                    }
                });

                // Engine loop: QUIC events → TUN / network manager / presence.
                let nm_for_events = network_manager.clone();
                let tun_tx = tun_inbound_tx.clone();
                let quic_engine = Arc::clone(&quic);
                let ctrl_for_events = controller.clone();
                let tun_for_events = Arc::clone(&tun_arc);
                let peers_for_events = peer_manager.clone();
                let identity_for_events = identity.clone();
                let mut applied_networks: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                tokio::spawn(async move {
                    while let Some(event) = quic_events_rx.recv().await {
                        use zgalaxy_rs::quic::QuicEvent;
                        // Presence refresh throttle: datagrams can arrive at
                        // game tick rates; member/peer bookkeeping runs at most
                        // once per PEER_PRESENCE_INTERVAL per remote.
                        let mut last_presence: std::collections::HashMap<
                            std::net::SocketAddr,
                            std::time::Instant,
                        > = std::collections::HashMap::new();
                        match event {
                            QuicEvent::Datagram { remote, data } => {
                                tracing::debug!("[ZGALAXY QUIC] Frame ({}B) from {}", data.len(), remote);
                                let fresh = last_presence
                                    .get(&remote)
                                    .map(|t| t.elapsed() >= PEER_PRESENCE_INTERVAL)
                                    .unwrap_or(true);
                                if fresh {
                                    last_presence.insert(remote, std::time::Instant::now());
                                    if let Some(addr_str) =
                                        quic_engine.announced_address(remote).await
                                    {
                                        if let Ok(node) =
                                            zgalaxy_rs::identity::Address::from_str(&addr_str)
                                        {
                                            peers_for_events
                                                .add_or_update_peer(
                                                    node,
                                                    zgalaxy_rs::peer::PeerRole::Leaf,
                                                    remote,
                                                    0,
                                                )
                                                .await;
                                            ctrl_for_events
                                                .touch_member_last_seen(
                                                    &addr_str,
                                                    &format!("{}/{}", remote.ip(), remote.port()),
                                                )
                                                .await;
                                        }
                                    }
                                }
                                let _ = tun_tx.send(data.to_vec()).await;
                            }
                            QuicEvent::Control { remote, message } => {
                                use zgalaxy_rs::quic::control::ControlMessage;
                                match message {
                                    ControlMessage::NodeAnnounce { address, .. } => {
                                        info!("[ZGALAXY QUIC] Peer {} announced as {} — challenging (أ3)", remote, address);
                                        // أ3: never trust an unproven announcement.
                                        // Issue a nonce challenge; presence wiring
                                        // happens only after a valid AnnounceProof.
                                        let nonce =
                                            quic_engine.challenge_announce(remote, &address).await;
                                        let _ = quic_engine
                                            .send_control(
                                                remote,
                                                &ControlMessage::NodeChallenge { nonce },
                                            )
                                            .await;
                                    }
                                    ControlMessage::NodeChallenge { nonce } => {
                                        // We are the announcer side: prove possession
                                        // of our identity's secret key.
                                        let address =
                                            identity_for_events.address.to_string();
                                        let material = zgalaxy_rs::quic::challenge_material(
                                            nonce,
                                            &address,
                                        );
                                        match identity_for_events.sign(&material) {
                                            Ok(sig) => {
                                                let _ = quic_engine
                                                    .send_control(
                                                        remote,
                                                        &ControlMessage::AnnounceProof {
                                                            nonce,
                                                            address,
                                                            signature: hex::encode(sig),
                                                            public_identity: identity_for_events.to_public_string(),
                                                        },
                                                    )
                                                    .await;
                                            }
                                            Err(e) => warn!(
                                                "[ZGALAXY QUIC] cannot sign announce proof: {}", e
                                            ),
                                        }
                                    }
                                    ControlMessage::AnnounceProof { nonce, address, signature, public_identity } => {
                                        // ب3 + أ3: presence/ZTNET observability is
                                        // granted only to cryptographically proven
                                        // announcements.
                                        if quic_engine
                                            .verify_announce_proof(
                                                remote, nonce, &address, &signature, &public_identity,
                                            )
                                            .await
                                        {
                                            info!(
                                                "[ZGALAXY QUIC] Peer {} proven as {}",
                                                remote, address
                                            );
                                            if let Ok(node) =
                                                zgalaxy_rs::identity::Address::from_str(&address)
                                            {
                                                peers_for_events
                                                    .add_or_update_peer(
                                                        node,
                                                        zgalaxy_rs::peer::PeerRole::Leaf,
                                                        remote,
                                                        0,
                                                    )
                                                    .await;
                                            }
                                            ctrl_for_events
                                                .touch_member_last_seen(
                                                    &address,
                                                    &format!("{}/{}", remote.ip(), remote.port()),
                                                )
                                                .await;
                                        } else {
                                            warn!(
                                                "[ZGALAXY QUIC] announce proof from {} rejected",
                                                remote
                                            );
                                        }
                                    }
                                    ControlMessage::NetworkConfigRequest { nwid, token: _ } => {
                                        // Act as controller for networks owned by this node.
                                        // The member id MUST be the announced node address —
                                        // anything else cannot map to a member record.
                                        // announced_address is Some only AFTER a valid
                                        // AnnounceProof (أ3) — unproven peers are refused.
                                        let Some(member) = quic_engine.announced_address(remote).await else {
                                            warn!("[ZGALAXY QUIC] config request from {} before proven announce — ignored", remote);
                                            continue;
                                        };
                                        match ctrl_for_events.register_join_request(&nwid, &member, None).await {
                                            Ok(_) => {
                                                if let Some(net) = ctrl_for_events.get_network(&nwid).await {
                                                    let member_rec = ctrl_for_events.get_member(&nwid, &member).await;
                                                    let authorized = member_rec.as_ref().map(|m| m.authorized).unwrap_or(false);
                                                    let ips = member_rec.map(|m| m.ip_assignments).unwrap_or_default();
                                                    let response = serde_json::json!({
                                                        "nwid": nwid,
                                                        "name": net.name,
                                                        "authorized": authorized,
                                                        "ipAssignments": ips,
                                                        "routes": net.routes,
                                                        "mtu": net.mtu,
                                                    });
                                                    if let Err(e) = quic_engine.send_control(
                                                        remote,
                                                        &ControlMessage::NetworkConfigResponse { nwid, config: response },
                                                    ).await {
                                                        warn!("[ZGALAXY QUIC] failed to answer config request from {}: {}", remote, e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!("[ZGALAXY QUIC] join request {} from {} rejected: {}", nwid, member, e);
                                            }
                                        }
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
                                            net.membership_token = config
                                                .get("membershipToken")
                                                .and_then(|v| v.as_str())
                                                .map(String::from);
                                            net.status = if authorized {
                                                zgalaxy_rs::network::NetworkStatus::Ok
                                            } else {
                                                zgalaxy_rs::network::NetworkStatus::AccessDenied
                                            };
                                            if !ips.is_empty() {
                                                net.assigned_addresses = ips;
                                            }
                                            nm_for_events.update_network(net.clone()).await;
                                            info!("[ZGALAXY QUIC] Network {} updated (authorized: {})", nwid, authorized);

                                            // Apply managed address + routes to the host
                                            // adapter once per network (idempotent set).
                                            if authorized
                                                && !net.assigned_addresses.is_empty()
                                                && applied_networks.insert(nwid.clone())
                                            {
                                                let ip = &net.assigned_addresses[0];
                                                // Prefer the managed route that contains
                                                // the assigned IP; fall back to /24.
                                                let cidr = net
                                                    .routes
                                                    .iter()
                                                    .find(|r| r.via.is_none())
                                                    .map(|r| r.target.clone())
                                                    .unwrap_or_else(|| format!("{}/24", ip));
                                                tun_for_events
                                                    .assign_address(&cidr, Some(&net.mac))
                                                    .await;
                                                for route in &net.routes {
                                                    if route.target != cidr {
                                                        let _ = zgalaxy_rs::route_manager::RouteManager::add_route(
                                                            &tun_for_events.name,
                                                            &route.target,
                                                        );
                                                    }
                                                }
                                            }
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
                                    ControlMessage::Pong { sent_ms, .. } => {
                                        // Real RTT is measured at the sender:
                                        // receipt_now - original_sent_ms.
                                        let now = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis() as u64;
                                        let rtt = now.saturating_sub(sent_ms) as i32;
                                        if let Some(addr_str) =
                                            quic_engine.announced_address(remote).await
                                        {
                                            if let Ok(node) =
                                                zgalaxy_rs::identity::Address::from_str(&addr_str)
                                            {
                                                peers_for_events
                                                    .add_or_update_peer(
                                                        node,
                                                        zgalaxy_rs::peer::PeerRole::Leaf,
                                                        remote,
                                                        rtt,
                                                    )
                                                    .await;
                                                tracing::debug!(
                                                    "[ZGALAXY QUIC RTT] {} -> {}ms",
                                                    addr_str,
                                                    rtt
                                                );
                                            }
                                        }
                                    }
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
                        let frame = bytes::Bytes::from(frame);
                        let peers = quic_for_outbound.connected_peers().read().await;
                        for remote in peers.keys() {
                            if let Err(e) = quic_for_outbound.send_frame(*remote, frame.clone()).await {
                                warn!("[ZGALAXY QUIC] frame to {} dropped: {}", remote, e);
                            }
                        }
                    }
                });
                }

                // ب3: periodic RTT probes ride the reliable control streams;
                // Pong handling feeds real latency into PeerManager (ZTNET).
                // These pings double as NAT-binding keepalives in QUIC mode.
                let quic_for_rtt = Arc::clone(&quic);
                spawn_watched("quic-rtt-prober", async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(RTT_PROBE_INTERVAL_SECS));
                    loop {
                        interval.tick().await;
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let remotes: Vec<std::net::SocketAddr> = quic_for_rtt
                            .connected_peers()
                            .read()
                            .await
                            .keys()
                            .copied()
                            .collect();
                        for remote in remotes {
                            let msg = zgalaxy_rs::quic::control::ControlMessage::Ping {
                                nonce: rand::random::<u64>(),
                                sent_ms: now,
                            };
                            if let Err(e) = quic_for_rtt.send_control(remote, &msg).await {
                                tracing::debug!("[ZGALAXY QUIC RTT] probe to {} failed: {}", remote, e);
                            }
                        }
                    }
                });

                // Periodic network-config sync over reliable control streams.
                let nm_for_sync = network_manager.clone();
                let resolver_for_sync = resolver.clone();
                let quic_for_sync = Arc::clone(&quic);
                spawn_watched("quic-sync-loop", async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                    loop {
                        interval.tick().await;
                        let nets = nm_for_sync.list().await;
                        if nets.is_empty() {
                            tracing::debug!("[ZGALAXY QUIC SYNC] heartbeat: no joined networks");
                            continue;
                        }
                        let mut targets = resolver_for_sync.get_all_active_addresses().await;
                        if let Ok(extra) = std::env::var("ZGALAXY_EXTRA_ENDPOINTS") {
                            for ep in extra.split(',') {
                                let ep = ep.trim();
                                if ep.is_empty() {
                                    continue;
                                }
                                // SocketAddr::parse cannot resolve hostnames —
                                // dial names via DNS (docker service names etc.).
                                match ep.parse::<std::net::SocketAddr>() {
                                    Ok(addr) => targets.push(addr),
                                    Err(_) => match tokio::net::lookup_host(ep).await {
                                        Ok(mut addrs) => {
                                            if let Some(addr) = addrs.next() {
                                                targets.push(addr);
                                            }
                                        }
                                        Err(e) => {
                                            warn!("[ZGALAXY QUIC] cannot resolve extra endpoint '{}': {}", ep, e)
                                        }
                                    },
                                }
                            }
                        }
                        // Heartbeat: proves the sync task is alive and shows
                        // exactly what it is about to dial.
                        info!(
                            "[ZGALAXY QUIC SYNC] heartbeat: {} network(s), {} target(s): {:?}",
                            nets.len(),
                            targets.len(),
                            targets
                        );
                        let mut ok = 0usize;
                        let mut failed = 0usize;
                        for net in &nets {
                            for target in &targets {
                                // Per-target timeout: one dead root (e.g. the
                                // unreachable default domain) must not starve
                                // the remaining targets in the sequential loop.
                                let msg = zgalaxy_rs::quic::control::ControlMessage::NetworkConfigRequest {
                                    nwid: net.nwid.clone(),
                                    token: net.membership_token.clone(),
                                };
                                let req = quic_for_sync.send_control(*target, &msg);
                                if let Err(e) = tokio::time::timeout(
                                    std::time::Duration::from_secs(5),
                                    req,
                                )
                                .await
                                .map_err(|_| anyhow::anyhow!("timeout"))
                                .and_then(|r| r)
                                {
                                    failed += 1;
                                    tracing::debug!("[ZGALAXY QUIC] config request to {} for {} failed: {}", target, net.nwid, e);
                                } else {
                                    ok += 1;
                                }
                            }
                        }
                        info!(
                            "[ZGALAXY QUIC SYNC] cycle done: sent={} ok={} failed={}",
                            ok + failed,
                            ok,
                            failed
                        );
                    }
                });
            }
            Err(e) => {
                error!("[ZGALAXY QUIC] Failed to bind QUIC transport: {} — falling back to UDP", e);
            }
        }
    }

    if transport_mode != "quic" || !quic_started {
    nat_engine.clone().start_worker();
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
        spawn_watched("udp-sync-loop", async move {
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

/// Spawn an infinite background task whose death (panic or unexpected return)
/// is logged instead of passing silently — a panicked `tokio::spawn` task
/// otherwise just stops running with no trace at the spawn site.
fn spawn_watched(name: &'static str, fut: impl std::future::Future<Output = ()> + Send + 'static) {
    let handle = tokio::spawn(fut);
    tokio::spawn(async move {
        match handle.await {
            Ok(()) => warn!("[ZGALAXY WATCHDOG] task '{}' finished unexpectedly", name),
            Err(e) if e.is_panic() => {
                let payload = e.into_panic();
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "non-string panic payload".to_string());
                error!("[ZGALAXY WATCHDOG] task '{}' panicked: {}", name, msg);
            }
            Err(e) => error!("[ZGALAXY WATCHDOG] task '{}' was cancelled: {}", name, e),
        }
    });
}
