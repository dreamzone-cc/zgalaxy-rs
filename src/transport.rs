use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use bytes::Bytes;
use tracing::{info, debug, warn, error};
use anyhow::{Context, Result};

use crate::crypto::CryptoEngine;
use crate::identity::{Address, Identity};
use crate::packet::{Packet, PacketType};
use crate::peer::PeerManager;
use crate::resolver::DynamicDnsResolver;

/// Asynchronous UDP Transport and Wire Protocol Dispatcher
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
    identity: Identity,
    peer_manager: PeerManager,
    resolver: Arc<DynamicDnsResolver>,
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
        })
    }

    /// Start the asynchronous UDP receive and dispatch loop
    pub fn start_rx_loop(
        &self,
        tun_inbound_tx: mpsc::Sender<Vec<u8>>,
    ) {
        let socket = self.socket.clone();
        let identity = self.identity.clone();
        let peer_manager = self.peer_manager.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            info!("[ZGALAXY UDP RX] Native packet processing pipeline active.");

            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, src_addr)) => {
                        let data = Bytes::copy_from_slice(&buf[..len]);
                        if let Ok(packet) = Packet::decode(data) {
                            debug!("[ZGALAXY UDP PKT] Received {:?} from {} (src_node: {})", packet.packet_type, src_addr, packet.source);

                            match packet.packet_type {
                                PacketType::Echo => {
                                    // Respond with PONG
                                    let pong = Packet::new(
                                        packet.source,
                                        identity.address,
                                        packet.packet_id,
                                        PacketType::Pong,
                                        Bytes::new(),
                                    );
                                    let _ = socket.send_to(&pong.encode(), src_addr).await;
                                }
                                PacketType::Pong => {
                                    debug!("[ZGALAXY PONG] Received heartbeat response from {}", src_addr);
                                }
                                PacketType::Frame => {
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

    /// Send a packet to a target peer or root
    pub async fn send_packet(&self, packet: Packet, target_endpoint: SocketAddr) -> Result<()> {
        let encoded = packet.encode();
        self.socket.send_to(&encoded, target_endpoint).await?;
        Ok(())
    }
}
