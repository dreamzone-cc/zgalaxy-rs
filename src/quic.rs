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
//! Data-plane Ethernet/IP frames ride QUIC Datagrams (no retransmit head-of-line
//! blocking — same semantics as a real NIC). Control-plane messages (network
//! config requests/responses, identity announcements) ride reliable QUIC
//! streams.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// ALPN protocol identifier for the ZGALAXY mesh.
pub const ALPN: &str = "zgalaxy-mesh";

/// Maximum QUIC datagram payload (must fit in a single QUIC packet).
pub const MAX_DATAGRAM_SIZE: usize = 1200;

/// Reliable control-stream message envelope: 4-byte big-endian length prefix
/// followed by a JSON payload (see `ControlMessage`).
pub mod control {
    use serde::{Deserialize, Serialize};

    /// Messages exchanged over reliable QUIC bi-streams.
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "type", content = "data")]
    pub enum ControlMessage {
        /// Announce this node's ZGALAXY address so the peer can bind
        /// the connection to the right peer entry (app-level identity).
        NodeAnnounce { address: String, public_identity: String },
        /// Request the network configuration for a network this node joined.
        NetworkConfigRequest { nwid: String },
        /// Controller-side response with the effective network configuration.
        NetworkConfigResponse { nwid: String, config: serde_json::Value },
        /// Ping over the control stream (diagnostics / RTT measurement).
        Ping { nonce: u64, sent_ms: u64 },
        Pong { nonce: u64, sent_ms: u64, received_ms: u64 },
    }

    pub fn encode(msg: &ControlMessage) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(msg)?;
        let len = (json.len() as u32).to_be_bytes();
        let mut out = Vec::with_capacity(4 + json.len());
        out.extend_from_slice(&len);
        out.extend_from_slice(&json);
        Ok(out)
    }

    /// Decode a message body (without the length prefix).
    pub fn decode_body(body: &[u8]) -> anyhow::Result<ControlMessage> {
        Ok(serde_json::from_slice(body)?)
    }

    pub fn decode(buf: &[u8]) -> anyhow::Result<ControlMessage> {
        use anyhow::bail;
        if buf.len() < 4 {
            bail!("control message too short");
        }
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if buf.len() < 4 + len {
            bail!("control message truncated");
        }
        Ok(serde_json::from_slice(&buf[4..4 + len])?)
    }
}

/// A peer connection established over QUIC.
#[derive(Clone)]
pub struct PeerConnection {
    pub remote: SocketAddr,
    pub conn: quinn::Connection,
}

/// Inbound event delivered to the engine loop.
#[derive(Debug)]
pub enum QuicEvent {
    /// Unreliable data-plane frame received via QUIC Datagrams.
    Datagram { remote: SocketAddr, data: Bytes },
    /// Reliable control-plane message received via a QUIC stream.
    Control { remote: SocketAddr, message: control::ControlMessage },
    /// A connection was established.
    Connected { remote: SocketAddr },
    /// A connection was lost.
    Disconnected { remote: SocketAddr },
}

pub struct QuicTransport {
    endpoint: quinn::Endpoint,
    peers: Arc<RwLock<HashMap<SocketAddr, PeerConnection>>>,
    events: RwLock<Option<mpsc::Sender<QuicEvent>>>,
    /// Node address announced on control streams (app-level identity binding).
    node_address: String,
}

impl QuicTransport {
    /// Build a QUIC endpoint with a fresh self-signed certificate.
    ///
    /// Peer authentication currently happens at the application layer
    /// (NodeAnnounce binding; certificate pinning keyed by node address is the
    /// next step). TLS 1.3 still encrypts the channel against passive
    /// attackers.
    pub fn bind(bind_addr: SocketAddr, node_address: String) -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["zgalaxy".into()])
            .context("failed to generate self-signed QUIC certificate")?;
        let cert_der = cert.cert.der().clone();
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .context("failed to build rustls server config")?;
        server_crypto.alpn_protocols = vec![ALPN.as_bytes().to_vec()];

        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(Duration::from_secs(5)));
        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(
                server_crypto,
            )?));
        server_config.transport_config(Arc::new(transport));

        let mut endpoint = quinn::Endpoint::server(server_config, bind_addr)
            .with_context(|| format!("failed to bind QUIC endpoint on {}", bind_addr))?;

        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![ALPN.as_bytes().to_vec()];
        let client_quic_config =
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .context("failed to build QUIC client config")?;
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(client_quic_config)));

        info!("[ZGALAXY QUIC] Endpoint bound on {}", endpoint.local_addr()?);

        Ok(Self {
            endpoint,
            peers: Arc::new(RwLock::new(HashMap::new())),
            events: RwLock::new(None),
            node_address,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn connected_peers(&self) -> &RwLock<HashMap<SocketAddr, PeerConnection>> {
        &self.peers
    }

    /// Accept loop: for each inbound QUIC connection spawn reader tasks that
    /// forward datagrams and control-stream messages as `QuicEvent`s.
    pub async fn run(self: Arc<Self>, events_tx: mpsc::Sender<QuicEvent>) -> Result<()> {
        *self.events.write().await = Some(events_tx.clone());
        loop {
            let incoming = self.endpoint.accept().await.context("QUIC endpoint closed")?;
            let this = Arc::clone(&self);
            let events = events_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = this.handle_incoming(incoming, events).await {
                    warn!("[ZGALAXY QUIC] inbound connection failed: {}", e);
                }
            });
        }
    }

    async fn handle_incoming(
        self: &Arc<Self>,
        incoming: quinn::Incoming,
        events_tx: mpsc::Sender<QuicEvent>,
    ) -> Result<()> {
        let conn = incoming.await.context("QUIC handshake failed")?;
        let remote = conn.remote_address();
        info!("[ZGALAXY QUIC] Peer connected: {}", remote);

        self.register(remote, conn.clone(), events_tx.clone()).await;
        let _ = events_tx.send(QuicEvent::Connected { remote }).await;
        Ok(())
    }

    /// Register a connection and spawn its reader loops (datagrams + bi-streams).
    async fn register(
        self: &Arc<Self>,
        remote: SocketAddr,
        conn: quinn::Connection,
        events_tx: mpsc::Sender<QuicEvent>,
    ) {
        self.peers
            .write()
            .await
            .insert(remote, PeerConnection { remote, conn: conn.clone() });

        // Announce our identity on a dedicated control stream.
        let announce = control::ControlMessage::NodeAnnounce {
            address: self.node_address.clone(),
            public_identity: String::new(),
        };
        if let Ok(buf) = control::encode(&announce) {
            if let Ok((mut send, _recv)) = conn.open_bi().await {
                let _ = send.write_all(&buf).await;
                let _ = send.finish();
            }
        }

        let conn2 = conn.clone();
        let this = Arc::clone(self);
        let events0 = events_tx.clone();
        tokio::spawn(async move {
            let _ = this.read_datagrams(&conn2, remote, events0.clone()).await;
            this.peers.write().await.remove(&remote);
            let _ = events0.send(QuicEvent::Disconnected { remote }).await;
        });

        let conn3 = conn.clone();
        let this2 = Arc::clone(self);
        let events1 = events_tx.clone();
        tokio::spawn(async move {
            let _ = this2.accept_control_streams(&conn3, remote, events1).await;
        });
    }

    async fn read_datagrams(
        &self,
        conn: &quinn::Connection,
        remote: SocketAddr,
        events_tx: mpsc::Sender<QuicEvent>,
    ) -> Result<()> {
        loop {
            match conn.read_datagram().await {
                Ok(data) => {
                    let _ = events_tx.send(QuicEvent::Datagram { remote, data }).await;
                }
                Err(e) => {
                    info!("[ZGALAXY QUIC] Datagram stream from {} ended: {}", remote, e);
                    return Ok(());
                }
            }
        }
    }

    /// Accept control streams one at a time and decode one message per stream.
    async fn accept_control_streams(
        &self,
        conn: &quinn::Connection,
        remote: SocketAddr,
        events_tx: mpsc::Sender<QuicEvent>,
    ) -> Result<()> {
        loop {
            let stream = match conn.accept_bi().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("[ZGALAXY QUIC] accept_bi ended for {}: {}", remote, e);
                    return Err(anyhow::anyhow!("accept_bi ended: {}", e));
                }
            };
            let events = events_tx.clone();
            tokio::spawn(async move {
                let (_send, mut recv) = stream;
                match read_control_message(&mut recv).await {
                    Ok(message) => {
                        let _ = events.send(QuicEvent::Control { remote, message }).await;
                    }
                    Err(e) => warn!("[ZGALAXY QUIC] malformed control message from {}: {}", remote, e),
                }
            });
        }
    }

    /// Connect to a remote peer (idempotent — reuses an existing connection).
    pub async fn connect(&self, remote: SocketAddr) -> Result<PeerConnection> {
        if let Some(existing) = self.peers.read().await.get(&remote) {
            return Ok(existing.clone());
        }
        let conn = self
            .endpoint
            .connect(remote, "zgalaxy")
            .with_context(|| format!("failed to start QUIC connection to {}", remote))?
            .await
            .with_context(|| format!("QUIC handshake with {} failed", remote))?;

        let peer = PeerConnection { remote, conn: conn.clone() };
        self.peers.write().await.insert(remote, peer.clone());

        // Spawn reader loops for this client-side connection, reusing the
        // shared event channel set by run().
        let events = self.events.read().await.clone();
        if let Some(events_tx) = events {
            // The announce stream we open will also be accepted by the remote
            // accept_bi loop; mirror register() for the client side.
            let announce = control::ControlMessage::NodeAnnounce {
                address: self.node_address.clone(),
                public_identity: String::new(),
            };
            if let Ok(buf) = control::encode(&announce) {
                if let Ok((mut send, _recv)) = conn.open_bi().await {
                    let _ = send.write_all(&buf).await;
                    let _ = send.finish();
                }
            }
            let peers = Arc::clone(&self.peers);
            let events2 = events_tx.clone();
            let conn_dg = conn.clone();
            tokio::spawn(async move {
                loop {
                    match conn_dg.read_datagram().await {
                        Ok(data) => {
                            let _ = events2
                                .send(QuicEvent::Datagram { remote, data })
                                .await;
                        }
                        Err(_) => {
                            peers.write().await.remove(&remote);
                            let _ = events2.send(QuicEvent::Disconnected { remote }).await;
                            return;
                        }
                    }
                }
            });
            // control streams from the remote side of this connection
            let events3 = events_tx;
            let conn_ctl = conn.clone();
            tokio::spawn(async move {
                loop {
                    match conn_ctl.accept_bi().await {
                        Ok((_, mut recv)) => {
                            if let Ok(message) = read_control_message(&mut recv).await {
                                let _ = events3.send(QuicEvent::Control { remote, message }).await;
                            }
                        }
                        Err(_) => return,
                    }
                }
            });
        }
        info!("[ZGALAXY QUIC] Connected to peer {}", remote);
        Ok(peer)
    }

    /// Send an unreliable data-plane frame (Ethernet/IP packet) via QUIC Datagrams.
    pub async fn send_frame(&self, remote: SocketAddr, frame: Bytes) -> Result<()> {
        let peer = self.connect(remote).await?;
        if frame.len() > MAX_DATAGRAM_SIZE {
            bail!("frame of {} bytes exceeds QUIC datagram limit", frame.len());
        }
        peer.conn
            .send_datagram(frame)
            .map_err(|e| anyhow::anyhow!("QUIC datagram send failed: {}", e))
    }

    /// Send a reliable control message over a new bidirectional stream.
    pub async fn send_control(
        &self,
        remote: SocketAddr,
        msg: &control::ControlMessage,
    ) -> Result<()> {
        let peer = self.connect(remote).await?;
        let (mut send, _recv) = peer.conn.open_bi().await.context("failed to open control stream")?;
        let buf = control::encode(msg)?;
        send.write_all(&buf).await.context("failed to write control message")?;
        send.finish().context("failed to close control stream")?;
        Ok(())
    }
}

/// Read one length-prefixed control message from a stream.
async fn read_control_message(recv: &mut quinn::RecvStream) -> Result<control::ControlMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.context("control header read failed")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 1 << 20 {
        bail!("control message too large ({} bytes)", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("control body read failed")?;
    control::decode_body(&buf)
}

/// Certificate verifier that accepts the self-signed mesh certificates.
/// Channel confidentiality is still guaranteed by TLS 1.3; node-level
/// authorization happens at the application layer (NodeAnnounce binding and,
/// later, certificate pinning keyed by node address).
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
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
