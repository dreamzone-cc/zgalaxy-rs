use std::path::Path;
use tokio::fs;
use crate::identity::Address;
use anyhow::{bail, Context, Result};
use serde::{Serialize, Deserialize};

pub const WORLD_TYPE_PLANET: u8 = 1;
pub const WORLD_TYPE_MOON: u8 = 127;

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
                        if cursor + 20 <= data.len() {
                            // InetAddress: sockaddr_storage binary format
                            let fam = u16::from_be_bytes(data[cursor..cursor + 2].try_into()?);
                            let port = u16::from_be_bytes(data[cursor + 2..cursor + 4].try_into()?);
                            let ip = if fam == 2 || fam == 0 { // IPv4
                                format!("{}.{}.{}.{}", data[cursor + 4], data[cursor + 5], data[cursor + 6], data[cursor + 7])
                            } else { // IPv6
                                format!("[{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}]",
                                    data[cursor+4], data[cursor+5], data[cursor+6], data[cursor+7],
                                    data[cursor+8], data[cursor+9], data[cursor+10], data[cursor+11],
                                    data[cursor+12], data[cursor+13], data[cursor+14], data[cursor+15],
                                    data[cursor+16], data[cursor+17], data[cursor+18], data[cursor+19])
                            };
                            stable_endpoints.push(format!("{}/{}", ip, port));
                            cursor += 20;
                        }
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
}
