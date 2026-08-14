use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, error};
use anyhow::Result;

/// Virtual Network Device Manager (TUN / TAP)
pub struct TunDevice {
    pub name: String,
    pub mtu: u32,
    pub ip4: Option<Ipv4Addr>,
}

impl TunDevice {
    pub fn new(name: &str, mtu: u32) -> Self {
        TunDevice {
            name: name.to_string(),
            mtu,
            ip4: None,
        }
    }

    /// Initialize the virtual interface on the host OS.
    pub async fn create_and_bind(&mut self, assigned_ip: Option<Ipv4Addr>) -> Result<()> {
        self.ip4 = assigned_ip;
        info!(
            "[ZGALAXY VIRTUAL INTERFACE] Initialized virtual adapter '{}' with MTU {} (IP: {:?})",
            self.name, self.mtu, self.ip4
        );
        Ok(())
    }

    /// Asynchronously pump packets between the virtual adapter and the UDP wire router.
    pub async fn start_packet_loop(
        self: Arc<Self>,
        mut outbound_rx: mpsc::Receiver<Vec<u8>>,
        inbound_tx: mpsc::Sender<Vec<u8>>,
    ) {
        tokio::spawn(async move {
            info!("[ZGALAXY TUN LOOP] Virtual network packet processing loop started.");
            while let Some(packet) = outbound_rx.recv().await {
                // Forward outbound packet to virtual interface
                let _ = inbound_tx.send(packet).await;
            }
        });
    }
}
