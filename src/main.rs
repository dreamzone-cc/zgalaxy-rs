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
    let local_config = LocalConfig::load(&working_dir).await;

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
    let _ = resolver.load_sources(&working_dir).await;
    resolver.clone().start_worker();

    // Initialize NAT Traversal & Hole-Punching Engine
    let nat_engine = Arc::new(NatTraversalEngine::new(peer_manager.clone()));
    nat_engine.clone().start_worker();

    // Initialize Embedded Controller (Pure Rust, FileDB compatible with ZeroTier controller.d/)
    let controller = EmbeddedController::new(identity.clone(), working_dir.clone());
    let _ = controller.init().await;

    // Start Local REST API Controller Plane (Port 9993)
    let app_state = AppState {
        identity: identity.clone(),
        auth_token,
        peer_manager: peer_manager.clone(),
        network_manager: network_manager.clone(),
        controller,
        resolver: resolver.clone(),
    };

    tokio::spawn(async move {
        if let Err(e) = ControllerServer::start(app_state, 9993).await {
            error!("[ZGALAXY REST API ERROR] {}", e);
        }
    });

    // Initialize High-Performance UDP Transport Loop & Virtual Adapter Routing
    let (tun_inbound_tx, tun_inbound_rx) = mpsc::channel::<Vec<u8>>(1024);
    let (tun_outbound_tx, mut tun_outbound_rx) = mpsc::channel::<Vec<u8>>(1024);

    let mut tun_device = zgalaxy_rs::tun::TunDevice::new("zgalaxy0", 2800);
    let _ = tun_device.create_and_bind(None).await;
    let tun_arc = Arc::new(tun_device);
    tun_arc.start_packet_loop(tun_inbound_rx, tun_outbound_tx);

    if let Ok(transport) = UdpTransport::bind(local_config.port, identity.clone(), peer_manager.clone(), resolver.clone()).await {
        let transport_arc = Arc::new(transport);
        transport_arc.start_rx_loop(tun_inbound_tx);
        nat_engine.set_transport(transport_arc.clone()).await;

        // Relay host outbound frames captured from TUN into UDP wire transport
        let tp_for_outbound = transport_arc.clone();
        tokio::spawn(async move {
            while let Some(frame) = tun_outbound_rx.recv().await {
                let _ = tp_for_outbound.broadcast_frame(frame).await;
            }
        });
    } else {
        warn!("[ZGALAXY UDP] Could not bind UDP port {}, running in client API mode.", local_config.port);
    }

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
