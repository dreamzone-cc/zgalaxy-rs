use std::process::Command;
use tracing::{info, warn};
use anyhow::Result;

/// Host Operating System Route, MTU, and IP Provisioning Manager
pub struct RouteManager;

impl RouteManager {
    /// Configure MTU on the virtual network interface.
    pub fn set_mtu(device_name: &str, mtu: u32) -> Result<()> {
        if mtu == 0 {
            return Ok(());
        }
        info!("[ZGALAXY ROUTE MANAGER] Setting MTU {} on interface {}", mtu, device_name);

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("ip")
                .args(["link", "set", "dev", device_name, "mtu", &mtu.to_string()])
                .status();
        }

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("netsh")
                .args(["interface", "ipv4", "set", "subinterface", device_name, &format!("mtu={}", mtu), "store=persistent"])
                .status();
            let _ = Command::new("netsh")
                .args(["interface", "ipv6", "set", "subinterface", device_name, &format!("mtu={}", mtu), "store=persistent"])
                .status();
        }

        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("ifconfig")
                .args([device_name, "mtu", &mtu.to_string()])
                .status();
        }

        Ok(())
    }

    /// Configure IP address (IPv4 or IPv6 with CIDR) on the virtual network interface.
    pub fn configure_interface_ip(device_name: &str, ip_with_cidr: &str) -> Result<()> {
        info!("[ZGALAXY ROUTE MANAGER] Assigning IP {} to interface {}", ip_with_cidr, device_name);
        let _is_v6 = ip_with_cidr.contains(':');

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
            if is_v6 {
                let _ = Command::new("netsh")
                    .args(["interface", "ipv6", "add", "address", device_name, ip])
                    .status();
            } else {
                let _ = Command::new("netsh")
                    .args(["interface", "ipv4", "set", "address", &format!("name=\"{}\"", device_name), "static", ip, "255.255.255.0"])
                    .status();
            }
        }

        #[cfg(target_os = "macos")]
        {
            let parts: Vec<&str> = ip_with_cidr.split('/').collect();
            let ip = parts[0];
            if is_v6 {
                let _ = Command::new("ifconfig")
                    .args([device_name, "inet6", ip_with_cidr, "add"])
                    .status();
            } else {
                let _ = Command::new("ifconfig")
                    .args([device_name, ip, ip, "netmask", "255.255.255.0", "up"])
                    .status();
            }
        }

        Ok(())
    }

    /// Add a managed network route (IPv4 or IPv6) through the virtual interface.
    pub fn add_route(device_name: &str, target_subnet: &str) -> Result<()> {
        info!("[ZGALAXY ROUTE MANAGER] Adding route {} via interface {}", target_subnet, device_name);
        let is_v6 = target_subnet.contains(':');

        #[cfg(target_os = "linux")]
        {
            if is_v6 {
                let _ = Command::new("ip")
                    .args(["-6", "route", "add", target_subnet, "dev", device_name])
                    .status();
            } else {
                let _ = Command::new("ip")
                    .args(["route", "add", target_subnet, "dev", device_name])
                    .status();
            }
        }

        #[cfg(target_os = "windows")]
        {
            if is_v6 {
                let _ = Command::new("netsh")
                    .args(["interface", "ipv6", "add", "route", target_subnet, device_name])
                    .status();
            } else {
                let _ = Command::new("route")
                    .args(["add", target_subnet, "0.0.0.0", "IF", device_name])
                    .status();
            }
        }

        #[cfg(target_os = "macos")]
        {
            if is_v6 {
                let _ = Command::new("route")
                    .args(["add", "-inet6", target_subnet, "-interface", device_name])
                    .status();
            } else {
                let _ = Command::new("route")
                    .args(["add", "-net", target_subnet, "-interface", device_name])
                    .status();
            }
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
