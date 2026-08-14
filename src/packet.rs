use bytes::{Buf, BufMut, Bytes, BytesMut};
use crate::identity::Address;
use anyhow::{bail, Result};

/// ZeroTier Protocol Wire Constants
pub const PROTO_MAX_HOPS: u8 = 7;
pub const PACKET_HEADER_SIZE: usize = 28;

/// ZeroTier / ZGALAXY Wire Protocol Verbs (Exact match with canonical ZeroTier Packet.hpp)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Nop = 0x00,
    Hello = 0x01,
    Error = 0x02,
    Ok = 0x03,
    Whois = 0x04,
    Rendezvous = 0x05,
    Frame = 0x06,
    ExtFrame = 0x07,
    Echo = 0x08,
    Pong = 0x0f,
    MulticastLike = 0x09,
    NetworkCredentials = 0x0a,
    NetworkConfigRequest = 0x0b,
    NetworkConfig = 0x0c,
    MulticastGather = 0x0d,
    MulticastFrame = 0x0e,
    PushDirectPaths = 0x10,
    UserMessage = 0x12,
    RemoteTrace = 0x13,
    Unknown(u8),
}

impl From<u8> for PacketType {
    fn from(val: u8) -> Self {
        match val {
            0x00 => PacketType::Nop,
            0x01 => PacketType::Hello,
            0x02 => PacketType::Error,
            0x03 => PacketType::Ok,
            0x04 => PacketType::Whois,
            0x05 => PacketType::Rendezvous,
            0x06 => PacketType::Frame,
            0x07 => PacketType::ExtFrame,
            0x08 => PacketType::Echo,
            0x09 => PacketType::MulticastLike,
            0x0a => PacketType::NetworkCredentials,
            0x0b => PacketType::NetworkConfigRequest,
            0x0c => PacketType::NetworkConfig,
            0x0d => PacketType::MulticastGather,
            0x0e => PacketType::MulticastFrame,
            0x0f => PacketType::Pong,
            0x10 => PacketType::PushDirectPaths,
            0x12 => PacketType::UserMessage,
            0x13 => PacketType::RemoteTrace,
            other => PacketType::Unknown(other),
        }
    }
}

impl From<PacketType> for u8 {
    fn from(pt: PacketType) -> Self {
        match pt {
            PacketType::Nop => 0x00,
            PacketType::Hello => 0x01,
            PacketType::Error => 0x02,
            PacketType::Ok => 0x03,
            PacketType::Whois => 0x04,
            PacketType::Rendezvous => 0x05,
            PacketType::Frame => 0x06,
            PacketType::ExtFrame => 0x07,
            PacketType::Echo => 0x08,
            PacketType::Pong => 0x0f,
            PacketType::MulticastLike => 0x09,
            PacketType::NetworkCredentials => 0x0a,
            PacketType::NetworkConfigRequest => 0x0b,
            PacketType::NetworkConfig => 0x0c,
            PacketType::MulticastGather => 0x0d,
            PacketType::MulticastFrame => 0x0e,
            PacketType::PushDirectPaths => 0x10,
            PacketType::UserMessage => 0x12,
            PacketType::RemoteTrace => 0x13,
            PacketType::Unknown(v) => v,
        }
    }
}

/// ZeroTier 28-Byte Wire-Protocol Packet Structure
/// Format:
/// - [0..8]  : 64-bit Packet ID / Crypto IV
/// - [8..13] : 5-byte Destination ZT Address
/// - [13..18]: 5-byte Source ZT Address
/// - [18]    : Flags / Cipher / Hops (1 byte)
/// - [19..27]: 64-bit MAC (8 bytes)
/// - [27]    : Encrypted Flags & Verb (1 byte)
/// - [28..]  : Verb Payload
#[derive(Debug, Clone)]
pub struct Packet {
    pub packet_id: u64,
    pub dest: Address,
    pub source: Address,
    pub flags: u8,
    pub mac: u64,
    pub packet_type: PacketType,
    pub payload: Bytes,
}

impl Packet {
    pub const HEADER_SIZE: usize = PACKET_HEADER_SIZE; // 28 bytes

    pub fn new(dest: Address, source: Address, packet_id: u64, packet_type: PacketType, payload: Bytes) -> Self {
        Packet {
            packet_id,
            dest,
            source,
            flags: 0,
            mac: 0,
            packet_type,
            payload,
        }
    }

    /// Serialize packet into canonical ZeroTier 28-byte wire-format bytes.
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(Self::HEADER_SIZE + self.payload.len());
        buf.put_u64(self.packet_id);
        buf.put_slice(self.dest.as_bytes());
        buf.put_slice(self.source.as_bytes());
        buf.put_u8(self.flags);
        buf.put_u64(self.mac);
        buf.put_u8(self.packet_type.into());
        buf.put_slice(&self.payload);
        buf.freeze()
    }

    /// Parse wire-format bytes into a Packet structure.
    pub fn decode(mut data: Bytes) -> Result<Self> {
        if data.len() < Self::HEADER_SIZE {
            bail!("Packet truncated: expected at least {} bytes, got {}", Self::HEADER_SIZE, data.len());
        }

        let packet_id = data.get_u64();

        let mut dest_bytes = [0u8; 5];
        data.copy_to_slice(&mut dest_bytes);
        let dest = Address(dest_bytes);

        let mut src_bytes = [0u8; 5];
        data.copy_to_slice(&mut src_bytes);
        let source = Address(src_bytes);

        let flags = data.get_u8();
        let mac = data.get_u64();
        let packet_type = PacketType::from(data.get_u8());
        let payload = data;

        Ok(Packet {
            packet_id,
            dest,
            source,
            flags,
            mac,
            packet_type,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_encode_decode() {
        let dest = Address([0x06, 0x9a, 0xe3, 0x80, 0x92]);
        let src = Address([0x12, 0x34, 0x56, 0x78, 0x9a]);
        let payload = Bytes::from_static(b"PING_ZGALAXY_ROOT");

        let pkt = Packet::new(dest, src, 1001, PacketType::Echo, payload.clone());
        let encoded = pkt.encode();

        let decoded = Packet::decode(encoded).unwrap();
        assert_eq!(decoded.dest, dest);
        assert_eq!(decoded.source, src);
        assert_eq!(decoded.packet_id, 1001);
        assert_eq!(decoded.packet_type, PacketType::Echo);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_all_packet_types_round_trip() {
        let dest = Address([0x06, 0x9a, 0xe3, 0x80, 0x92]);
        let src = Address([0x12, 0x34, 0x56, 0x78, 0x9a]);
        let types = [
            PacketType::Nop,
            PacketType::Hello,
            PacketType::Error,
            PacketType::Ok,
            PacketType::Whois,
            PacketType::Rendezvous,
            PacketType::Frame,
            PacketType::ExtFrame,
            PacketType::Echo,
            PacketType::Pong,
            PacketType::MulticastLike,
            PacketType::NetworkCredentials,
            PacketType::NetworkConfigRequest,
            PacketType::NetworkConfig,
            PacketType::MulticastGather,
            PacketType::MulticastFrame,
            PacketType::PushDirectPaths,
            PacketType::UserMessage,
            PacketType::RemoteTrace,
        ];
        for pt in types {
            let pkt = Packet::new(dest, src, 1, pt, Bytes::new());
            let decoded = Packet::decode(pkt.encode()).unwrap();
            assert_eq!(decoded.packet_type, pt, "round-trip failed for {pt:?}");
        }
    }
}
