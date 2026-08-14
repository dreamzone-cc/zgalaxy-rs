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

    /// Parse binary Planet or Moon bytes.
    pub fn parse_binary(data: &[u8]) -> Result<Self> {
        if data.len() < 19 {
            bail!("Planet binary truncated: must be at least 19 bytes, got {}", data.len());
        }

        let world_type = data[0];
        let id = u64::from_be_bytes(data[1..9].try_into()?);
        let timestamp = u64::from_be_bytes(data[9..17].try_into()?);
        let root_count = u16::from_be_bytes(data[17..19].try_into()?) as usize;

        let mut roots = Vec::new();
        let mut cursor = 19;
        for _ in 0..root_count {
            if cursor + 5 > data.len() {
                bail!("Planet binary truncated: missing root identity data");
            }
            let mut root_addr = [0u8; 5];
            root_addr.copy_from_slice(&data[cursor..cursor + 5]);
            roots.push(WorldRoot {
                identity: Address(root_addr),
                stable_endpoints: vec!["dz.dreamzone.cc:9993".to_string()],
            });
            cursor += 5;
        }

        // Optional trailing signature: 4-byte length followed by signature bytes
        let mut signature = Vec::new();
        if cursor + 4 <= data.len() {
            let sig_len = u32::from_be_bytes(data[cursor..cursor + 4].try_into()?) as usize;
            cursor += 4;
            if sig_len > 0 && cursor + sig_len <= data.len() {
                signature = data[cursor..cursor + sig_len].to_vec();
            }
        }

        Ok(World {
            world_type,
            id,
            timestamp,
            roots,
            signature,
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
    /// timestamp, root count, root identities, and optional signature).
    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(self.world_type);
        data.extend_from_slice(&self.id.to_be_bytes());
        data.extend_from_slice(&self.timestamp.to_be_bytes());
        data.extend_from_slice(&(self.roots.len() as u16).to_be_bytes());
        for root in &self.roots {
            data.extend_from_slice(root.identity.as_bytes());
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
                stable_endpoints: vec!["dz.dreamzone.cc:9993".to_string()],
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
        assert_eq!(decoded.roots[1].identity, world.roots[1].identity);
        assert_eq!(decoded.signature, world.signature);
    }

    #[test]
    fn test_world_parse_truncated() {
        assert!(World::parse_binary(&[0u8; 10]).is_err());
    }
}
