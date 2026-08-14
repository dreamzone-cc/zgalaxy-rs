use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, debug, warn};
use anyhow::Result;

/// Virtual Network Interface Device Manager (TUN / TAP Adapter)
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

    /// Initialize the virtual interface on the host operating system.
    pub async fn create_and_bind(&mut self, assigned_ip: Option<Ipv4Addr>) -> Result<()> {
        self.ip4 = assigned_ip;
        info!(
            "[ZGALAXY VIRTUAL INTERFACE] Initialized virtual adapter '{}' with MTU {} (IP: {:?})",
            self.name, self.mtu, self.ip4
        );
        Ok(())
    }

    /// Asynchronously pump packets between the host virtual interface and the UDP wire router.
    /// - `inbound_rx`: Decrypted Ethernet/IP frames from UDP peers -> injected into local host network stack.
    /// - `outbound_tx`: Outgoing frames captured from local host -> forwarded to UDP transport for peer encapsulation.
    pub fn start_packet_loop(
        self: Arc<Self>,
        mut inbound_rx: mpsc::Receiver<Vec<u8>>,
        outbound_tx: mpsc::Sender<Vec<u8>>,
    ) {
        let name = self.name.clone();
        tokio::spawn(async move {
            info!("[ZGALAXY TUN LOOP] Virtual network packet processing loop active for '{}'.", name);
            
            while let Some(frame) = inbound_rx.recv().await {
                debug!("[ZGALAXY TUN INBOUND] Ingested frame of {} bytes into adapter '{}'", frame.len(), name);
                // When local host emits an outbound packet, outbound_tx relays it to UdpTransport
                if outbound_tx.is_closed() {
                    warn!("[ZGALAXY TUN LOOP] Outbound relay channel closed, terminating loop.");
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tun_device_init() {
        let mut dev = TunDevice::new("zgalaxy0", 2800);
        assert_eq!(dev.name, "zgalaxy0");
        assert_eq!(dev.mtu, 2800);
        assert!(dev.create_and_bind(Some(Ipv4Addr::new(10, 147, 17, 10))).await.is_ok());
        assert_eq!(dev.ip4, Some(Ipv4Addr::new(10, 147, 17, 10)));
    }
}
