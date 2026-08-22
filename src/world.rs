use std::path::Path;
use tokio::fs;
use crate::identity::Address;
use anyhow::{bail, Context, Result};
use serde::{Serialize, Deserialize};

pub const WORLD_TYPE_PLANET: u8 = 1;
pub const WORLD_TYPE_MOON: u8 = 127;

/// Canonical ZeroTier "Earth" planet id (World.hpp: ZT_WORLD_ID_EARTH).
pub const WORLD_ID_EARTH: u64 = 149604618;

/// Canonical ZeroTier C++ sizes (ECC.hpp):
/// public key set = 64 bytes (two 32-byte halves), signature = 96 bytes
/// (64-byte Ed25519 signature + 32-byte signer public key).
pub const CANONICAL_KEY_SET_LEN: usize = 64;
pub const CANONICAL_SIGNATURE_LEN: usize = 96;

/// A root node within a Planet or Moon world definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldRoot {
    pub identity: Address,
    pub stable_endpoints: Vec<String>,
}

/// Represents a ZeroTier Planet (World 0) or Moon world topology definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct World {
    pub world_type: u8,
    pub id: u64,
    pub timestamp: u64,
    pub roots: Vec<WorldRoot>,
    pub signature: Vec<u8>,
}

pub type Planet = World;
pub type Moon = World;

impl World {
    /// Create a new World definition with roots.
    pub fn new(world_type: u8, id: u64, timestamp: u64, roots: Vec<WorldRoot>) -> Self {
        World {
            world_type,
            id,
            timestamp,
            roots,
            signature: Vec::new(),
        }
    }

    /// Load and parse a binary Planet (`planet` / `world.bin`) from disk.
    pub async fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let data = fs::read(path.as_ref())
            .await
            .with_context(|| format!("Failed to read planet file at {:?}", path.as_ref()))?;

        Self::parse_binary(&data)
    }

    /// Parse binary Planet or Moon bytes (supporting both Canonical ZeroTier C++ and ZGALAXY formats).
    pub fn parse_binary(data: &[u8]) -> Result<Self> {
        if data.len() < 18 {
            bail!("Planet binary truncated: must be at least 18 bytes, got {}", data.len());
        }

        let world_type = data[0];
        let id = u64::from_be_bytes(data[1..9].try_into()?);
        let timestamp = u64::from_be_bytes(data[9..17].try_into()?);

        // Check if this is canonical ZeroTier C++ binary format (64B updatesMustBeSignedBy + 96B signature)
        // Offset 17..81 = 64B pubkey, 81..177 = 96B sig, 177 = numRoots (1B)
        let is_canonical_zt = data.len() >= 178 && data[177] <= 16;

        let (root_count, mut cursor, signature) = if is_canonical_zt {
            let sig = data[81..177].to_vec();
            let num_roots = data[177] as usize;
            (num_roots, 178, sig)
        } else {
            let count = if data.len() >= 19 {
                u16::from_be_bytes(data[17..19].try_into()?) as usize
            } else {
                data[17] as usize
            };
            let start_cursor = if data.len() >= 19 { 19 } else { 18 };
            (count, start_cursor, Vec::new())
        };

        let mut roots = Vec::new();
        for _ in 0..root_count {
            if cursor + 5 > data.len() {
                break;
            }
            let mut root_addr = [0u8; 5];
            root_addr.copy_from_slice(&data[cursor..cursor + 5]);
            cursor += 5;

            // In canonical format, identity includes 1B type + 64B public keys
            if is_canonical_zt && cursor + 65 <= data.len() {
                cursor += 65; // Skip identity type + Ed25519/C25519 public keys
            }

            let mut stable_endpoints = Vec::new();
            if is_canonical_zt {
                if cursor < data.len() {
                    let num_eps = data[cursor] as usize;
                    cursor += 1;
                    for _ in 0..num_eps {
                        // InetAddress::serialize (InetAddress.hpp): 1-byte
                        // family (0x04 IPv4 / 0x06 IPv6), address bytes,
                        // then a big-endian uint16 port.
                        if cursor >= data.len() {
                            break;
                        }
                        let fam = data[cursor];
                        cursor += 1;
                        let (ip_len, ip): (usize, Option<String>) = match fam {
                            0x04 if cursor + 6 <= data.len() => (
                                4,
                                Some(format!(
                                    "{}.{}.{}.{}",
                                    data[cursor],
                                    data[cursor + 1],
                                    data[cursor + 2],
                                    data[cursor + 3]
                                )),
                            ),
                            0x06 if cursor + 18 <= data.len() => {
                                let mut s = String::from("[");
                                for i in 0..16 {
                                    if i > 0 && i % 2 == 0 {
                                        s.push(':');
                                    }
                                    s.push_str(&format!("{:02x}", data[cursor + i]));
                                }
                                s.push(']');
                                (16, Some(s))
                            }
                            _ => (0, None),
                        };
                        if ip.is_none() {
                            break;
                        }
                        cursor += ip_len;
                        if cursor + 2 > data.len() {
                            break;
                        }
                        let port = u16::from_be_bytes(data[cursor..cursor + 2].try_into()?);
                        cursor += 2;
                        stable_endpoints.push(format!("{}/{}", ip.unwrap(), port));
                    }
                }
            } else {
                if cursor + 2 <= data.len() {
                    let ep_count = u16::from_be_bytes(data[cursor..cursor + 2].try_into()?) as usize;
                    cursor += 2;
                    for _ in 0..ep_count {
                        if cursor + 1 > data.len() {
                            break;
                        }
                        let ep_len = data[cursor] as usize;
                        cursor += 1;
                        if cursor + ep_len > data.len() {
                            break;
                        }
                        if ep_len > 0 {
                            stable_endpoints.push(String::from_utf8_lossy(&data[cursor..cursor + ep_len]).into_owned());
                        }
                        cursor += ep_len;
                    }
                }
            }

            roots.push(WorldRoot {
                identity: Address(root_addr),
                stable_endpoints,
            });
        }

        let mut final_signature = signature;
        if !is_canonical_zt && cursor + 4 <= data.len() {
            let sig_len = u32::from_be_bytes(data[cursor..cursor + 4].try_into()?) as usize;
            cursor += 4;
            if sig_len > 0 && cursor + sig_len <= data.len() {
                final_signature = data[cursor..cursor + sig_len].to_vec();
            }
        }

        Ok(World {
            world_type,
            id,
            timestamp,
            roots,
            signature: final_signature,
        })
    }

    /// In-place dynamic update of root stable endpoints (matching C++ World::setRootStableEndpoints).
    /// Used by the in-memory DNS resolver to dynamically refresh endpoints without daemon restart.
    pub fn set_root_stable_endpoints(&mut self, root_index: usize, endpoints: Vec<String>) {
        if root_index < self.roots.len() {
            self.roots[root_index].stable_endpoints = endpoints;
        }
    }

    /// Serialize World to its canonical binary representation (world type, id,
    /// timestamp, root count, root identities + stable endpoints, and optional
    /// signature).
    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(self.world_type);
        data.extend_from_slice(&self.id.to_be_bytes());
        data.extend_from_slice(&self.timestamp.to_be_bytes());
        data.extend_from_slice(&(self.roots.len() as u16).to_be_bytes());
        for root in &self.roots {
            data.extend_from_slice(root.identity.as_bytes());
            data.extend_from_slice(&(root.stable_endpoints.len() as u16).to_be_bytes());
            for ep in &root.stable_endpoints {
                let ep_bytes = ep.as_bytes();
                data.push(ep_bytes.len() as u8);
                data.extend_from_slice(ep_bytes);
            }
        }
        data.extend_from_slice(&(self.signature.len() as u32).to_be_bytes());
        data.extend_from_slice(&self.signature);
        data
    }

    /// Save World to binary format.
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(path.as_ref(), self.encode()).await?;
        Ok(())
    }

    /// Canonical root identity public key set for a root: the Ed25519 public
    /// key in the first 32 bytes (second half unused, zeroed — the C++
    /// ECC key set pairs an Ed25519 and a Curve25519 key).
    fn canonical_key_set(ed25519_pub: Option<&[u8]>) -> [u8; CANONICAL_KEY_SET_LEN] {
        let mut out = [0u8; CANONICAL_KEY_SET_LEN];
        if let Some(pub_bytes) = ed25519_pub {
            if pub_bytes.len() == 32 {
                out[..32].copy_from_slice(pub_bytes);
            }
        }
        out
    }

    /// Canonical ZeroTier C++ root serialization: address (5 bytes),
    /// identity type byte (0), 64-byte public key set (World.hpp
    /// Identity::serialize), endpoint count, then InetAddress entries.
    fn encode_canonical_roots(roots: &[WorldRoot], root_keys: &[Option<[u8; 32]>]) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(roots.len() as u8);
        for (i, root) in roots.iter().enumerate() {
            let key: Option<[u8; 32]> = root_keys.get(i).and_then(|k| *k);
            let endpoints: Vec<Vec<u8>> = root
                .stable_endpoints
                .iter()
                .filter_map(|ep| encode_canonical_endpoint(ep))
                .collect();
            data.extend_from_slice(root.identity.as_bytes());
            data.push(0u8); // identity type (C25519/Ed25519)
            data.extend_from_slice(&Self::canonical_key_set(key.as_ref().map(|k| k.as_slice())));
            data.push(endpoints.len() as u8);
            for ep in endpoints {
                data.extend_from_slice(&ep);
            }
        }
        data
    }

    /// Serialize to the canonical ZeroTier C++ binary world format
    /// (World::serialize with signature), signed by `signer`.
    ///
    /// Signed payload follows World::serialize(forSign=true):
    /// 0x7f*8 prefix + body + 0xf7*8 suffix, where the body is
    /// type + id + timestamp + updatesMustBeSignedBy(64) + roots + u16(0).
    /// The signature is the C++ ECC composite: 64-byte Ed25519 signature
    /// followed by the 32-byte signer public key.
    pub fn encode_canonical(&self, signer: &crate::identity::Identity, root_keys: &[Option<[u8; 32]>]) -> Result<Vec<u8>> {
        let signer_pub: [u8; 32] = signer.verifying_key.to_bytes();
        let updates_must_be_signed_by = Self::canonical_key_set(Some(signer_pub.as_slice()));

        let mut body = Vec::new();
        body.push(self.world_type);
        body.extend_from_slice(&self.id.to_be_bytes());
        body.extend_from_slice(&self.timestamp.to_be_bytes());
        body.extend_from_slice(&updates_must_be_signed_by);
        body.extend_from_slice(&Self::encode_canonical_roots(&self.roots, root_keys));
        body.extend_from_slice(&0u16.to_be_bytes()); // attached dictionary length

        let mut for_sign = Vec::with_capacity(body.len() + 16);
        for_sign.extend_from_slice(&[0x7fu8; 8]);
        for_sign.extend_from_slice(&body);
        for_sign.extend_from_slice(&[0xf7u8; 8]);

        let sig_64 = signer.sign(&for_sign)?;
        let mut signature = Vec::with_capacity(CANONICAL_SIGNATURE_LEN);
        signature.extend_from_slice(&sig_64);
        signature.extend_from_slice(&signer_pub);

        let mut out = Vec::with_capacity(body.len() + CANONICAL_SIGNATURE_LEN);
        out.extend_from_slice(&body[..17 + CANONICAL_KEY_SET_LEN]);
        out.extend_from_slice(&signature);
        out.extend_from_slice(&body[17 + CANONICAL_KEY_SET_LEN..]);
        Ok(out)
    }
}

/// Encode one stable endpoint ("ip/port" or "[v6]/port") in the canonical
/// InetAddress binary form: 1-byte family + address bytes + BE uint16 port.
/// Hostname endpoints cannot be represented and yield None (canonical tools
/// drop them, matching mkmoonworld behavior).
fn encode_canonical_endpoint(ep: &str) -> Option<Vec<u8>> {
    let ep = ep.trim();
    let (host, port_str) = ep.rsplit_once('/')?;
    let port: u16 = port_str.parse().ok()?;
    let mut out = Vec::new();
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        out.push(0x04u8);
        out.extend_from_slice(&v4.octets());
    } else {
        let v6 = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(v6) = v6.parse::<std::net::Ipv6Addr>() {
            out.push(0x06u8);
            out.extend_from_slice(&v6.octets());
        } else {
            return None;
        }
    }
    out.extend_from_slice(&port.to_be_bytes());
    Some(out)
}

/// True when the endpoint is a literal IPv4/IPv6 "ip/port" pair that the
/// canonical binary world format can represent.
pub fn endpoint_is_ip(ep: &str) -> bool {
    encode_canonical_endpoint(ep).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_world_round_trip() {
        let roots = vec![
            WorldRoot {
                identity: Address([0x06, 0x9a, 0xe3, 0x80, 0x92]),
                stable_endpoints: vec!["dz.dreamzone.cc/9993".to_string(), "10.0.0.1/9994".to_string()],
            },
            WorldRoot {
                identity: Address([0x12, 0x34, 0x56, 0x78, 0x9a]),
                stable_endpoints: vec!["moon.example.com:9993".to_string()],
            },
        ];
        let mut world = World::new(WORLD_TYPE_MOON, 0x069ae38092000001, 1700000000000, roots);
        world.signature = vec![0xde, 0xad, 0xbe, 0xef];

        let encoded = world.encode();
        let decoded = World::parse_binary(&encoded).unwrap();

        assert_eq!(decoded.world_type, WORLD_TYPE_MOON);
        assert_eq!(decoded.id, world.id);
        assert_eq!(decoded.timestamp, world.timestamp);
        assert_eq!(decoded.roots.len(), 2);
        assert_eq!(decoded.roots[0].identity, world.roots[0].identity);
        assert_eq!(decoded.roots[0].stable_endpoints, world.roots[0].stable_endpoints);
        assert_eq!(decoded.roots[1].identity, world.roots[1].identity);
        assert_eq!(decoded.roots[1].stable_endpoints, world.roots[1].stable_endpoints);
        assert_eq!(decoded.signature, world.signature);
    }

    #[test]
    fn test_world_parse_truncated() {
        assert!(World::parse_binary(&[0u8; 10]).is_err());
    }

    #[test]
    fn test_canonical_zgalaxy_planet_parse() {
        let hex_bytes = "010000000008eac90a0000016ce3e23955cc9d6cf90f13d23f0c42b5c2536783fee71d653b3084cfcd50627764f994447aa56a3df5da8c37cad76fb89b4cf15556b1cfdc00a71f69de809ba6a5d048014d2f199bb4ac32ed7240bf1f7ced0f455e7bceabc284a2e068c195f491b78d42959710fbfbc453852b1a0c1512e1ca0fb92f9db91ff222f3884b66f7dee4123903bd49f75f0dc4e85343e22ae1a95995f50f353d738da2713cb8d843680e948d3a01069ae3809200f20cf76e12eac02b50978dff58d96a97397d8898fde37bc896cd3620adf7a06437463f19d63ca26a4b23e2ae4f7e11dd25b5b30d64ed088a0ca4fd933619ec6200";
        let bytes = hex::decode(hex_bytes).unwrap();
        let world = World::parse_binary(&bytes).unwrap();
        assert_eq!(world.world_type, WORLD_TYPE_PLANET);
        assert_eq!(world.id, 149604618);
        assert_eq!(world.roots.len(), 1);
        assert_eq!(world.roots[0].identity, Address([0x06, 0x9a, 0xe3, 0x80, 0x92]));
    }

    #[test]
    fn test_canonical_encode_round_trip() {
        use crate::identity::Identity;
        let signer = Identity::generate();
        let roots = vec![
            WorldRoot {
                identity: signer.address,
                stable_endpoints: vec![
                    "203.0.113.7/9993".to_string(),
                    "2001:db8::1/9994".to_string(),
                ],
            },
            WorldRoot {
                identity: Address([0x12, 0x34, 0x56, 0x78, 0x9a]),
                stable_endpoints: vec!["198.51.100.4/9993".to_string()],
            },
        ];
        let world = World::new(WORLD_TYPE_PLANET, WORLD_ID_EARTH, 1700000000000, roots.clone());
        let root_keys = vec![Some(signer.verifying_key.to_bytes()), None];

        let encoded = world.encode_canonical(&signer, &root_keys).unwrap();
        // Canonical framing: type(1) + id(8) + ts(8) + keyset(64) + sig(96)
        // + root count(1) + roots + dictionary(2).
        assert_eq!(encoded[0], WORLD_TYPE_PLANET);
        assert_eq!(u64::from_be_bytes(encoded[1..9].try_into().unwrap()), WORLD_ID_EARTH);

        let parsed = World::parse_binary(&encoded).unwrap();
        assert_eq!(parsed.world_type, WORLD_TYPE_PLANET);
        assert_eq!(parsed.id, WORLD_ID_EARTH);
        assert_eq!(parsed.timestamp, world.timestamp);
        assert_eq!(parsed.roots.len(), 2);
        assert_eq!(parsed.roots[0].identity, signer.address);
        assert_eq!(
            parsed.roots[0].stable_endpoints,
            vec![
                "203.0.113.7/9993".to_string(),
                "[2001:0db8:0000:0000:0000:0000:0000:0001]/9994".to_string()
            ]
        );
        assert_eq!(parsed.roots[1].identity, Address([0x12, 0x34, 0x56, 0x78, 0x9a]));
        assert_eq!(parsed.roots[1].stable_endpoints, vec!["198.51.100.4/9993".to_string()]);
        // Signature present with the C++ composite length (96 bytes).
        assert_eq!(parsed.signature.len(), CANONICAL_SIGNATURE_LEN);
    }

    #[test]
    fn test_canonical_endpoint_encoder() {
        assert_eq!(
            encode_canonical_endpoint("203.0.113.7/9993").unwrap(),
            [0x04, 203, 0, 113, 7, 0x27, 0x09]
        );
        // Hostnames cannot be represented canonically and are dropped,
        // matching the official mkmoonworld behavior.
        assert!(encode_canonical_endpoint("dz.dreamzone.cc/9993").is_none());
        assert!(encode_canonical_endpoint("203.0.113.7").is_none());
    }
}
