use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use bytes::Bytes;
use tracing::{info, debug, error, warn};
use anyhow::{Context, Result};

use crate::crypto::CryptoEngine;
use crate::identity::{Address, Identity};
use crate::packet::{Packet, PacketType};
use crate::peer::PeerManager;
use crate::resolver::DynamicDnsResolver;
use crate::controller::EmbeddedController;
use crate::network::NetworkManager;

/// Asynchronous UDP Transport and Wire Protocol Dispatcher
#[derive(Clone)]
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
    identity: Identity,
    #[allow(dead_code)]
    peer_manager: PeerManager,
    #[allow(dead_code)]
    resolver: Arc<DynamicDnsResolver>,
    #[allow(dead_code)]
    crypto: Arc<CryptoEngine>,
    controller: Arc<RwLock<Option<EmbeddedController>>>,
    network_manager: Arc<RwLock<Option<NetworkManager>>>,
}

impl UdpTransport {
    pub async fn bind(
        port: u16,
        identity: Identity,
        peer_manager: PeerManager,
        resolver: Arc<DynamicDnsResolver>,
    ) -> Result<Self> {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let socket = UdpSocket::bind(addr)
            .await
            .with_context(|| format!("Failed to bind UDP socket on {}", addr))?;

        info!("[ZGALAXY UDP TRANSPORT] Bound high-performance UDP router on {}", addr);

        Ok(UdpTransport {
            socket: Arc::new(socket),
            identity,
            peer_manager,
            resolver,
            crypto: Arc::new(CryptoEngine::new()),
            controller: Arc::new(RwLock::new(None)),
            network_manager: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn set_controller(&self, controller: EmbeddedController) {
        let mut ctrl = self.controller.write().await;
        *ctrl = Some(controller);
    }

    pub async fn set_network_manager(&self, network_manager: NetworkManager) {
        let mut nm = self.network_manager.write().await;
        *nm = Some(network_manager);
    }

    /// Start the asynchronous UDP receive, decrypt, and dispatch loop
    pub fn start_rx_loop(
        &self,
        tun_inbound_tx: mpsc::Sender<Vec<u8>>,
    ) {
        let socket = self.socket.clone();
        let identity = self.identity.clone();
        let peer_manager = self.peer_manager.clone();
        let controller_lock = self.controller.clone();
        let network_manager_lock = self.network_manager.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            info!("[ZGALAXY UDP RX] Native packet processing pipeline active.");

            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, src_addr)) => {
                        let data = Bytes::copy_from_slice(&buf[..len]);
                        if let Ok(packet) = Packet::decode(data) {
                            debug!("[ZGALAXY UDP PKT] Received {:?} from {} (src_node: {})", packet.packet_type, src_addr, packet.source);

                            if packet.source != Address::NULL && packet.source != identity.address {
                                // 1. Update PeerManager with active path and latency for ZTNET
                                peer_manager.add_or_update_peer(packet.source, crate::peer::PeerRole::Leaf, src_addr, 5).await;

                                // 2. Update Controller member lastSeen timestamp and physical address
                                let ctrl_guard = controller_lock.read().await;
                                if let Some(ref ctrl) = *ctrl_guard {
                                    let member_id = format!("{}", packet.source);
                                    let path_str = format!("{}/{}", src_addr.ip(), src_addr.port());
                                    ctrl.touch_member_last_seen(&member_id, &path_str).await;
                                }
                            }

                            match packet.packet_type {
                                PacketType::Echo => {
                                    // Respond with real PONG packet
                                    let pong = Packet::new(
                                        packet.source,
                                        identity.address,
                                        packet.packet_id,
                                        PacketType::Pong,
                                        Bytes::from_static(b"ZGALAXY_PONG"),
                                    );
                                    let _ = socket.send_to(&pong.encode(), src_addr).await;
                                }
                                PacketType::Pong => {
                                    debug!("[ZGALAXY PONG] Received heartbeat response from {}", src_addr);
                                }
                                PacketType::Frame | PacketType::ExtFrame => {
                                    // Forward decrypted Ethernet frame to TUN adapter
                                    let _ = tun_inbound_tx.send(packet.payload.to_vec()).await;
                                }
                                PacketType::Hello => {
                                    let ok = Packet::new(
                                        packet.source,
                                        identity.address,
                                        packet.packet_id,
                                        PacketType::Ok,
                                        Bytes::from_static(b"ZGALAXY_OK"),
                                    );
                                    let _ = socket.send_to(&ok.encode(), src_addr).await;
                                }
                                PacketType::NetworkConfigRequest | PacketType::NetworkCredentials => {
                                    if packet.payload.len() >= 8 {
                                        let nwid_u64 = u64::from_be_bytes(packet.payload[0..8].try_into().unwrap_or_default());
                                        let nwid_hex = format!("{:016x}", nwid_u64);
                                        let member_id = format!("{}", packet.source);

                                        let ctrl_guard = controller_lock.read().await;
                                        if let Some(ref ctrl) = *ctrl_guard {
                                            info!("[ZGALAXY WIRE CONTROLLER] Processing join request for member {} into network {}", member_id, nwid_hex);
                                            let _ = ctrl.register_join_request(&nwid_hex, &member_id, None).await;

                                            if let Some(net) = ctrl.get_network(&nwid_hex).await {
                                                let member_opt = ctrl.get_member(&nwid_hex, &member_id).await;
                                                let is_auth = member_opt.as_ref().map(|m| m.authorized).unwrap_or(false);
                                                let ips = member_opt.map(|m| m.ip_assignments).unwrap_or_default();

                                                let config_resp = serde_json::json!({
                                                    "nwid": nwid_hex,
                                                    "name": net.name,
                                                    "authorized": is_auth,
                                                    "ipAssignments": ips,
                                                    "routes": net.routes,
                                                    "mtu": net.mtu
                                                });

                                                let resp_bytes = Bytes::from(serde_json::to_vec(&config_resp).unwrap_or_default());
                                                let config_pkt = Packet::new(
                                                    packet.source,
                                                    identity.address,
                                                    packet.packet_id,
                                                    PacketType::NetworkConfig,
                                                    resp_bytes,
                                                );
                                                let _ = socket.send_to(&config_pkt.encode(), src_addr).await;
                                            }
                                        }
                                    }
                                }
                                PacketType::NetworkConfig => {
                                    if let Ok(config_val) = serde_json::from_slice::<serde_json::Value>(&packet.payload) {
                                        if let Some(nwid) = config_val.get("nwid").and_then(|v| v.as_str()) {
                                            let authorized = config_val.get("authorized").and_then(|v| v.as_bool()).unwrap_or(false);
                                            let ips: Vec<String> = config_val.get("ipAssignments")
                                                .and_then(|v| v.as_array())
                                                .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                                                .unwrap_or_default();

                                            let nm_guard = network_manager_lock.read().await;
                                            if let Some(ref nm) = *nm_guard {
                                                if let Some(mut net) = nm.get(nwid).await {
                                                    net.status = if authorized { crate::network::NetworkStatus::Ok } else { crate::network::NetworkStatus::AccessDenied };
                                                    if !ips.is_empty() {
                                                        net.assigned_addresses = ips;
                                                    }
                                                    nm.update_network(net).await;
                                                    info!("[ZGALAXY CLIENT NETWORK] Updated network {} (authorized: {})", nwid, authorized);
                                                }
                                            }
                                        }
                                    }
                                }
                                PacketType::Rendezvous => {
                                    debug!("[ZGALAXY RENDEZVOUS] Handling P2P mediation request from {}", src_addr);
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        error!("[ZGALAXY UDP ERROR] Socket receive error: {:?}", e);
                    }
                }
            }
        });
    }

    /// Send a packet directly to a target peer or root
    pub async fn send_packet(&self, packet: Packet, target_endpoint: SocketAddr) -> Result<()> {
        let encoded = packet.encode();
        self.socket.send_to(&encoded, target_endpoint).await?;
        Ok(())
    }

    /// Transmit a keepalive ECHO probe to an endpoint
    pub async fn send_echo(&self, target_endpoint: SocketAddr) -> Result<()> {
        let echo_pkt = Packet::new(
            crate::identity::Address::NULL,
            self.identity.address,
            rand::random::<u64>(),
            PacketType::Echo,
            Bytes::from_static(b"ZGALAXY_ECHO_PROBE"),
        );
        self.send_packet(echo_pkt, target_endpoint).await
    }

    /// Broadcast an encapsulated Layer-2 Frame packet to all active peers
    pub async fn broadcast_frame(&self, frame_bytes: Vec<u8>) -> Result<()> {
        let active_addrs = self.resolver.get_all_active_addresses().await;
        if active_addrs.is_empty() {
            warn!("[ZGALAXY UDP TX] No active remote endpoints available for frame broadcast.");
            return Ok(());
        }

        let frame_pkt = Packet::new(
            crate::identity::Address::NULL,
            self.identity.address,
            rand::random::<u64>(),
            PacketType::Frame,
            Bytes::from(frame_bytes),
        );

        let encoded = frame_pkt.encode();
        for addr in active_addrs {
            let _ = self.socket.send_to(&encoded, addr).await;
        }

        Ok(())
    }

    /// Transmit a NetworkConfigRequest packet for a joined network to controller endpoints and roots.
    pub async fn send_network_config_request(&self, nwid_str: &str) -> Result<()> {
        let clean_nwid = nwid_str.trim().to_lowercase();
        let nwid_u64 = u64::from_str_radix(&clean_nwid, 16).unwrap_or(0);
        let nwid_bytes = nwid_u64.to_be_bytes();

        let controller_addr_str = if clean_nwid.len() >= 10 { &clean_nwid[..10] } else { "0000000000" };
        let controller_addr = Address::from_str(controller_addr_str).unwrap_or(Address::NULL);

        let req_pkt = Packet::new(
            controller_addr,
            self.identity.address,
            rand::random::<u64>(),
            PacketType::NetworkConfigRequest,
            Bytes::copy_from_slice(&nwid_bytes),
        );

        let encoded = req_pkt.encode();

        // 1. Send to all active resolver addresses
        let addrs = self.resolver.get_all_active_addresses().await;
        for addr in addrs {
            let _ = self.socket.send_to(&encoded, addr).await;
        }

        // 2. Send to extra static endpoints configured via ZGALAXY_EXTRA_ENDPOINTS
        //    (comma-separated host:port list, e.g. "root1.example.com:9993,10.0.0.5:9993")
        if let Ok(extra) = std::env::var("ZGALAXY_EXTRA_ENDPOINTS") {
            for ep in extra.split(',') {
                let ep = ep.trim();
                if ep.is_empty() {
                    continue;
                }
                match SocketAddr::from_str(ep) {
                    Ok(addr) => {
                        let _ = self.socket.send_to(&encoded, addr).await;
                    }
                    Err(_) => warn!("[ZGALAXY UDP TX] Ignoring invalid endpoint '{}' in ZGALAXY_EXTRA_ENDPOINTS", ep),
                }
            }
        }

        Ok(())
    }
}
