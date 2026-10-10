//! Transport identity and capability descriptors.
//!
//! Two independent questions (docs/GTP-INTEGRATION.md §2):
//! - *How do I reach a peer?* → [`PathMode`] (direct or via a relay).
//! - *Which transport runs inside that path?* → [`TransportKind`].

use std::fmt;
use std::str::FromStr;

/// Ethernet header carried in front of every mesh frame (dst + src + type).
pub const ETHERNET_HEADER_LEN: usize = 14;

/// Canonical ZeroTier virtual-interface MTU. ≥ 1280 so IPv6 (RFC 8200) works.
pub const CANONICAL_MTU: u32 = 2800;

/// Smallest MTU that still carries IPv6 (RFC 8200 §5).
pub const IPV6_MIN_MTU: u32 = 1280;

/// The protocol a mesh path runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// QUIC (quinn) — datagrams for frames, bi-streams for control.
    Quic,
    /// GTP/1.1 (GTP-rs) — Unreliable for frames, ReliableOrdered for control.
    /// Available when built with the `gtp` feature (`crate::gtp_transport`).
    Gtp,
    /// Legacy plaintext UDP wire bridge (`src/transport.rs`).
    Udp,
}

impl TransportKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransportKind::Quic => "quic",
            TransportKind::Gtp => "gtp",
            TransportKind::Udp => "udp",
        }
    }

    /// Whether a mesh backend for this kind is compiled into this build.
    pub fn is_available(&self) -> bool {
        match self {
            TransportKind::Quic | TransportKind::Udp => true,
            TransportKind::Gtp => cfg!(feature = "gtp"),
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TransportKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "quic" => Ok(TransportKind::Quic),
            "gtp" => Ok(TransportKind::Gtp),
            "udp" => Ok(TransportKind::Udp),
            other => anyhow::bail!("unknown transport mode '{}' (expected quic | gtp | udp)", other),
        }
    }
}

/// How a peer is reached. Only `Direct` exists today; `Relay` is the
/// fallback path planned in docs/GTP-INTEGRATION.md (Phase 5+).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PathMode {
    #[default]
    Direct,
    Relay,
}

/// What a transport can do. The data plane sizes the virtual interface from
/// these numbers instead of hard-coding one transport's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilities {
    /// Unreliable, unordered single-packet delivery exists (QUIC datagram,
    /// GTP Unreliable).
    pub unreliable: bool,
    /// A reliable ordered channel exists for control messages.
    pub reliable_ordered: bool,
    /// The transport can move a live session to a new network path.
    pub path_migration: bool,
    /// Largest frame that still goes out as ONE unreliable packet, at the
    /// minimum path MTU. The real limit can grow once PMTU discovery runs.
    pub max_datagram_payload: usize,
    /// Largest frame the transport accepts at all. Frames above
    /// `max_datagram_payload` take a fallback path (e.g. QUIC uni-stream).
    pub max_frame: usize,
}

impl TransportCapabilities {
    /// Virtual-interface MTU this transport can carry: the canonical MTU,
    /// capped so a full Ethernet frame still fits in `max_frame`.
    pub fn interface_mtu(&self) -> u32 {
        let cap = self.max_frame.saturating_sub(ETHERNET_HEADER_LEN);
        CANONICAL_MTU.min(u32::try_from(cap).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_kind_parses_case_insensitively() {
        assert_eq!("QUIC".parse::<TransportKind>().unwrap(), TransportKind::Quic);
        assert_eq!(" gtp ".parse::<TransportKind>().unwrap(), TransportKind::Gtp);
        assert_eq!("udp".parse::<TransportKind>().unwrap(), TransportKind::Udp);
        assert!("tcp".parse::<TransportKind>().is_err());
        assert_eq!(TransportKind::Gtp.is_available(), cfg!(feature = "gtp"));
    }

    #[test]
    fn interface_mtu_follows_transport_capabilities() {
        let roomy = TransportCapabilities {
            unreliable: true,
            reliable_ordered: true,
            path_migration: false,
            max_datagram_payload: 1100,
            max_frame: 64 * 1024,
        };
        assert_eq!(roomy.interface_mtu(), CANONICAL_MTU);

        let tight = TransportCapabilities { max_frame: 1200, ..roomy };
        assert_eq!(tight.interface_mtu(), 1186);
    }
}
