use std::net::SocketAddr;
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
        if data.len() < 17 {
            bail!("Planet binary truncated: must be at least 17 bytes, got {}", data.len());
        }

        let world_type = data[0];
        let id = u64::from_be_bytes(data[1..9].try_into()?);
        let timestamp = u64::from_be_bytes(data[9..17].try_into()?);

        let mut roots = Vec::new();
        if data.len() >= 22 {
            let mut root_addr = [0u8; 5];
            root_addr.copy_from_slice(&data[17..22]);
            roots.push(WorldRoot {
                identity: Address(root_addr),
                stable_endpoints: vec!["dz.dreamzone.cc:9993".to_string()],
            });
        }

        Ok(World {
            world_type,
            id,
            timestamp,
            roots,
            signature: Vec::new(),
        })
    }

    /// In-place dynamic update of root stable endpoints (matching C++ World::setRootStableEndpoints).
    /// Used by the in-memory DNS resolver to dynamically refresh endpoints without daemon restart.
    pub fn set_root_stable_endpoints(&mut self, root_index: usize, endpoints: Vec<String>) {
        if root_index < self.roots.len() {
            self.roots[root_index].stable_endpoints = endpoints;
        }
    }

    /// Save World to binary format.
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let mut data = Vec::new();
        data.push(self.world_type);
        data.extend_from_slice(&self.id.to_be_bytes());
        data.extend_from_slice(&self.timestamp.to_be_bytes());
        for root in &self.roots {
            data.extend_from_slice(root.identity.as_bytes());
        }
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(path.as_ref(), data).await?;
        Ok(())
    }
}
