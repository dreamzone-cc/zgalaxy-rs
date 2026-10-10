//! GTP Transport Layer (GTP-rs backend, feature `gtp`).
//!
//! Same contract as [`crate::quic`] (docs/GTP-INTEGRATION.md §3), on GTP/1.1:
//!
//! | zgalaxy need                 | GTP-rs                                              |
//! |------------------------------|-----------------------------------------------------|
//! | Channel encryption           | X25519 handshake + AEAD (`connect(.., secure=true)`) |
//! | Node identity (أ3)           | unchanged — challenge/proof over the control channel |
//! | L2 frame ≤ Unreliable limit  | `send_unreliable` (no retransmit, like a NIC)        |
//! | L2 frame above the limit     | `send_reliable_unordered` (fragmented, no HOL)       |
//! | Control message              | `send_reliable_ordered` on group [`CONTROL_GROUP`]   |
//! | Dead-peer detection          | GTP idle timeout (15 s, `competitive_fps`)           |
//!
//! Facts from the phase-7 smoke test (`tests/gtp_api_smoke.rs`) that shape
//! this module:
//! - `OrderedGroupId(0)` is GTP-rs's UNORDERED group, so control uses group 1.
//! - `recv` needs `&mut self` while sends need `&self`: every session is
//!   owned by ONE task that multiplexes inbound messages and a send queue.
//! - Dropping a connection handle does not close the session (GTP-rs tasks
//!   keep answering keepalives), so [`GtpTransport::close`] sends an explicit
//!   CLOSE and waits for it to be queued.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use gtp::{AsyncGtpConnection, ConnectionId, GtpEndpoint, MessageClass, OrderedGroupId, PriorityTier, TransportError};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::control::{self, ControlMessage};
use crate::mesh::{MeshTransport, SessionGeneration, TransportCapabilities, TransportEvent, TransportKind};
use crate::quic::canonical_endpoint;

/// Ordered group carrying control messages. Group 0 is unordered in GTP-rs.
pub const CONTROL_GROUP: OrderedGroupId = OrderedGroupId(1);

/// Unreliable payload ceiling measured with `GtpConfig::competitive_fps()`
/// (min MTU 1200). Advertised in the capabilities only: sends learn the live
/// limit from `PayloadTooLarge` and fall back to ReliableUnordered.
pub const UNRELIABLE_PAYLOAD: usize = 1133;

/// Largest frame accepted (ReliableUnordered fragments up to this).
pub const MAX_FRAME_SIZE: usize = crate::quic::MAX_FRAME_SIZE;

/// A dial to a dead endpoint gives up after this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Frames queued per session before new ones are dropped (NIC semantics).
const SEND_QUEUE: usize = 1024;

/// How long `close` waits for the CLOSE frame to be queued.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// `graceful_close` only QUEUES the CLOSE; GTP-rs's endpoint task puts it on
/// the wire. At shutdown the process must outlive that, or peers only notice
/// after their idle timeout (seen live: 15 s instead of < 1 ms).
const CLOSE_FLUSH_GRACE: Duration = Duration::from_millis(200);

enum Command {
    Frame(Bytes),
    Control(Vec<u8>, oneshot::Sender<Result<()>>),
    Close(oneshot::Sender<()>),
}

#[derive(Clone)]
struct Session {
    generation: SessionGeneration,
    commands: mpsc::Sender<Command>,
}

pub struct GtpTransport {
    endpoint: GtpEndpoint,
    peers: RwLock<HashMap<SocketAddr, Session>>,
    events: RwLock<Option<mpsc::Sender<TransportEvent>>>,
    /// One dial lock per remote (see `QuicTransport::dial_locks`).
    dial_locks: Mutex<HashMap<SocketAddr, Arc<Mutex<()>>>>,
    next_generation: AtomicU64,
}

impl GtpTransport {
    pub async fn bind(bind_addr: SocketAddr) -> Result<Self> {
        let endpoint = GtpEndpoint::bind(bind_addr)
            .await
            .map_err(|e| anyhow!("failed to bind GTP endpoint on {}: {:?}", bind_addr, e))?;
        let this = Self {
            endpoint,
            peers: RwLock::new(HashMap::new()),
            events: RwLock::new(None),
            dial_locks: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
        };
        info!("[ZGALAXY GTP] Endpoint bound on {}", this.local_addr()?);
        Ok(this)
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint
            .local_addr()
            .map_err(|e| anyhow!("GTP local address unavailable: {:?}", e))
    }

    /// Accept loop: every inbound session becomes the active one for its remote.
    pub async fn run(self: Arc<Self>, events_tx: mpsc::Sender<TransportEvent>) -> Result<()> {
        *self.events.write().await = Some(events_tx.clone());
        loop {
            let conn = self.endpoint.accept().await.context("GTP endpoint closed")?;
            let remote = canonical_endpoint(conn.peer_addr().await);
            info!("[ZGALAXY GTP] Peer connected: {}", remote);
            self.activate(remote, conn, events_tx.clone()).await;
        }
    }

    /// Make `conn` the active session for `remote` and start its task.
    /// A new session always wins (ب4); the replaced one is closed. Connected
    /// is emitted before the task starts reading, so it precedes all traffic.
    async fn activate(self: &Arc<Self>, remote: SocketAddr, conn: AsyncGtpConnection, events_tx: mpsc::Sender<TransportEvent>) {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (commands, command_rx) = mpsc::channel(SEND_QUEUE);
        let replaced = self.peers.write().await.insert(remote, Session { generation, commands });
        if let Some(old) = replaced {
            warn!("[ZGALAXY GTP] replacing session generation {} for {}", old.generation, remote);
            // Not try_send: a full frame queue must not keep the old session alive.
            tokio::spawn(async move {
                let (ack, _) = oneshot::channel();
                let _ = old.commands.send(Command::Close(ack)).await;
            });
        }

        let _ = events_tx.send(TransportEvent::Connected { remote, generation }).await;

        let this = Arc::clone(self);
        tokio::spawn(async move {
            drive_session(conn, remote, command_rx, &events_tx).await;
            {
                let mut peers = this.peers.write().await;
                if peers.get(&remote).map(|s| s.generation == generation).unwrap_or(false) {
                    peers.remove(&remote);
                }
            }
            info!("[ZGALAXY GTP] Session with {} ended (generation {})", remote, generation);
            let _ = events_tx.send(TransportEvent::Disconnected { remote, generation }).await;
        });
    }

    async fn active(&self, remote: SocketAddr) -> Option<Session> {
        self.peers.read().await.get(&canonical_endpoint(remote)).cloned()
    }

    async fn dial_lock(&self, remote: SocketAddr) -> Arc<Mutex<()>> {
        let mut locks = self.dial_locks.lock().await;
        locks.retain(|_, l| Arc::strong_count(l) > 1);
        Arc::clone(locks.entry(remote).or_default())
    }

    /// Ensure a session to `remote` (idempotent). Refuses to run before
    /// `run()` installed the event sink so inbound traffic is never lost.
    pub async fn connect(self: &Arc<Self>, remote: SocketAddr) -> Result<()> {
        let remote = canonical_endpoint(remote);
        if self.active(remote).await.is_some() {
            return Ok(());
        }
        let Some(events) = self.events.read().await.clone() else {
            bail!("GTP transport not started (run() must be called before connect)");
        };

        let lock = self.dial_lock(remote).await;
        let _guard = lock.lock().await;
        if self.active(remote).await.is_some() {
            return Ok(());
        }

        // The CID only routes packets; identity comes from the أ3 proof.
        let cid = ConnectionId(rand::random::<u64>());
        let conn = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.endpoint.connect(cid, remote, true))
            .await
            .map_err(|_| anyhow!("GTP handshake with {} timed out", remote))?
            .map_err(|e| anyhow!("GTP handshake with {} failed: {:?}", remote, e))?;
        info!("[ZGALAXY GTP] Connected to peer {}", remote);
        self.activate(remote, conn, events).await;
        Ok(())
    }

    /// Queue a data-plane frame on an EXISTING session. Never dials, never
    /// waits: a full queue drops the frame like a congested NIC would.
    pub async fn send_frame(&self, remote: SocketAddr, frame: Bytes) -> Result<()> {
        if frame.len() > MAX_FRAME_SIZE {
            bail!("frame of {} bytes exceeds the {} byte mesh frame limit", frame.len(), MAX_FRAME_SIZE);
        }
        let Some(session) = self.active(remote).await else {
            bail!("no GTP session to {}", remote);
        };
        session.commands.try_send(Command::Frame(frame)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => anyhow!("GTP send queue to {} is full — frame dropped", remote),
            mpsc::error::TrySendError::Closed(_) => anyhow!("GTP session to {} is closing", remote),
        })
    }

    /// Send a control message reliably and in order (dials if needed).
    pub async fn send_control(self: &Arc<Self>, remote: SocketAddr, msg: &ControlMessage) -> Result<()> {
        self.connect(remote).await?;
        let session = self.active(remote).await.with_context(|| format!("GTP session to {} closed", remote))?;
        let (done, result) = oneshot::channel();
        session
            .commands
            .send(Command::Control(control::encode(msg)?, done))
            .await
            .map_err(|_| anyhow!("GTP session to {} is closing", remote))?;
        result.await.map_err(|_| anyhow!("GTP session to {} ended before the send", remote))?
    }

    /// Close the session to `remote` with an explicit CLOSE frame.
    pub async fn close(&self, remote: SocketAddr) {
        let Some(session) = self.peers.write().await.remove(&canonical_endpoint(remote)) else {
            return;
        };
        let (ack, closed) = oneshot::channel();
        if session.commands.send(Command::Close(ack)).await.is_ok() {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, closed).await;
        }
    }

    /// Close every session. Needed at shutdown: unlike QUIC, an abandoned
    /// GTP session is not torn down by dropping it.
    pub async fn close_all(&self) {
        let remotes: Vec<SocketAddr> = self.peers.read().await.keys().copied().collect();
        if remotes.is_empty() {
            return;
        }
        futures::future::join_all(remotes.into_iter().map(|r| self.close(r))).await;
        tokio::time::sleep(CLOSE_FLUSH_GRACE).await;
    }
}

/// Own one session: route inbound messages to events, execute queued sends.
/// Returns when the session ends (peer CLOSE, idle timeout, or local close).
async fn drive_session(
    mut conn: AsyncGtpConnection,
    remote: SocketAddr,
    mut commands: mpsc::Receiver<Command>,
    events_tx: &mpsc::Sender<TransportEvent>,
) {
    // Learned from PayloadTooLarge; frames above it go reliable-unordered.
    let mut unreliable_max = usize::MAX;
    loop {
        tokio::select! {
            inbound = conn.recv() => {
                let Some(msg) = inbound else { return };
                let event = match msg.class {
                    MessageClass::Unreliable | MessageClass::ReliableUnordered => {
                        TransportEvent::Frame { remote, data: Bytes::from(msg.payload) }
                    }
                    MessageClass::ReliableOrdered { group_id, .. } if group_id == CONTROL_GROUP => {
                        match control::decode(&msg.payload) {
                            Ok(message) => TransportEvent::Control { remote, message },
                            Err(e) => {
                                warn!("[ZGALAXY GTP] malformed control message from {}: {:#}", remote, e);
                                continue;
                            }
                        }
                    }
                    other => {
                        debug!("[ZGALAXY GTP] ignoring {:?} message from {}", other, remote);
                        continue;
                    }
                };
                if events_tx.send(event).await.is_err() {
                    return;
                }
            }
            command = commands.recv() => match command {
                Some(Command::Frame(frame)) => {
                    if let Err(e) = send_frame_on(&conn, frame, &mut unreliable_max).await {
                        debug!("[ZGALAXY GTP] frame to {} dropped: {:?}", remote, e);
                    }
                }
                Some(Command::Control(payload, done)) => {
                    let r = conn
                        .send_reliable_ordered(CONTROL_GROUP, payload, PriorityTier::P0Control)
                        .await
                        .map(|_| ())
                        .map_err(|e| anyhow!("GTP control send to {} failed: {:?}", remote, e));
                    let _ = done.send(r);
                }
                Some(Command::Close(ack)) => {
                    let _ = conn.graceful_close(0, "zgalaxy-close").await;
                    let _ = ack.send(());
                    return;
                }
                // Every handle dropped (transport gone): close, don't leak.
                None => {
                    let _ = conn.graceful_close(0, "zgalaxy-shutdown").await;
                    return;
                }
            }
        }
    }
}

async fn send_frame_on(conn: &AsyncGtpConnection, frame: Bytes, unreliable_max: &mut usize) -> gtp::Result<()> {
    if frame.len() <= *unreliable_max {
        match conn.send_unreliable(frame.to_vec(), PriorityTier::P2WorldState).await {
            Ok(_) => return Ok(()),
            Err(TransportError::PayloadTooLarge { max }) => *unreliable_max = max,
            Err(e) => return Err(e),
        }
    }
    conn.send_reliable_unordered(frame.to_vec(), PriorityTier::P2WorldState).await.map(|_| ())
}

/// [`MeshTransport`] adapter over a shared [`GtpTransport`].
#[derive(Clone)]
pub struct GtpMesh(pub Arc<GtpTransport>);

#[async_trait]
impl MeshTransport for GtpMesh {
    fn kind(&self) -> TransportKind {
        TransportKind::Gtp
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities {
            unreliable: true,
            reliable_ordered: true,
            // GTP-rs supports it (trigger_path_challenge); not used yet.
            path_migration: false,
            max_datagram_payload: UNRELIABLE_PAYLOAD,
            max_frame: MAX_FRAME_SIZE,
        }
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.0.local_addr()
    }

    async fn connect(&self, remote: SocketAddr) -> Result<()> {
        self.0.connect(remote).await
    }

    async fn send_frame(&self, remote: SocketAddr, frame: Bytes) -> Result<()> {
        self.0.send_frame(remote, frame).await
    }

    async fn send_control(&self, remote: SocketAddr, message: &ControlMessage) -> Result<()> {
        self.0.send_control(remote, message).await
    }

    async fn connected_peers(&self) -> Vec<SocketAddr> {
        self.0.peers.read().await.keys().copied().collect()
    }

    async fn active_generation(&self, remote: SocketAddr) -> Option<SessionGeneration> {
        self.0.active(remote).await.map(|s| s.generation)
    }

    async fn close(&self, remote: SocketAddr) {
        self.0.close(remote).await
    }
}
