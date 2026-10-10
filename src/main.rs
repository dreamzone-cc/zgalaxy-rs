use std::path::PathBuf;
use std::sync::Arc;
use bytes::Bytes;
use clap::Parser;
use tokio::fs;
use tokio::sync::mpsc;
use tracing::{info, warn, error};
use tracing_subscriber::FmtSubscriber;
use anyhow::{Context, Result};

use zgalaxy_rs::cli::Cli;
use zgalaxy_rs::config::LocalConfig;
use zgalaxy_rs::control_engine::{self, ControlEngine, InterfaceTarget, MeshContext};
use zgalaxy_rs::controller::EmbeddedController;
use zgalaxy_rs::controller_api::{AppState, ControllerServer};
use zgalaxy_rs::identity::Identity;
use zgalaxy_rs::interface_sync;
use zgalaxy_rs::mesh::capabilities::{CANONICAL_MTU, IPV6_MIN_MTU};
use zgalaxy_rs::mesh::{MeshTransport, PeerSessions, TransportKind};
use zgalaxy_rs::nat::NatTraversalEngine;
use zgalaxy_rs::network::NetworkManager;
use zgalaxy_rs::peer::PeerManager;
use zgalaxy_rs::quic::{QuicMesh, QuicTransport};
use zgalaxy_rs::resolver::DynamicDnsResolver;
use zgalaxy_rs::transport::{BridgeMode, UdpTransport};

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

    // ZeroTier daemon flags: -d (fork to background), -p<port> (port override),
    // and -C / --controller / --client (controller vs client mode).
    let mut daemonize = false;
    let mut port_override: Option<u16> = None;
    let mut controller_cli_override: Option<bool> = None;
    cli_args.retain(|a| {
        if a == "-d" {
            daemonize = true;
            false
        } else if a == "-C" || a == "--controller" || a == "--mode=controller" {
            controller_cli_override = Some(true);
            false
        } else if a == "--client" || a == "--mode=client" {
            controller_cli_override = Some(false);
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

    #[cfg(unix)]
    if daemonize {
        // Re-spawn without -d in a new process group (detached, stdio null)
        // and exit — matching the ZeroTier `-d` daemonize contract that the
        // ZGALAXY container entrypoint relies on. The port and mode flags must be
        // re-passed to the child: they were consumed from argv above.
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe().context("cannot resolve own executable")?;
        let mut child_args = cli_args.clone();
        if let Some(p) = port_override {
            child_args.push(format!("-p{}", p));
        }
        if let Some(ctrl) = controller_cli_override {
            if ctrl {
                child_args.push("--controller".to_string());
            } else {
                child_args.push("--client".to_string());
            }
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
    #[cfg(not(unix))]
    if daemonize {
        warn!("-d (daemonize) is not supported on this platform; running in the foreground");
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
                // Match clap convention: bad usage is an exit-code failure,
                // never a silent success (rc=0 broke engine B1 delegation).
                std::process::exit(2);
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
        set_authtoken_permissions(&auth_path);
        fs::read_to_string(&auth_path).await?.trim().to_string()
    } else {
        let token = hex::encode(rand::random::<[u8; 16]>());
        let _ = fs::write(&auth_path, &token).await;
        set_authtoken_permissions(&auth_path);
        token
    };

    let peer_manager = PeerManager::new();
    let network_manager = NetworkManager::new();
    network_manager
        .set_node_address(identity.address.to_string())
        .await;
    network_manager
        .set_working_dir(working_dir.clone())
        .await;
    let _ = network_manager.load_from_disk(&working_dir).await;

    // Apply CLI controller mode override if present
    if let Some(ctrl) = controller_cli_override {
        local_config.controller_enabled = ctrl;
    }

    if local_config.controller_enabled {
        info!("[ZGALAXY MODE] Running in CONTROLLER Mode (ZTNet & Controller Management Active)");
    } else {
        info!("[ZGALAXY MODE] Running in CLIENT Mode (Standard Default)");
    }

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

    // NAT traversal / path keep-alive engine. With a mesh transport it only
    // re-probes paths without a live session (the RTT prober covers live
    // ones); with the legacy bridge it sends wire echoes.
    let nat_engine = Arc::new(NatTraversalEngine::new(peer_manager.clone()));

    // Initialize Embedded Controller (Pure Rust, FileDB compatible with ZeroTier controller.d/)
    let controller = EmbeddedController::new(identity.clone(), working_dir.clone());
    if local_config.controller_enabled {
        let _ = controller.init().await;
        info!("[ZGALAXY CONTROLLER] Embedded controller database initialized under {:?}", working_dir.join("controller.d"));
    }

    // Start Local REST API Controller Plane on the same port as the UDP
    // transport (ZeroTier binds its control plane and wire socket together).
    let api_port = local_config.port;
    let (join_tx, mut join_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let app_state = AppState {
        identity: identity.clone(),
        auth_token,
        allow_management_from: local_config.allow_management_from.clone(),
        peer_manager: peer_manager.clone(),
        network_manager: network_manager.clone(),
        controller: controller.clone(),
        controller_enabled: local_config.controller_enabled,
        resolver: resolver.clone(),
        join_trigger: Some(join_tx),
    };

    tokio::spawn(async move {
        if let Err(e) = ControllerServer::start(app_state, api_port).await {
            error!("[ZGALAXY REST API ERROR] {}", e);
        }
    });

    // Spawn Dynamic Sync Provider Background Worker if configured
    if let Some(sync_cfg) = local_config.sync_provider.clone() {
        if sync_cfg.enabled {
            let sync_client = zgalaxy_rs::SyncProviderClient::new(sync_cfg.clone());
            let sync_working_dir = working_dir.clone();
            let sync_identity = identity.clone();
            let sync_pm = peer_manager.clone();
            tokio::spawn(async move {
                info!("[ZGALAXY SYNC] Starting background sync worker for {}", sync_cfg.server_url);
                let interval = std::time::Duration::from_secs(sync_cfg.sync_interval_minutes.max(1) * 60);
                loop {
                    if let Err(e) = sync_client.execute_sync(&sync_working_dir, &sync_identity, Some(&sync_pm)).await {
                        warn!("[ZGALAXY SYNC WORKER] Sync attempt failed: {:?}", e);
                    }
                    tokio::time::sleep(interval).await;
                }
            });
        }
    }

    // Mesh transport selection (settings.transportMode). The transport is
    // bound FIRST: its capabilities size the virtual interface.
    let requested = local_config.transport_mode();
    let transport_kind = if requested.is_available() {
        requested
    } else {
        warn!(
            "[ZGALAXY MESH] transportMode '{}' has no backend in this build yet (docs/GTP-INTEGRATION.md) — using quic",
            requested
        );
        TransportKind::Quic
    };

    let mut quic_transport: Option<Arc<QuicTransport>> = None;
    if transport_kind == TransportKind::Quic {
        let bind_v6 = std::net::SocketAddr::from(([0u8; 16], local_config.port));
        let bind_v4 = std::net::SocketAddr::from(([0, 0, 0, 0], local_config.port));
        let bound = QuicTransport::bind(bind_v6).or_else(|e_v6| {
            info!(
                "[ZGALAXY QUIC] IPv6 bind on [::]:{} unavailable ({:#}), falling back to 0.0.0.0:{}",
                local_config.port, e_v6, local_config.port
            );
            QuicTransport::bind(bind_v4)
        });
        match bound {
            Ok(quic) => quic_transport = Some(Arc::new(quic)),
            Err(e) => error!("[ZGALAXY QUIC] Failed to bind QUIC transport: {:#} — falling back to UDP", e),
        }
    }
    let mesh: Option<Arc<dyn MeshTransport>> = quic_transport
        .as_ref()
        .map(|q| Arc::new(QuicMesh(Arc::clone(q))) as Arc<dyn MeshTransport>);

    // Canonical ZeroTier MTU (2800, ≥ 1280 for RFC 8200 IPv6), capped by what
    // the transport can carry. QUIC sends frames above the datagram limit on
    // a stream, so it carries the full 2800.
    let tun_mtu = mesh
        .as_ref()
        .map(|m| m.capabilities().interface_mtu())
        .unwrap_or(CANONICAL_MTU);
    if tun_mtu < IPV6_MIN_MTU {
        warn!("[ZGALAXY MESH] interface MTU {} is below the IPv6 minimum of {}", tun_mtu, IPV6_MIN_MTU);
    }

    // ب2: one shared L2 learning table across inbound events, the outbound
    // forwarder and the prober.
    let l2_shared = zgalaxy_rs::L2Switch::new();
    let (tun_inbound_tx, tun_inbound_rx) = mpsc::channel::<Bytes>(1024);
    let (tun_outbound_tx, tun_outbound_rx) = mpsc::channel::<Bytes>(1024);

    let mut tun_device = zgalaxy_rs::tun::TunDevice::new("zgalaxy0", tun_mtu);
    let _ = tun_device.create_and_bind(None).await;
    // Checked before the packet loop takes the kernel handle out.
    let interface = tun_device.is_live().then(|| InterfaceTarget {
        device: tun_device.name.clone(),
        mtu_cap: tun_mtu,
    });
    let tun_arc = Arc::new(tun_device);
    tun_arc.start_packet_loop(tun_inbound_rx, tun_outbound_tx);

    // Boot auto-recovery: re-apply the cached config of every connected
    // network, then let the sync loop refresh it from the controller.
    for net in network_manager.list().await {
        if let Some(iface) = &interface {
            let plan = interface_sync::plan_boot(&net);
            if !plan.is_empty() {
                interface_sync::apply(&iface.device, plan).await;
                info!(
                    "[ZGALAXY BOOT] Pre-configured interface for active network {} (IPs: {:?})",
                    net.nwid, net.assigned_addresses
                );
            }
        }
    }

    let sync_now = Arc::new(tokio::sync::Notify::new());
    // Headless: the packet loop never started, so there is nothing to forward.
    let mut tun_outbound_rx = interface.is_some().then_some(tun_outbound_rx);

    if let (Some(quic), Some(mesh)) = (&quic_transport, &mesh) {
        nat_engine.set_mesh_transport(Arc::clone(mesh)).await;

        let (events_tx, events_rx) = mpsc::channel(1024);
        let quic_runner = Arc::clone(quic);
        tokio::spawn(async move {
            if let Err(e) = quic_runner.run(events_tx).await {
                error!("[ZGALAXY QUIC] Accept loop terminated: {:#}", e);
            }
        });

        let ctx = MeshContext {
            transport: Arc::clone(mesh),
            sessions: PeerSessions::new(),
            identity: identity.clone(),
            controller: controller.clone(),
            is_controller: local_config.controller_enabled,
            networks: network_manager.clone(),
            peers: peer_manager.clone(),
            l2: l2_shared.clone(),
            tun_tx: tun_inbound_tx.clone(),
            interface: interface.clone(),
            sync_now: Arc::clone(&sync_now),
        };
        info!(
            "[ZGALAXY MESH] {} transport active (interface MTU {}, data plane {})",
            mesh.kind(),
            tun_mtu,
            if interface.is_some() { "live" } else { "headless" }
        );
        spawn_watched("mesh-engine", ControlEngine::new(ctx.clone()).run(events_rx));
        if let Some(rx) = tun_outbound_rx.take() {
            spawn_watched("mesh-outbound", control_engine::forward_outbound(ctx.clone(), rx));
        }
        spawn_watched("mesh-rtt-prober", control_engine::rtt_prober(ctx.clone()));
        spawn_watched("mesh-config-sync", control_engine::config_sync_loop(ctx, resolver.clone()));
    }

    // Legacy UDP wire bridge. Sole transport when no mesh transport is up;
    // beside one it only does controller duties (see BridgeMode).
    let run_bridge = mesh.is_none() || local_config.udp_bridge_enabled();
    let mut udp_bridge: Option<Arc<UdpTransport>> = None;
    if run_bridge {
        match UdpTransport::bind(local_config.port, identity.clone(), peer_manager.clone(), resolver.clone()).await {
            Ok(transport) => {
                let transport = Arc::new(transport);
                let mode = if mesh.is_some() { BridgeMode::ControllerOnly } else { BridgeMode::Full };
                transport.set_controller(controller.clone()).await;
                transport.set_network_manager(network_manager.clone()).await;
                transport.start_rx_loop(tun_inbound_tx.clone(), mode);
                nat_engine.set_transport(Arc::clone(&transport)).await;

                if let Some(mut rx) = tun_outbound_rx.take() {
                    let tp = Arc::clone(&transport);
                    spawn_watched("udp-outbound", async move {
                        while let Some(frame) = rx.recv().await {
                            let _ = tp.broadcast_frame(frame).await;
                        }
                    });
                }

                // Join requests over the wire bridge so controllers register
                // members for ztnet, in parallel with the mesh transport.
                let nm_for_sync = network_manager.clone();
                let tp_for_sync = Arc::clone(&transport);
                spawn_watched("udp-sync-loop", async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                    loop {
                        interval.tick().await;
                        for net in nm_for_sync.list().await.into_iter().filter(|n| n.connected) {
                            let _ = tp_for_sync.send_network_config_request(&net.nwid).await;
                        }
                    }
                });
                udp_bridge = Some(transport);
            }
            Err(e) => warn!("[ZGALAXY UDP] Could not bind UDP port {} ({:#}), running in client API mode.", local_config.port, e),
        }
    }
    nat_engine.clone().start_worker();

    // Instant join: a REST join wakes the mesh sync loop and fires a wire
    // request right away instead of waiting for the next cycle.
    let sync_for_join = Arc::clone(&sync_now);
    spawn_watched("instant-join-trigger", async move {
        while let Some(nwid) = join_rx.recv().await {
            info!("[ZGALAXY INSTANT JOIN] config request dispatched immediately for {}", nwid);
            sync_for_join.notify_one();
            if let Some(tp) = &udp_bridge {
                let _ = tp.send_network_config_request(&nwid).await;
            }
        }
    });

    // Auto-join configured networks from local.conf if not already loaded from networks.d/
    for nwid in &local_config.auto_join_networks {
        if network_manager.get(nwid).await.is_none() {
            info!("[ZGALAXY AUTO-JOIN] Joining persistent network from local.conf: {}", nwid);
            let _ = network_manager.join(nwid).await;
        }
    }

    info!("ZGALAXY Client Daemon is fully active, listening, and serving.");

    // Keep running until signal
    tokio::signal::ctrl_c().await?;
    info!("Shutting down ZGALAXY Client Daemon cleanly...");
    Ok(())
}

/// Restrict private key files (identity.secret) to owner-only permissions (0600).
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

/// Ensure authtoken.secret is readable by local user desktop apps and CLI (0644).
#[cfg(unix)]
fn set_authtoken_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o644);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_authtoken_permissions(_path: &std::path::Path) {}

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
