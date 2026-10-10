//! The transport abstraction the engine is written against.

use std::net::SocketAddr;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use super::capabilities::{TransportCapabilities, TransportKind};
use super::events::SessionGeneration;
use crate::control::ControlMessage;

/// A mesh transport backend (QUIC today, GTP next).
///
/// Shaped by what zgalaxy-rs needs, not by QUIC: dial a peer, push frames
/// unreliably, push control messages reliably, and report sessions.
/// Inbound traffic arrives as [`super::TransportEvent`]s on the channel the
/// backend was started with.
///
/// Peers are keyed by their physical endpoint for now (the identity layer —
/// [`super::PeerSessions`] — maps endpoints to proven node addresses).
#[async_trait]
pub trait MeshTransport: Send + Sync {
    fn kind(&self) -> TransportKind;

    fn capabilities(&self) -> TransportCapabilities;

    fn local_addr(&self) -> Result<SocketAddr>;

    /// Ensure a session to `remote` exists (idempotent).
    async fn connect(&self, remote: SocketAddr) -> Result<()>;

    /// Send one data-plane frame. Unreliable when it fits one packet.
    async fn send_frame(&self, remote: SocketAddr, frame: Bytes) -> Result<()>;

    /// Send one control message on the reliable channel.
    async fn send_control(&self, remote: SocketAddr, message: &ControlMessage) -> Result<()>;

    /// Endpoints with a live session.
    async fn connected_peers(&self) -> Vec<SocketAddr>;

    /// Generation of the active session to `remote`, if one exists. A
    /// `Disconnected` whose generation differs from this (while it is Some)
    /// belongs to a session that was already replaced.
    async fn active_generation(&self, remote: SocketAddr) -> Option<SessionGeneration>;

    /// Close the session to `remote`, if any.
    async fn close(&self, remote: SocketAddr);
}
