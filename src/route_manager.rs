use std::process::Command;
use tracing::{info, warn};
use anyhow::Result;

/// Host Operating System Route and IP Provisioning Manager
pub struct RouteManager;

impl RouteManager {
    /// Configure IP address and subnet mask on the virtual network interface.
    pub fn configure_interface_ip(device_name: &str, ip_with_cidr: &str) -> Result<()> {
        info!("[ZGALAXY ROUTE MANAGER] Assigning IP {} to interface {}", ip_with_cidr, device_name);

        #[cfg(target_os = "linux")]
        {
            let status = Command::new("ip")
                .args(["addr", "add", ip_with_cidr, "dev", device_name])
                .status();
            if let Ok(s) = status {
                if s.success() {
                    let _ = Command::new("ip").args(["link", "set", "dev", device_name, "up"]).status();
                    return Ok(());
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let parts: Vec<&str> = ip_with_cidr.split('/').collect();
            let ip = parts[0];
            let _ = Command::new("netsh")
                .args(["interface", "ipv4", "set", "address", &format!("name=\"{}\"", device_name), "static", ip, "255.255.255.0"])
                .status();
        }

        #[cfg(target_os = "macos")]
        {
            let parts: Vec<&str> = ip_with_cidr.split('/').collect();
            let ip = parts[0];
            let _ = Command::new("ifconfig")
                .args([device_name, ip, ip, "netmask", "255.255.255.0", "up"])
                .status();
        }

        Ok(())
    }

    /// Add a managed network route through the virtual interface.
    pub fn add_route(device_name: &str, target_subnet: &str) -> Result<()> {
        info!("[ZGALAXY ROUTE MANAGER] Adding route {} via interface {}", target_subnet, device_name);

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("ip")
                .args(["route", "add", target_subnet, "dev", device_name])
                .status();
        }

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("route")
                .args(["add", target_subnet, "0.0.0.0", "IF", device_name])
                .status();
        }

        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("route")
                .args(["add", "-net", target_subnet, "-interface", device_name])
                .status();
        }

        Ok(())
    }
}

impl RouteManager {
    /// Set the hardware (MAC) address of the virtual adapter.
    /// Idempotent-safe: `ip link set` overwrites the previous value.
    pub fn set_mac(device_name: &str, mac: &str) {
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("ip")
                .args(["link", "set", "dev", device_name, "address", mac])
                .status()
                .map(|s| {
                    if s.success() {
                        info!("[ZGALAXY ROUTE MANAGER] MAC {} set on {}", mac, device_name);
                    } else {
                        warn!("[ZGALAXY ROUTE MANAGER] failed to set MAC {} on {}", mac, device_name);
                    }
                });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (device_name, mac);
        }
    }
}
