//! The mesh engine: everything that happens between a transport and the rest
//! of the daemon, written against [`MeshTransport`] so QUIC and GTP share it.
//!
//! - [`ControlEngine`] consumes [`TransportEvent`]s: identity binding (أ3),
//!   controller duties (config requests, membership tokens), client duties
//!   (config responses → interface), presence and RTT bookkeeping, and the
//!   inbound data plane (L2 learning → TAP).
//! - [`forward_outbound`] is the outbound data plane (TAP → L2 switch → mesh).
//! - [`rtt_prober`] and [`config_sync_loop`] are the periodic workers.
//!
//! This used to be one ~600-line closure in `main.rs` bound to QUIC types.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{mpsc, Notify};
use tracing::{debug, info, warn};

use crate::control::{challenge_material, unix_millis, ControlMessage};
use crate::controller::EmbeddedController;
use crate::identity::{Address, Identity};
use crate::interface_sync;
use crate::l2_switch::L2Switch;
use crate::mesh::{MeshTransport, PeerSessions, TransportEvent};
use crate::network::{NetworkManager, NetworkStatus};
use crate::peer::{format_path_address, parse_path_address, PeerManager, PeerRole};
use crate::resolver::DynamicDnsResolver;

/// Presence bookkeeping cadence for frame-driven refreshes (ب3).
pub const PEER_PRESENCE_INTERVAL: Duration = Duration::from_secs(5);
/// RTT probe cadence over the reliable control channel (ب3).
pub const RTT_PROBE_INTERVAL: Duration = Duration::from_secs(10);
/// Learned MACs silent this long are forgotten (ب2).
pub const MAC_MAX_AGE: Duration = Duration::from_secs(300);
/// Upper bound for one outbound control reply. Replies run off the event
/// loop so a peer that withholds stream credit cannot stall the engine.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one sync request (dial + proof + send) may take (ب4).
const SYNC_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a sync request waits for the identity handshake (أ3↔ب4).
const SYNC_ACCEPT_WAIT: Duration = Duration::from_millis(750);

/// Where controller-assigned addresses and routes are applied.
#[derive(Clone, Debug)]
pub struct InterfaceTarget {
    pub device: String,
    /// The most the active transport can carry (TransportCapabilities).
    pub mtu_cap: u32,
}

/// Shared handles every engine task needs.
#[derive(Clone)]
pub struct MeshContext {
    pub transport: Arc<dyn MeshTransport>,
    pub sessions: PeerSessions,
    pub identity: Identity,
    pub controller: EmbeddedController,
    /// Answer config requests (controller mode) — client nodes ignore them.
    pub is_controller: bool,
    pub networks: NetworkManager,
    pub peers: PeerManager,
    pub l2: L2Switch,
    /// Inbound frames → TAP writer.
    pub tun_tx: mpsc::Sender<Bytes>,
    /// None when headless (no TAP device): config is tracked, not applied.
    pub interface: Option<InterfaceTarget>,
    /// Wakes the config sync loop immediately (e.g. a fresh join).
    pub sync_now: Arc<Notify>,
}

impl MeshContext {
    /// Send a control message without blocking the caller.
    fn reply(&self, remote: SocketAddr, message: ControlMessage) {
        let transport = Arc::clone(&self.transport);
        tokio::spawn(async move {
            match tokio::time::timeout(REPLY_TIMEOUT, transport.send_control(remote, &message)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => debug!("[ZGALAXY MESH] {} to {} failed: {:#}", message.kind(), remote, e),
                Err(_) => debug!("[ZGALAXY MESH] {} to {} timed out", message.kind(), remote),
            }
        });
    }

    async fn touch_presence(&self, remote: SocketAddr, address: &str, latency_ms: i32) {
        if let Ok(node) = Address::from_str(address) {
            self.peers.add_or_update_peer(node, PeerRole::Leaf, remote, latency_ms).await;
        }
        self.controller
            .touch_member_last_seen(address, &format_path_address(remote))
            .await;
    }
}

/// Consumes transport events. One instance per daemon; owns per-remote
/// throttling state.
pub struct ControlEngine {
    ctx: MeshContext,
    last_presence: HashMap<SocketAddr, Instant>,
}

impl ControlEngine {
    pub fn new(ctx: MeshContext) -> Self {
        Self { ctx, last_presence: HashMap::new() }
    }

    pub async fn run(mut self, mut events: mpsc::Receiver<TransportEvent>) {
        while let Some(event) = events.recv().await {
            self.handle(event).await;
        }
        warn!("[ZGALAXY MESH] transport event channel closed — engine stopped");
    }

    pub async fn handle(&mut self, event: TransportEvent) {
        match event {
            TransportEvent::Connected { remote, generation } => {
                info!("[ZGALAXY MESH] session {} established with {}", generation, remote);
                // A new session must prove itself again, whatever the old one did.
                self.ctx.sessions.reset(remote).await;
                self.ctx.reply(
                    remote,
                    ControlMessage::NodeAnnounce {
                        address: self.ctx.identity.address.to_string(),
                        public_identity: self.ctx.identity.to_public_string(),
                    },
                );
            }
            TransportEvent::Disconnected { remote, generation } => {
                let replaced = self
                    .ctx
                    .transport
                    .active_generation(remote)
                    .await
                    .is_some_and(|active| active != generation);
                info!("[ZGALAXY MESH] session {} with {} lost (replaced: {})", generation, remote, replaced);
                if replaced {
                    return;
                }
                self.ctx.sessions.reset(remote).await;
                self.last_presence.remove(&remote);
                // ب4: frames to the lost peer's MACs fall back to flooding
                // until it reconnects and re-teaches itself.
                let removed = self.ctx.l2.remove_endpoint(remote).await;
                if removed > 0 {
                    debug!("[ZGALAXY MESH] evicted {} learned MAC(s) for {}", removed, remote);
                }
            }
            TransportEvent::Frame { remote, data } => self.on_frame(remote, data).await,
            TransportEvent::Control { remote, message } => self.on_control(remote, message).await,
        }
    }

    async fn on_frame(&mut self, remote: SocketAddr, data: Bytes) {
        // Only sessions that proved a node identity may reach the virtual
        // LAN; an anonymous endpoint must not inject frames.
        let Some(address) = self.ctx.sessions.proven_address(remote).await else {
            debug!("[ZGALAXY MESH] dropped {}B frame from unproven {}", data.len(), remote);
            return;
        };
        // ب2 backward learning: src MAC ↔ delivering endpoint.
        if let Some((_, src_mac)) = L2Switch::parse_ethernet(&data) {
            self.ctx.l2.learn(src_mac, remote).await;
        }
        // Frames arrive at game tick rates; presence runs at most once per
        // PEER_PRESENCE_INTERVAL per remote.
        let due = self
            .last_presence
            .get(&remote)
            .is_none_or(|t| t.elapsed() >= PEER_PRESENCE_INTERVAL);
        if due {
            self.last_presence.insert(remote, Instant::now());
            self.ctx.touch_presence(remote, &address, 0).await;
        }
        let _ = self.ctx.tun_tx.send(data).await;
    }

    async fn on_control(&mut self, remote: SocketAddr, message: ControlMessage) {
        match message {
            ControlMessage::NodeAnnounce { address, .. } => {
                // أ3: never trust an unproven announcement — challenge it.
                info!("[ZGALAXY MESH] {} announced as {} — challenging", remote, address);
                if let Some(nonce) = self.ctx.sessions.challenge(remote, &address).await {
                    self.ctx.reply(remote, ControlMessage::NodeChallenge { nonce });
                }
            }
            ControlMessage::NodeChallenge { nonce } => {
                // We are the announcer: prove possession of our secret key.
                let address = self.ctx.identity.address.to_string();
                match self.ctx.identity.sign(&challenge_material(nonce, &address)) {
                    Ok(sig) => self.ctx.reply(
                        remote,
                        ControlMessage::AnnounceProof {
                            nonce,
                            address,
                            signature: hex::encode(sig),
                            public_identity: self.ctx.identity.to_public_string(),
                        },
                    ),
                    Err(e) => warn!("[ZGALAXY MESH] cannot sign announce proof: {}", e),
                }
            }
            ControlMessage::AnnounceProof { nonce, address, signature, public_identity } => {
                if self
                    .ctx
                    .sessions
                    .verify_proof(remote, nonce, &address, &signature, &public_identity)
                    .await
                {
                    info!("[ZGALAXY MESH] {} proven as {}", remote, address);
                    self.ctx.reply(remote, ControlMessage::AnnounceAccepted { address: address.clone() });
                    self.last_presence.insert(remote, Instant::now());
                    self.ctx.touch_presence(remote, &address, 0).await;
                } else {
                    warn!("[ZGALAXY MESH] announce proof from {} rejected", remote);
                }
            }
            ControlMessage::AnnounceAccepted { address } => {
                // The peer verified OUR proof — config requests may flow.
                self.ctx.sessions.mark_accepted_us(remote, &address).await;
                debug!("[ZGALAXY MESH] our announce accepted by {}", remote);
            }
            ControlMessage::NetworkConfigRequest { nwid, token } => {
                self.on_config_request(remote, nwid, token).await;
            }
            ControlMessage::NetworkConfigResponse { nwid, config } => {
                self.on_config_response(remote, nwid, config).await;
            }
            ControlMessage::Ping { nonce, sent_ms } => {
                self.ctx.reply(remote, ControlMessage::Pong { nonce, sent_ms, received_ms: unix_millis() });
            }
            ControlMessage::Pong { sent_ms, .. } => {
                // RTT is measured at the sender against its own clock.
                let rtt = i32::try_from(unix_millis().saturating_sub(sent_ms)).unwrap_or(i32::MAX);
                if let Some(address) = self.ctx.sessions.proven_address(remote).await {
                    if let Ok(node) = Address::from_str(&address) {
                        self.ctx.peers.add_or_update_peer(node, PeerRole::Leaf, remote, rtt).await;
                        debug!("[ZGALAXY MESH RTT] {} -> {}ms", address, rtt);
                    }
                }
            }
        }
    }

    /// Controller side: answer a member's config request.
    async fn on_config_request(&mut self, remote: SocketAddr, nwid: String, token: Option<String>) {
        if !self.ctx.is_controller {
            debug!("[ZGALAXY MESH] config request from {} for {} ignored: client mode", remote, nwid);
            return;
        }
        // The member id is the PROVEN node address — anything else cannot map
        // to a member record, and unproven peers are refused (أ3).
        let Some(member) = self.ctx.sessions.proven_address(remote).await else {
            warn!("[ZGALAXY MESH] config request from {} before proven announce — ignored", remote);
            return;
        };
        let controller = &self.ctx.controller;
        if let Err(e) = controller.register_join_request(&nwid, &member, None).await {
            debug!("[ZGALAXY MESH] join request {} from {} rejected: {}", nwid, member, e);
            return;
        }
        let Some(net) = controller.get_network(&nwid).await else { return };
        let member_rec = controller.get_effective_member(&nwid, &member).await;
        let authorized = member_rec.as_ref().map(|m| m.authorized).unwrap_or(false);
        let ips = member_rec.map(|m| m.ip_assignments).unwrap_or_default();

        // أ1: the member record is authoritative. A presented token that no
        // longer verifies (expired after the member was offline, issued
        // before a revision bump, or forged) is logged and replaced — never
        // a reason to withhold the config: refusing would lock an authorized
        // member out for good, since only a response carries a fresh token.
        if let Some(presented) = token.as_deref() {
            if controller.verify_membership_token(presented, &nwid, &member).is_none() {
                info!(
                    "[ZGALAXY MESH] stale or invalid membership token from {} for {} — reissuing",
                    member, nwid
                );
            }
        }
        let issued = if authorized { controller.issue_membership_token(&nwid, &member).await } else { None };

        let response = serde_json::json!({
            "nwid": nwid,
            "name": net.name,
            "authorized": authorized,
            "ipAssignments": ips,
            "routes": net.routes,
            "mtu": net.mtu,
            "membershipToken": issued,
        });
        self.ctx.reply(remote, ControlMessage::NetworkConfigResponse { nwid, config: response });
    }

    /// Client side: apply a controller's config to the joined network.
    async fn on_config_response(&mut self, remote: SocketAddr, nwid: String, config: serde_json::Value) {
        // Only the network's controller may configure it: a network id is the
        // controller's 10-hex address followed by 6 hex digits. Without this
        // any proven peer could push addresses and routes for any network.
        let controller_address = nwid.get(..10).map(str::to_ascii_lowercase);
        let proven = self.ctx.sessions.proven_address(remote).await;
        if controller_address.is_none() || proven.as_deref() != controller_address.as_deref() {
            warn!(
                "[ZGALAXY MESH] config for {} from {} (proven as {:?}) rejected: not the network's controller",
                nwid, remote, proven
            );
            return;
        }
        let Some(applied) = self.ctx.networks.apply_controller_config(&nwid, &config).await else {
            return;
        };
        if applied.changed {
            info!(
                "[ZGALAXY MESH] network {} updated (name: {}, authorized: {}, ips: {:?})",
                nwid, applied.current.name, applied.authorized, applied.current.assigned_addresses
            );
        }
        if let Some(iface) = &self.ctx.interface {
            let plan = interface_sync::plan_update(
                &applied.previous,
                &applied.current,
                applied.was_authorized,
                applied.authorized,
                iface.mtu_cap,
            );
            interface_sync::apply(&iface.device, plan).await;
        }
    }
}

/// Outbound data plane (ب2): unicast frames go only to the learned owner of
/// the destination MAC; broadcast, multicast and unknown destinations flood
/// to every connected peer.
pub async fn forward_outbound(ctx: MeshContext, mut frames: mpsc::Receiver<Bytes>) {
    while let Some(frame) = frames.recv().await {
        let unicast_dst = L2Switch::parse_ethernet(&frame)
            .map(|(dst, _)| dst)
            .filter(|dst| !L2Switch::is_broadcast(dst) && !L2Switch::is_multicast(dst));
        let owner = match unicast_dst {
            Some(dst) => ctx.l2.resolve(&dst).await,
            None => None,
        };
        match owner {
            Some(endpoint) => {
                if let Err(e) = ctx.transport.send_frame(endpoint, frame).await {
                    debug!("[ZGALAXY MESH] unicast frame to {} dropped: {:#}", endpoint, e);
                }
            }
            None => {
                // Snapshot first: never hold a transport lock across sends.
                for remote in ctx.transport.connected_peers().await {
                    if let Err(e) = ctx.transport.send_frame(remote, frame.clone()).await {
                        debug!("[ZGALAXY MESH] flooded frame to {} dropped: {:#}", remote, e);
                    }
                }
            }
        }
    }
}

/// ب3: periodic RTT probes on the reliable channel; Pong handling feeds real
/// latency into PeerManager (ZTNET `/peer`). Also ages out silent MACs.
pub async fn rtt_prober(ctx: MeshContext) {
    let mut interval = tokio::time::interval(RTT_PROBE_INTERVAL);
    loop {
        interval.tick().await;
        let now = unix_millis();
        for remote in ctx.transport.connected_peers().await {
            ctx.reply(remote, ControlMessage::Ping { nonce: rand::random::<u64>(), sent_ms: now });
        }
        let _ = ctx.l2.evict_stale(MAC_MAX_AGE).await;
    }
}

/// Endpoints from `ZGALAXY_EXTRA_ENDPOINTS` (comma-separated `host:port`).
/// Names are resolved through DNS (docker service names etc.).
pub async fn extra_endpoints() -> Vec<SocketAddr> {
    let mut out = Vec::new();
    let Ok(extra) = std::env::var("ZGALAXY_EXTRA_ENDPOINTS") else { return out };
    for ep in extra.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let resolved = match ep.parse::<SocketAddr>() {
            Ok(addr) => Some(addr),
            Err(_) => match tokio::net::lookup_host(ep).await {
                Ok(mut addrs) => addrs.next(),
                Err(e) => {
                    warn!("[ZGALAXY MESH] cannot resolve extra endpoint '{}': {}", ep, e);
                    None
                }
            },
        };
        if let Some(addr) = resolved {
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
    }
    out
}

/// Periodic network-config sync over the reliable channel (ب4).
///
/// Targets, in order: operator-pinned endpoints, known paths of each joined
/// network's controller, then resolver roots. Each target gets its own
/// detached job (at most one in flight per target), so a dead target costs
/// only its own timeout and never slows the cadence for the others.
/// Cadence: 500ms while any connected network is not OK, else 3s; a join
/// wakes the loop immediately through `ctx.sync_now`.
pub async fn config_sync_loop(ctx: MeshContext, resolver: Arc<DynamicDnsResolver>) {
    let in_flight: Arc<std::sync::Mutex<std::collections::HashSet<SocketAddr>>> = Arc::default();
    loop {
        let nets: Vec<_> = ctx.networks.list().await.into_iter().filter(|n| n.connected).collect();
        if nets.is_empty() {
            debug!("[ZGALAXY MESH SYNC] heartbeat: no connected networks");
            sleep_or_wake(&ctx.sync_now, Duration::from_secs(3)).await;
            continue;
        }

        let mut targets = extra_endpoints().await;
        for net in &nets {
            let Some(ctrl) = net.nwid.get(..10).and_then(|s| Address::from_str(s).ok()) else { continue };
            if let Some(peer) = ctx.peers.get_peer(&ctrl).await {
                for path in peer.paths.iter().filter(|p| p.active) {
                    if let Some(addr) = parse_path_address(&path.address) {
                        if !targets.contains(&addr) {
                            targets.push(addr);
                        }
                    }
                }
            }
        }
        for addr in resolver.get_all_active_addresses().await {
            if !targets.contains(&addr) {
                targets.push(addr);
            }
        }
        debug!("[ZGALAXY MESH SYNC] {} network(s), {} target(s): {:?}", nets.len(), targets.len(), targets);

        for target in targets {
            if !in_flight.lock().map(|mut set| set.insert(target)).unwrap_or(false) {
                continue; // previous request to this target still running
            }
            let requests: Vec<ControlMessage> = nets
                .iter()
                .map(|n| ControlMessage::NetworkConfigRequest { nwid: n.nwid.clone(), token: n.membership_token.clone() })
                .collect();
            let ctx = ctx.clone();
            let in_flight = Arc::clone(&in_flight);
            tokio::spawn(async move {
                let attempt = async {
                    ctx.transport.connect(target).await?;
                    // أ3 sequencing: let the identity handshake finish
                    // both ways, so neither the request (unproven us) nor
                    // the response (unproven controller) is dropped.
                    ctx.sessions.await_mutual(target, SYNC_ACCEPT_WAIT).await;
                    for msg in &requests {
                        ctx.transport.send_control(target, msg).await?;
                    }
                    anyhow::Ok(())
                };
                match tokio::time::timeout(SYNC_REQUEST_TIMEOUT, attempt).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => debug!("[ZGALAXY MESH SYNC] request to {} failed: {:#}", target, e),
                    Err(_) => debug!("[ZGALAXY MESH SYNC] request to {} timed out", target),
                }
                if let Ok(mut set) = in_flight.lock() {
                    set.remove(&target);
                }
            });
        }

        let any_pending = ctx
            .networks
            .list()
            .await
            .iter()
            .any(|n| n.connected && n.status != NetworkStatus::Ok);
        let cadence = if any_pending { Duration::from_millis(500) } else { Duration::from_secs(3) };
        sleep_or_wake(&ctx.sync_now, cadence).await;
    }
}

async fn sleep_or_wake(wake: &Notify, dur: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(dur) => {}
        _ = wake.notified() => {}
    }
}
