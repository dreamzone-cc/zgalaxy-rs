//! Reconcile the virtual interface with a network's controller-assigned state.
//!
//! Planning is pure (and unit-tested): given the previous and the new view of
//! a network, compute exactly which addresses, routes and MTU change.
//! Applying the plan shells out to `ip` (via `RouteManager`), which blocks —
//! so it runs on the blocking pool, never on an async worker thread.

use crate::network::{ManagedRoute, Network};
use crate::route_manager::RouteManager;

/// Host-side changes needed to bring the interface in line with a network.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InterfacePlan {
    pub mtu: Option<u32>,
    /// MAC to set before assigning addresses.
    pub mac: Option<String>,
    /// CIDRs to remove — the SAME prefix they were added with.
    pub remove_addresses: Vec<String>,
    pub add_addresses: Vec<String>,
    pub remove_routes: Vec<String>,
    pub add_routes: Vec<String>,
}

impl InterfacePlan {
    pub fn is_empty(&self) -> bool {
        self.mtu.is_none()
            && self.remove_addresses.is_empty()
            && self.add_addresses.is_empty()
            && self.remove_routes.is_empty()
            && self.add_routes.is_empty()
    }
}

/// Address in CIDR form. A bare address takes the prefix of the first
/// same-family on-link managed route (ZeroTier semantics), else /24 or /128.
pub fn cidr_for_address(ip: &str, routes: &[ManagedRoute]) -> String {
    if ip.contains('/') {
        return ip.to_string();
    }
    let is_v6 = ip.contains(':');
    let prefix_len = routes
        .iter()
        .find(|r| r.via.is_none() && r.target.contains(':') == is_v6)
        .and_then(|r| r.target.split('/').nth(1))
        .unwrap_or(if is_v6 { "128" } else { "24" });
    format!("{}/{}", ip, prefix_len)
}

fn is_default_route(target: &str) -> bool {
    target == "0.0.0.0/0" || target == "::/0"
}

fn routes_allowed<'a>(net: &'a Network) -> impl Iterator<Item = &'a ManagedRoute> + 'a {
    net.routes
        .iter()
        .filter(move |r| net.allow_default || !is_default_route(&r.target))
}

/// Everything a connected, managed network needs at boot (cached config).
pub fn plan_boot(net: &Network) -> InterfacePlan {
    if !net.connected || !net.allow_managed {
        return InterfacePlan::default();
    }
    InterfacePlan {
        mtu: None,
        mac: Some(net.mac.clone()),
        remove_addresses: Vec::new(),
        add_addresses: net.assigned_addresses.iter().map(|ip| cidr_for_address(ip, &net.routes)).collect(),
        remove_routes: Vec::new(),
        add_routes: routes_allowed(net).map(|r| r.target.clone()).collect(),
    }
}

/// Changes after a controller config moved a network from `prev` to `new`.
/// `mtu_cap` is the most the active transport can carry.
pub fn plan_update(prev: &Network, new: &Network, was_authorized: bool, authorized: bool, mtu_cap: u32) -> InterfacePlan {
    let mut plan = InterfacePlan::default();
    if authorized && new.allow_managed {
        let mtu = new.mtu.min(mtu_cap);
        if new.mtu > 0 && new.mtu != prev.mtu {
            plan.mtu = Some(mtu);
        }
        let prev_cidrs: Vec<String> = prev.assigned_addresses.iter().map(|ip| cidr_for_address(ip, &prev.routes)).collect();
        let new_cidrs: Vec<String> = new.assigned_addresses.iter().map(|ip| cidr_for_address(ip, &new.routes)).collect();
        plan.remove_addresses = prev_cidrs.iter().filter(|c| !new_cidrs.contains(c)).cloned().collect();
        plan.add_addresses = new_cidrs.iter().filter(|c| !was_authorized || !prev_cidrs.contains(c)).cloned().collect();
        if !plan.add_addresses.is_empty() {
            plan.mac = Some(new.mac.clone());
        }

        let prev_targets: Vec<&str> = routes_allowed(prev).map(|r| r.target.as_str()).collect();
        let new_targets: Vec<&str> = routes_allowed(new).map(|r| r.target.as_str()).collect();
        plan.remove_routes = prev
            .routes
            .iter()
            .map(|r| r.target.as_str())
            .filter(|t| !new_targets.contains(t))
            .map(String::from)
            .collect();
        plan.add_routes = new_targets
            .iter()
            .filter(|t| !was_authorized || !prev_targets.contains(t))
            .map(|t| t.to_string())
            .collect();
    } else if was_authorized && !authorized {
        // De-authorized by the controller: flush addresses and routes now.
        plan.remove_addresses = prev.assigned_addresses.iter().map(|ip| cidr_for_address(ip, &prev.routes)).collect();
        plan.remove_routes = prev.routes.iter().map(|r| r.target.clone()).collect();
    }
    plan
}

/// Everything to remove when a network is disconnected or left.
pub fn plan_teardown(net: &Network) -> InterfacePlan {
    InterfacePlan {
        remove_addresses: net.assigned_addresses.iter().map(|ip| cidr_for_address(ip, &net.routes)).collect(),
        remove_routes: net.routes.iter().map(|r| r.target.clone()).collect(),
        ..Default::default()
    }
}

/// Apply `plan` to `device` on the blocking pool. Failures are logged by
/// `RouteManager` and never fatal (headless nodes have no device at all).
pub async fn apply(device: &str, plan: InterfacePlan) {
    if plan.is_empty() {
        return;
    }
    let device = device.to_string();
    let _ = tokio::task::spawn_blocking(move || apply_blocking(&device, &plan)).await;
}

fn apply_blocking(device: &str, plan: &InterfacePlan) {
    if let Some(mtu) = plan.mtu {
        let _ = RouteManager::set_mtu(device, mtu);
    }
    for cidr in &plan.remove_addresses {
        let _ = RouteManager::remove_interface_ip(device, cidr);
    }
    for route in &plan.remove_routes {
        let _ = RouteManager::remove_route(device, route);
    }
    if !plan.add_addresses.is_empty() {
        if let Some(mac) = &plan.mac {
            RouteManager::set_mac(device, mac);
        }
    }
    for cidr in &plan.add_addresses {
        if let Err(e) = RouteManager::configure_interface_ip(device, cidr) {
            tracing::warn!("[ZGALAXY VIRTUAL INTERFACE] could not assign {}: {}", cidr, e);
        }
    }
    for route in &plan.add_routes {
        let _ = RouteManager::add_route(device, route);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::NetworkManager;

    fn route(target: &str) -> ManagedRoute {
        ManagedRoute { target: target.into(), via: None }
    }

    async fn network() -> Network {
        let nm = NetworkManager::new();
        nm.set_node_address("a1b2c3d4e5".into()).await;
        nm.join("0123456789abcdef").await.unwrap()
    }

    #[test]
    fn cidr_uses_same_family_route_prefix() {
        let routes = vec![route("10.10.0.0/16"), route("fd00::/64")];
        assert_eq!(cidr_for_address("10.10.3.4", &routes), "10.10.3.4/16");
        assert_eq!(cidr_for_address("fd00::7", &routes), "fd00::7/64");
        assert_eq!(cidr_for_address("10.10.3.4/30", &routes), "10.10.3.4/30");
        assert_eq!(cidr_for_address("192.168.1.2", &[]), "192.168.1.2/24");
        assert_eq!(cidr_for_address("fd01::1", &[route("10.0.0.0/8")]), "fd01::1/128");
    }

    #[tokio::test]
    async fn first_authorization_adds_everything() {
        let prev = network().await;
        let mut new = prev.clone();
        new.assigned_addresses = vec!["10.10.0.5".into()];
        new.routes = vec![route("10.10.0.0/16"), route("0.0.0.0/0")];
        let plan = plan_update(&prev, &new, false, true, 2800);
        assert_eq!(plan.add_addresses, vec!["10.10.0.5/16"]);
        assert_eq!(plan.add_routes, vec!["10.10.0.0/16"], "default route needs allowDefault");
        assert_eq!(plan.mac.as_deref(), Some(new.mac.as_str()));
    }

    #[tokio::test]
    async fn steady_state_is_a_no_op_and_changes_are_minimal() {
        let mut prev = network().await;
        prev.assigned_addresses = vec!["10.10.0.5".into()];
        prev.routes = vec![route("10.10.0.0/16")];
        assert!(plan_update(&prev, &prev, true, true, 2800).is_empty());

        let mut new = prev.clone();
        new.assigned_addresses = vec!["10.10.0.9".into()];
        let plan = plan_update(&prev, &new, true, true, 2800);
        // Removal uses the prefix the address was added with (/16, not /24).
        assert_eq!(plan.remove_addresses, vec!["10.10.0.5/16"]);
        assert_eq!(plan.add_addresses, vec!["10.10.0.9/16"]);
        assert!(plan.add_routes.is_empty() && plan.remove_routes.is_empty());
    }

    #[tokio::test]
    async fn mtu_is_capped_by_transport() {
        let prev = network().await;
        let mut new = prev.clone();
        new.mtu = 9000;
        assert_eq!(plan_update(&prev, &new, true, true, 2800).mtu, Some(2800));
    }

    #[tokio::test]
    async fn deauthorization_flushes() {
        let mut prev = network().await;
        prev.assigned_addresses = vec!["10.10.0.5".into()];
        prev.routes = vec![route("10.10.0.0/16")];
        let plan = plan_update(&prev, &prev, true, false, 2800);
        assert_eq!(plan.remove_addresses, vec!["10.10.0.5/16"]);
        assert_eq!(plan.remove_routes, vec!["10.10.0.0/16"]);
        assert!(plan.add_addresses.is_empty());
    }

    #[tokio::test]
    async fn boot_plan_respects_intent_and_flags() {
        let mut net = network().await;
        net.assigned_addresses = vec!["10.10.0.5".into()];
        net.routes = vec![route("10.10.0.0/16"), route("::/0")];
        let plan = plan_boot(&net);
        assert_eq!(plan.add_addresses, vec!["10.10.0.5/16"]);
        assert_eq!(plan.add_routes, vec!["10.10.0.0/16"]);

        net.connected = false;
        assert!(plan_boot(&net).is_empty());
        net.connected = true;
        net.allow_managed = false;
        assert!(plan_boot(&net).is_empty());
    }
}
