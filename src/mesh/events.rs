//! Events every mesh transport delivers to the engine.
//!
//! QUIC emits these directly today; a GTP backend emits the same events, so
//! the engine (`crate::control_engine`) never needs to know which transport
//! produced them.

use std::net::SocketAddr;

use bytes::Bytes;

use crate::control::ControlMessage;

/// Identifies one concrete session to a remote endpoint. When a session is
/// replaced (peer restarted, both sides dialed at once) the new one gets a
/// new generation, so teardown events of the old one can be told apart.
pub type SessionGeneration = u64;

#[derive(Debug)]
pub enum TransportEvent {
    /// A session to `remote` became the active one. Delivered BEFORE any
    /// frame or control message of that session.
    Connected { remote: SocketAddr, generation: SessionGeneration },
    /// A session ended. Ignore it when `generation` is no longer the active
    /// session for `remote` (it was already replaced).
    Disconnected { remote: SocketAddr, generation: SessionGeneration },
    /// A data-plane Ethernet frame.
    Frame { remote: SocketAddr, data: Bytes },
    /// A control-plane message from the reliable channel.
    Control { remote: SocketAddr, message: ControlMessage },
}

impl TransportEvent {
    pub fn remote(&self) -> SocketAddr {
        match self {
            TransportEvent::Connected { remote, .. }
            | TransportEvent::Disconnected { remote, .. }
            | TransportEvent::Frame { remote, .. }
            | TransportEvent::Control { remote, .. } => *remote,
        }
    }
}
