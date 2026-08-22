//! Virtual network interface — real TAP (Layer 2) data plane.
//!
//! The device is an Ethernet (TAP) adapter, not L3 TUN: the QUIC datagram
//! path carries whole Ethernet frames, which keeps ARP/NDP and LAN-style
//! broadcast working end-to-end (required for LAN gaming — see
//! docs/REFERENCE_ANALYSIS.md §5).
//!
//! Privileges: creating /dev/net/tun requires CAP_NET_ADMIN. Without it the
//! daemon degrades gracefully to headless mode (controller/REST still serve —
//! this is how the ztnet controller container keeps working without the
//! data plane until privileges are provided).

use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Virtual Network Interface Device Manager (TAP adapter)
pub struct TunDevice {
    pub name: String,
    pub mtu: u32,
    pub ip4: Option<Ipv4Addr>,
    /// Kernel Ethernet frame reader/writer when the TAP device exists.
    /// (Mutex so the handle can be taken out of an Arc<TunDevice>.)
    device: std::sync::Mutex<Option<tun::AsyncDevice>>,
}

impl TunDevice {
    pub fn new(name: &str, mtu: u32) -> Self {
        TunDevice {
            name: name.to_string(),
            mtu,
            ip4: None,
            device: std::sync::Mutex::new(None),
        }
    }

    /// True when a kernel TAP device is attached (i.e. the data plane is live).
    pub fn is_live(&self) -> bool {
        self.device.lock().map(|d| d.is_some()).unwrap_or(false)
    }

    /// Create the kernel TAP (Layer 2) interface.
    ///
    /// NOTE: the MTU must leave room for the 14-byte Ethernet header inside a
    /// single QUIC datagram (max 1200 bytes on the wire), hence TAP MTU is
    /// capped at 1186 by callers in QUIC mode.
    pub async fn create_and_bind(&mut self, assigned_ip: Option<Ipv4Addr>) -> anyhow::Result<()> {
        self.ip4 = assigned_ip;
        self.create_device().await
    }

    async fn create_device(&mut self) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            let mut config = tun::Configuration::default();
            config
                .name(&self.name)
                .layer(tun::Layer::L2) // Ethernet frames — ARP/broadcast flow through
                .mtu(self.mtu as i32)
                .up();
            match tun::create_as_async(&config) {
                Ok(dev) => {
                    info!(
                        "[ZGALAXY VIRTUAL INTERFACE] TAP adapter '{}' created (L2, MTU {})",
                        self.name, self.mtu
                    );
                    *self.device.lock().unwrap() = Some(dev);
                    Ok(())
                }
                Err(e) => {
                    // Missing CAP_NET_ADMIN or /dev/net/tun — degrade to
                    // headless so the controller/REST plane keeps serving.
                    warn!(
                        "[ZGALAXY VIRTUAL INTERFACE] Cannot create TAP adapter '{}' ({}). \
                         Running headless — data plane inactive until restarted with \
                         CAP_NET_ADMIN and /dev/net/tun.",
                        self.name, e
                    );
                    *self.device.lock().unwrap() = None;
                    Ok(())
                }
            }
        }
        #[cfg(not(unix))]
        {
            warn!(
                "[ZGALAXY VIRTUAL INTERFACE] TAP adapter not yet implemented for this OS — headless."
            );
            *self.device.lock().unwrap() = None;
            Ok(())
        }
    }

    /// Apply a managed IPv4 address (CIDR) and interface MAC to the adapter.
    /// Both are idempotent at the call site (logged, never fatal).
    pub async fn assign_address(&self, ip_with_cidr: &str, mac: Option<&str>) {
        if let Some(mac) = mac {
            crate::route_manager::RouteManager::set_mac(&self.name, mac);
        }
        if let Err(e) = crate::route_manager::RouteManager::configure_interface_ip(&self.name, ip_with_cidr) {
            tracing::warn!("[ZGALAXY VIRTUAL INTERFACE] could not assign {}: {}", ip_with_cidr, e);
        }
    }

    /// Pump Ethernet frames between the kernel adapter and the mesh.
    /// - `inbound_rx`: decrypted Ethernet frames from peers -> written into
    ///   the host stack through the TAP device.
    /// - `outbound_tx`: frames read from the TAP device -> forwarded to the
    ///   transport for peer encapsulation.
    pub fn start_packet_loop(
        self: &Arc<Self>,
        mut inbound_rx: mpsc::Receiver<Vec<u8>>,
        outbound_tx: mpsc::Sender<Vec<u8>>,
    ) {
        let name = self.name.clone();

        // Move the kernel handle out — it is consumed by the pump tasks.
        let device = self
            .device
            .lock()
            .ok()
            .and_then(|mut d| d.take());
        let Some(device) = device else {
            warn!(
                "[ZGALAXY TUN LOOP] No kernel adapter '{}' — data-plane loop not started (headless).",
                name
            );
            return;
        };

        // Split the duplex fd: reader task -> mesh, writer task <- mesh.
        // tokio's split gives owned halves without needing Clone.
        let (reader_half, mut writer) = tokio::io::split(device);
        let mut reader = reader_half;
        let write_name = name.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(frame) = inbound_rx.recv().await {
                if let Err(e) = writer.write_all(&frame).await {
                    debug!(
                        "[ZGALAXY TUN WRITE] dropped frame ({}B) on '{}': {}",
                        frame.len(),
                        write_name,
                        e
                    );
                }
            }
        });

        // Reader task with a bounded read buffer sized to the MTU + headroom.
        let mut buf = vec![0u8; (self.mtu as usize) + 128];
        let read_name = name.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            info!(
                "[ZGALAXY TUN LOOP] Ethernet frame pump active for '{}'",
                read_name
            );
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => {
                        debug!("[ZGALAXY TUN READ] adapter '{}' closed", read_name);
                        break;
                    }
                    Ok(n) => {
                        if outbound_tx.send(buf[..n].to_vec()).await.is_err() {
                            debug!("[ZGALAXY TUN READ] transport channel closed; stopping pump");
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("[ZGALAXY TUN READ] read error on '{}': {}", read_name, e);
                        break;
                    }
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
        // Without CAP_NET_ADMIN this must degrade to headless, not fail.
        assert!(dev.create_and_bind(Some(Ipv4Addr::new(10, 147, 17, 10))).await.is_ok());
        assert_eq!(dev.ip4, Some(Ipv4Addr::new(10, 147, 17, 10)));
    }

    /// Requires CAP_NET_ADMIN + CAP_NET_RAW:
    /// `sudo ... cargo test test_tap_device_real_loopback -- --ignored --nocapture`
    ///
    /// Verifies BOTH directions of the data plane with a real kernel TAP
    /// adapter and a raw AF_PACKET socket on the interface:
    ///  1) host→mesh: AF_PACKET transmits a frame onto 'zgtest0' — the TAP
    ///     reader task must surface it on the outbound channel;
    ///  2) mesh→host: a frame injected through the inbound channel is written
    ///     into the TAP — AF_PACKET must observe it on the interface.
    #[tokio::test]
    #[ignore]
    async fn test_tap_device_real_loopback() {
        use nix::sys::socket::{bind, recvfrom, sendto, socket, AddressFamily, SockFlag, SockType, SockaddrLike};
        use std::os::unix::io::AsRawFd;

        let mut dev = TunDevice::new("zgtest0", 1186);
        dev.create_and_bind(None).await.unwrap();
        assert!(dev.is_live(), "TAP creation requires CAP_NET_ADMIN");

        let (inbound_tx, inbound_rx) = mpsc::channel(8);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let arc = Arc::new(dev);
        arc.start_packet_loop(inbound_rx, outbound_tx);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Raw Ethernet socket bound to the adapter.
        let fd = socket(
            AddressFamily::Packet,
            SockType::Raw,
            SockFlag::empty(),
            None,
        )
        .expect("AF_PACKET socket requires CAP_NET_RAW");
        let ifindex = nix::net::if_::if_nametoindex("zgtest0").unwrap() as i32;
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = 0x0800u16.to_be(); // ETH_P_IP
        sll.sll_ifindex = ifindex;
        sll.sll_halen = 0;
        let ll = unsafe {
            nix::sys::socket::LinkAddr::from_raw(
                &sll as *const libc::sockaddr_ll as *const libc::sockaddr,
                Some(std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t),
            )
        }
        .expect("LinkAddr length mismatch");
        bind(fd.as_raw_fd(), &ll).unwrap();

        let frame: Vec<u8> = [0xffu8; 6] // dst broadcast
            .into_iter()
            .chain([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]) // src
            .chain([0x08, 0x00]) // ethertype IPv4
            .chain(vec![0x5a; 64])
            .collect();

        // --- host -> mesh: transmit from the raw socket, expect TAP read.
        sendto(fd.as_raw_fd(), &frame, &ll, nix::sys::socket::MsgFlags::empty()).unwrap();
        let read_back = tokio::time::timeout(std::time::Duration::from_secs(5), outbound_rx.recv())
            .await
            .expect("timeout: TAP reader did not surface the transmitted frame")
            .expect("outbound channel closed");
        assert_eq!(read_back, frame, "host→mesh frame must match");

        // --- mesh -> host: inject via inbound channel, expect AF_PACKET copy.
        inbound_tx.send(frame.clone()).await.unwrap();
        let fd_raw = fd.as_raw_fd();
        let (n, _) = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; 2048];
            // Skip our own outgoing copy of the transmitted frame if queued
            // before the injected one arrives.
            for _ in 0..8 {
                let (n, _) = recvfrom::<nix::sys::socket::LinkAddr>(fd_raw, &mut buf).unwrap();
                if buf[..n.min(frame.len())] == frame[..n.min(frame.len())] && n > 0 {
                    return (n, buf);
                }
            }
            (0usize, buf)
        })
        .await
        .unwrap();
        assert!(n > 0, "mesh→host frame was not observed on the interface");
    }
}
