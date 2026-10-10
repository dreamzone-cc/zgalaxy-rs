//! ZGALAXY control-plane messages — transport independent.
//!
//! These are ZGALAXY semantics (identity binding, network configuration,
//! RTT probes), not QUIC semantics: every mesh transport (QUIC today, GTP
//! next — see docs/GTP-INTEGRATION.md) carries them on its reliable channel
//! using the same envelope: a 4-byte big-endian length prefix followed by a
//! JSON body.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Upper bound for one encoded control message body. Network configs are
/// small JSON documents; anything beyond this is malformed or hostile.
pub const MAX_CONTROL_MESSAGE_LEN: usize = 1 << 20;

/// Messages exchanged over a transport's reliable control channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ControlMessage {
    /// Announce this node's ZGALAXY address so the peer can bind
    /// the connection to the right peer entry (app-level identity).
    /// `public_identity` must be a full "<address>:0:<pubkey>" string whose
    /// pubkey derives exactly `address`; the responder challenges it.
    NodeAnnounce { address: String, public_identity: String },
    /// Responder challenge: prove possession of the announced identity by
    /// signing (domain || nonce || address) with its secret key.
    NodeChallenge { nonce: u64 },
    /// Proof of possession: ed25519 signature over the challenge material,
    /// produced by the secret key matching the announced public identity.
    AnnounceProof { nonce: u64, address: String, signature: String, public_identity: String },
    /// Verifier acknowledges a valid proof — the announcer may now treat
    /// this connection as fully trusted (send config requests etc.).
    AnnounceAccepted { address: String },
    /// Request the network configuration for a network this node joined.
    /// `token` carries the controller-signed membership token (COM
    /// replacement) when the client holds one from a previous response.
    NetworkConfigRequest { nwid: String, token: Option<String> },
    /// Controller-side response with the effective network configuration.
    NetworkConfigResponse { nwid: String, config: serde_json::Value },
    /// Ping over the control channel (diagnostics / RTT measurement).
    Ping { nonce: u64, sent_ms: u64 },
    Pong { nonce: u64, sent_ms: u64, received_ms: u64 },
}

impl ControlMessage {
    /// Short name for logs and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            ControlMessage::NodeAnnounce { .. } => "NodeAnnounce",
            ControlMessage::NodeChallenge { .. } => "NodeChallenge",
            ControlMessage::AnnounceProof { .. } => "AnnounceProof",
            ControlMessage::AnnounceAccepted { .. } => "AnnounceAccepted",
            ControlMessage::NetworkConfigRequest { .. } => "NetworkConfigRequest",
            ControlMessage::NetworkConfigResponse { .. } => "NetworkConfigResponse",
            ControlMessage::Ping { .. } => "Ping",
            ControlMessage::Pong { .. } => "Pong",
        }
    }
}

/// Encode a message with its 4-byte length prefix.
pub fn encode(msg: &ControlMessage) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(msg)?;
    if json.len() > MAX_CONTROL_MESSAGE_LEN {
        bail!("control message too large ({} bytes)", json.len());
    }
    let len = (json.len() as u32).to_be_bytes();
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(&json);
    Ok(out)
}

/// Decode a message body (without the length prefix).
pub fn decode_body(body: &[u8]) -> Result<ControlMessage> {
    Ok(serde_json::from_slice(body)?)
}

/// Decode one length-prefixed message from the start of `buf`.
pub fn decode(buf: &[u8]) -> Result<ControlMessage> {
    if buf.len() < 4 {
        bail!("control message too short");
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_CONTROL_MESSAGE_LEN {
        bail!("control message too large ({} bytes)", len);
    }
    if buf.len() < 4 + len {
        bail!("control message truncated");
    }
    decode_body(&buf[4..4 + len])
}

/// Domain-separated challenge material: sig = ed25519_sign(material).
pub fn challenge_material(nonce: u64, address: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(3 + 8 + address.len());
    m.extend_from_slice(b"ZG1");
    m.extend_from_slice(&nonce.to_be_bytes());
    m.extend_from_slice(address.as_bytes());
    m
}

/// Milliseconds since the Unix epoch (wall clock) for Ping/Pong stamps.
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_wire_format() {
        let msg = ControlMessage::NetworkConfigRequest { nwid: "0123456789abcdef".into(), token: None };
        let buf = encode(&msg).unwrap();
        // Wire format is unchanged from the QUIC-only era: length || JSON.
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(len, buf.len() - 4);
        assert!(std::str::from_utf8(&buf[4..]).unwrap().contains("\"type\":\"NetworkConfigRequest\""));
        match decode(&buf).unwrap() {
            ControlMessage::NetworkConfigRequest { nwid, token } => {
                assert_eq!(nwid, "0123456789abcdef");
                assert!(token.is_none());
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn decode_rejects_short_truncated_and_oversized() {
        assert!(decode(&[0, 0]).is_err());
        assert!(decode(&[0, 0, 0, 10, b'{']).is_err());
        assert!(decode(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn challenge_material_is_domain_separated() {
        let m = challenge_material(7, "abcdef0123");
        assert_eq!(&m[..3], b"ZG1");
        assert_eq!(&m[3..11], &7u64.to_be_bytes());
        assert_eq!(&m[11..], b"abcdef0123");
    }
}
