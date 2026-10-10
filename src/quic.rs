//! QUIC Transport Layer (primary transport for ZGALAXY-RS)
//!
//! Architectural constraint (project requirements §5): zgalaxy-rs uses QUIC as
//! its transport, NOT raw UDP. ZeroTier mechanisms designed for raw UDP are
//! intentionally NOT copied — their *goals* are re-implemented on top of QUIC:
//!
//! | ZeroTier (raw UDP) mechanism        | QUIC equivalent                                  |
//! |-------------------------------------|--------------------------------------------------|
//! | Custom keyed-MAC + Salsa20 payload  | TLS 1.3 encryption & authentication (built-in)   |
//! | Custom handshake / identity proof   | TLS handshake + app-level node-address binding   |
//! | Keepalive probes                    | QUIC keep-alives / idle timeout                   |
//! | Fragmentation below path MTU        | QUIC packetization & PMTU discovery               |
//! | Reliability for control messages    | QUIC streams (reliable, ordered)                  |
//! | Unreliable L2/L3 frame forwarding   | QUIC Datagrams (RFC 9221)                         |
//! | Path failover / direct paths        | QUIC connection migration (future)                |
//!
//! Data-plane Ethernet frames ride QUIC Datagrams (no retransmit head-of-line
//! blocking — same semantics as a real NIC). A frame larger than the current
//! datagram limit (the interface MTU is 2800 so IPv6 works) rides its own
//! unidirectional stream instead of being dropped. Control-plane messages
//! ride reliable bi-streams, one message per stream.
//!
//! This module is a pure transport: identity binding lives in
//! [`crate::mesh::PeerSessions`] and message handling in
//! [`crate::control_engine`]. The [`crate::mesh::MeshTransport`] impl at the
//! bottom is what the engine talks to.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

pub use crate::control;
pub use crate::control::challenge_material;
use crate::control::ControlMessage;
use crate::mesh::{MeshTransport, SessionGeneration, TransportCapabilities, TransportEvent, TransportKind};

/// Inbound event type. Kept as an alias so existing callers keep compiling;
/// it is the transport-independent [`TransportEvent`].
pub type QuicEvent = TransportEvent;

/// ALPN protocol identifier for the ZGALAXY mesh.
pub const ALPN: &str = "zgalaxy-mesh";

/// Conservative QUIC datagram payload before PMTU discovery raises it. The
/// live limit is `quinn::Connection::max_datagram_size()`.
pub const MAX_DATAGRAM_SIZE: usize = 1200;

/// Largest frame accepted on the stream fallback path.
pub const MAX_FRAME_SIZE: usize = 64 * 1024;

/// A control message must arrive completely within this window.
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// An oversized frame waits at most this long for stream credit; the data
/// plane never stalls behind one slow peer.
const FRAME_STREAM_OPEN_TIMEOUT: Duration = Duration::from_millis(50);

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);
/// ب4: bounded dead-peer detection — a lost peer is declared within ~15s.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// Canonical form of a remote endpoint: an IPv4 peer reached through a
/// dual-stack `[::]` socket is reported as `::ffff:a.b.c.d`, while the same
/// peer is dialed as `a.b.c.d`. Keying sessions by the raw form made every
/// dial miss the live session and replace it — reconnect churn on every
/// sync cycle. All session keys and events use the canonical form.
pub fn canonical_endpoint(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// A peer connection established over QUIC.
#[derive(Clone)]
pub struct PeerConnection {
    pub remote: SocketAddr,
    pub conn: quinn::Connection,
    /// Set once the peer withheld stream credit for an oversized frame.
    /// Peers older than the stream fallback never accept uni streams, so
    /// their credit never comes back: stop trying instead of paying the open
    /// timeout on every large frame.
    stream_fallback_off: Arc<std::sync::atomic::AtomicBool>,
}

impl PeerConnection {
    fn new(remote: SocketAddr, conn: quinn::Connection) -> Self {
        Self { remote, conn, stream_fallback_off: Arc::default() }
    }
}

pub struct QuicTransport {
    endpoint: quinn::Endpoint,
    peers: Arc<RwLock<HashMap<SocketAddr, PeerConnection>>>,
    events: RwLock<Option<mpsc::Sender<TransportEvent>>>,
    /// One dial lock per remote: concurrent dials to the same remote
    /// collapse into one handshake, while a dead remote never blocks dials
    /// to the others.
    dial_locks: Mutex<HashMap<SocketAddr, Arc<Mutex<()>>>>,
}

fn transport_config() -> quinn::TransportConfig {
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    transport.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().expect("valid idle timeout")));
    // Each oversized frame uses one short-lived uni stream.
    transport.max_concurrent_uni_streams(256u32.into());
    transport
}

impl QuicTransport {
    /// Build a QUIC endpoint with a fresh self-signed certificate.
    ///
    /// Peer authentication happens at the application layer (challenge /
    /// proof bound to the node identity, see `crate::mesh::PeerSessions`).
    /// TLS 1.3 still encrypts the channel against passive attackers.
    pub fn bind(bind_addr: SocketAddr) -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["zgalaxy".into()])
            .context("failed to generate self-signed QUIC certificate")?;
        let cert_der = cert.cert.der().clone();
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .context("failed to build rustls server config")?;
        server_crypto.alpn_protocols = vec![ALPN.as_bytes().to_vec()];

        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
        ));
        server_config.transport_config(Arc::new(transport_config()));

        let mut endpoint = quinn::Endpoint::server(server_config, bind_addr)
            .with_context(|| format!("failed to bind QUIC endpoint on {}", bind_addr))?;

        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![ALPN.as_bytes().to_vec()];
        let client_quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
            .context("failed to build QUIC client config")?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(client_quic_config));
        client_config.transport_config(Arc::new(transport_config()));
        endpoint.set_default_client_config(client_config);

        info!("[ZGALAXY QUIC] Endpoint bound on {}", endpoint.local_addr()?);

        Ok(Self {
            endpoint,
            peers: Arc::new(RwLock::new(HashMap::new())),
            events: RwLock::new(None),
            dial_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn connected_peers(&self) -> &RwLock<HashMap<SocketAddr, PeerConnection>> {
        &self.peers
    }

    /// Accept loop: for each inbound QUIC connection spawn reader tasks that
    /// forward datagrams and control-stream messages as events.
    pub async fn run(self: Arc<Self>, events_tx: mpsc::Sender<TransportEvent>) -> Result<()> {
        *self.events.write().await = Some(events_tx.clone());
        loop {
            let incoming = self.endpoint.accept().await.context("QUIC endpoint closed")?;
            let this = Arc::clone(&self);
            let events = events_tx.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        info!("[ZGALAXY QUIC] Peer connected: {}", canonical_endpoint(conn.remote_address()));
                        this.activate(conn, events).await;
                    }
                    Err(e) => warn!("[ZGALAXY QUIC] inbound handshake failed: {}", e),
                }
            });
        }
    }

    /// Make `conn` the active session for its remote and start its readers.
    ///
    /// ب4: a NEW successful handshake always wins — a stale entry (peer
    /// restarted, old connection half-dead) must never block recovery, so the
    /// old connection is closed and its slot taken. `Connected` is emitted
    /// before the readers start so the engine sees it ahead of any traffic.
    async fn activate(self: &Arc<Self>, conn: quinn::Connection, events_tx: mpsc::Sender<TransportEvent>) {
        let remote = canonical_endpoint(conn.remote_address());
        let generation = conn.stable_id() as SessionGeneration;
        {
            let mut peers = self.peers.write().await;
            if let Some(existing) = peers.get(&remote) {
                if existing.conn.stable_id() == conn.stable_id() {
                    return; // literally the same connection — already active
                }
            }
            if let Some(old) = peers.insert(remote, PeerConnection::new(remote, conn.clone())) {
                warn!(
                    "[ZGALAXY QUIC] replacing stale connection (stable_id {}) for {}",
                    old.conn.stable_id(),
                    remote
                );
                old.conn.close(quinn::VarInt::from_u32(0), b"zgalaxy-replaced");
            }
        }

        let _ = events_tx.send(TransportEvent::Connected { remote, generation }).await;

        let this = Arc::clone(self);
        let conn_dg = conn.clone();
        let events_dg = events_tx.clone();
        tokio::spawn(async move {
            read_datagrams(&conn_dg, remote, &events_dg).await;
            // Remove only if the closing connection is still the active one.
            {
                let mut peers = this.peers.write().await;
                if peers.get(&remote).map(|p| p.conn.stable_id() == conn_dg.stable_id()).unwrap_or(false) {
                    peers.remove(&remote);
                }
            }
            let _ = events_dg.send(TransportEvent::Disconnected { remote, generation }).await;
        });

        let conn_ctl = conn.clone();
        let events_ctl = events_tx.clone();
        tokio::spawn(async move { accept_control_streams(&conn_ctl, remote, &events_ctl).await });

        tokio::spawn(async move { accept_frame_streams(&conn, remote, &events_tx).await });
    }

    async fn active(&self, remote: SocketAddr) -> Option<PeerConnection> {
        self.peers.read().await.get(&canonical_endpoint(remote)).cloned()
    }

    async fn dial_lock(&self, remote: SocketAddr) -> Arc<Mutex<()>> {
        let mut locks = self.dial_locks.lock().await;
        // Drop locks nobody holds or waits on, so the map cannot grow forever.
        locks.retain(|_, l| Arc::strong_count(l) > 1);
        Arc::clone(locks.entry(remote).or_default())
    }

    /// Connect to a remote peer (idempotent — reuses an existing connection).
    /// Refuses to run before `run()` installed the event sink so inbound
    /// traffic can never be blackholed.
    pub async fn connect(self: &Arc<Self>, remote: SocketAddr) -> Result<PeerConnection> {
        let remote = canonical_endpoint(remote);
        if let Some(existing) = self.active(remote).await {
            return Ok(existing);
        }
        let Some(events) = self.events.read().await.clone() else {
            bail!("QUIC transport not started (run() must be called before connect)");
        };

        let lock = self.dial_lock(remote).await;
        let _guard = lock.lock().await;
        // Re-check after acquiring the lock — another caller may have won.
        if let Some(existing) = self.active(remote).await {
            return Ok(existing);
        }

        let conn = self
            .endpoint
            .connect(remote, "zgalaxy")
            .with_context(|| format!("failed to start QUIC connection to {}", remote))?
            .await
            .with_context(|| format!("QUIC handshake with {} failed", remote))?;
        info!("[ZGALAXY QUIC] Connected to peer {}", remote);

        self.activate(conn, events).await;
        self.active(remote)
            .await
            .with_context(|| format!("QUIC session to {} closed right after the handshake", remote))
    }

    /// Send a data-plane frame on an EXISTING session. Never dials: the data
    /// plane must not stall for a handshake timeout on a dead endpoint.
    pub async fn send_frame(&self, remote: SocketAddr, frame: Bytes) -> Result<()> {
        if frame.len() > MAX_FRAME_SIZE {
            bail!("frame of {} bytes exceeds the {} byte mesh frame limit", frame.len(), MAX_FRAME_SIZE);
        }
        let Some(peer) = self.active(remote).await else {
            bail!("no QUIC session to {}", remote);
        };
        let fits_datagram = peer.conn.max_datagram_size().map(|max| frame.len() <= max).unwrap_or(false);
        if fits_datagram {
            return peer
                .conn
                .send_datagram(frame)
                .map_err(|e| anyhow::anyhow!("QUIC datagram send failed: {}", e));
        }
        // Oversized for one datagram: one uni stream per frame.
        use std::sync::atomic::Ordering;
        if peer.stream_fallback_off.load(Ordering::Relaxed) {
            bail!("{} does not take oversized frames ({} bytes dropped)", remote, frame.len());
        }
        let mut stream = match tokio::time::timeout(FRAME_STREAM_OPEN_TIMEOUT, peer.conn.open_uni()).await {
            Ok(opened) => opened.context("failed to open frame stream")?,
            Err(_) => {
                peer.stream_fallback_off.store(true, Ordering::Relaxed);
                warn!(
                    "[ZGALAXY QUIC] {} withholds stream credit — oversized frames to it are dropped from now on \
                     (peer predates the stream fallback?)",
                    remote
                );
                bail!("no stream credit for a {} byte frame", frame.len());
            }
        };
        stream.write_chunk(frame).await.context("failed to write frame stream")?;
        stream.finish().context("failed to finish frame stream")?;
        Ok(())
    }

    /// Send a reliable control message over a new bidirectional stream.
    pub async fn send_control(self: &Arc<Self>, remote: SocketAddr, msg: &ControlMessage) -> Result<()> {
        let peer = self.connect(remote).await?;
        let (mut send, _recv) = peer.conn.open_bi().await.context("failed to open control stream")?;
        let buf = control::encode(msg)?;
        send.write_all(&buf).await.context("failed to write control message")?;
        send.finish().context("failed to close control stream")?;
        Ok(())
    }

    /// Close the session to `remote`, if any.
    pub async fn close(&self, remote: SocketAddr) {
        if let Some(peer) = self.peers.write().await.remove(&canonical_endpoint(remote)) {
            peer.conn.close(quinn::VarInt::from_u32(0), b"zgalaxy-close");
        }
    }
}

async fn read_datagrams(conn: &quinn::Connection, remote: SocketAddr, events_tx: &mpsc::Sender<TransportEvent>) {
    loop {
        match conn.read_datagram().await {
            Ok(data) => {
                if events_tx.send(TransportEvent::Frame { remote, data }).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                info!("[ZGALAXY QUIC] Datagram stream from {} ended: {}", remote, e);
                return;
            }
        }
    }
}

/// Accept control streams and decode one message per stream.
async fn accept_control_streams(conn: &quinn::Connection, remote: SocketAddr, events_tx: &mpsc::Sender<TransportEvent>) {
    loop {
        let (_send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(e) => {
                debug!("[ZGALAXY QUIC] accept_bi ended for {}: {}", remote, e);
                return;
            }
        };
        let events = events_tx.clone();
        tokio::spawn(async move {
            match tokio::time::timeout(CONTROL_READ_TIMEOUT, read_control_message(&mut recv)).await {
                Ok(Ok(message)) => {
                    let _ = events.send(TransportEvent::Control { remote, message }).await;
                }
                Ok(Err(e)) => warn!("[ZGALAXY QUIC] malformed control message from {}: {:#}", remote, e),
                Err(_) => warn!("[ZGALAXY QUIC] control message from {} timed out", remote),
            }
        });
    }
}

/// Accept oversized-frame streams: each uni stream carries exactly one frame.
async fn accept_frame_streams(conn: &quinn::Connection, remote: SocketAddr, events_tx: &mpsc::Sender<TransportEvent>) {
    loop {
        let mut recv = match conn.accept_uni().await {
            Ok(s) => s,
            Err(e) => {
                debug!("[ZGALAXY QUIC] accept_uni ended for {}: {}", remote, e);
                return;
            }
        };
        let events = events_tx.clone();
        tokio::spawn(async move {
            match tokio::time::timeout(CONTROL_READ_TIMEOUT, recv.read_to_end(MAX_FRAME_SIZE)).await {
                Ok(Ok(data)) => {
                    let _ = events.send(TransportEvent::Frame { remote, data: Bytes::from(data) }).await;
                }
                Ok(Err(e)) => debug!("[ZGALAXY QUIC] frame stream from {} failed: {}", remote, e),
                Err(_) => debug!("[ZGALAXY QUIC] frame stream from {} timed out", remote),
            }
        });
    }
}

/// Read one length-prefixed control message from a stream.
async fn read_control_message(recv: &mut quinn::RecvStream) -> Result<ControlMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.context("control header read failed")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > control::MAX_CONTROL_MESSAGE_LEN {
        bail!("control message too large ({} bytes)", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("control body read failed")?;
    control::decode_body(&buf)
}

/// [`MeshTransport`] adapter over a shared [`QuicTransport`].
#[derive(Clone)]
pub struct QuicMesh(pub Arc<QuicTransport>);

#[async_trait]
impl MeshTransport for QuicMesh {
    fn kind(&self) -> TransportKind {
        TransportKind::Quic
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities {
            unreliable: true,
            reliable_ordered: true,
            path_migration: false,
            max_datagram_payload: MAX_DATAGRAM_SIZE,
            max_frame: MAX_FRAME_SIZE,
        }
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.0.local_addr()
    }

    async fn connect(&self, remote: SocketAddr) -> Result<()> {
        self.0.connect(remote).await.map(|_| ())
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
        self.0.active(remote).await.map(|p| p.conn.stable_id() as SessionGeneration)
    }

    async fn close(&self, remote: SocketAddr) {
        self.0.close(remote).await
    }
}

/// Certificate verifier that accepts the self-signed mesh certificates.
/// Channel confidentiality is still guaranteed by TLS 1.3; node-level
/// authorization happens at the application layer (challenge/proof bound to
/// the node identity).
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
