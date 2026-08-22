//! # ZGALAXY-RS Core Engine & Sovereign ZeroTier-Compatible Client
//!
//! An enterprise-grade, memory-safe, ultra-high-performance ZeroTier-compatible
//! client daemon and protocol implementation written in 100% pure Rust under the
//! GNU Affero General Public License v3.0 (AGPL-3.0).
//!
//! Features:
//! - Native Async Engine powered by Tokio
//! - Zero-Restart In-Memory Dynamic IP & DNS Resolution
//! - Curve25519, Ed25519, ChaCha20-Poly1305, and Salsa20 Cryptographic Suite
//! - High-Speed Virtual TUN/TAP Interface Management (Linux / Windows Wintun / macOS)
//! - ZeroTier Wire Protocol Compatibility with Planet/Moon Root Topologies
//! - Embedded REST Control Plane on Port 9993 compatible with ZTNET & CLI tools
//! - STUN / Rendezvous NAT Traversal & P2P Hole Punching State Machine
//! - Host OS Route & IP Provisioning Engine (Linux Netlink, Windows, macOS)

pub mod cli;
pub mod config;
pub mod controller;
pub mod controller_api;
pub mod crypto;
pub mod identity;
pub mod nat;
pub mod network;
pub mod packet;
pub mod peer;
pub mod quic;
pub mod resolver;
pub mod route_manager;
pub mod transport;
pub mod tun;
pub mod world;

pub use identity::{Address, Identity};
pub use packet::{Packet, PacketType};
pub use world::{Planet, Moon, World};
pub use resolver::DynamicDnsResolver;
pub use peer::PeerManager;
pub use network::NetworkManager;
pub use controller::EmbeddedController;
pub use config::LocalConfig;
pub use nat::NatTraversalEngine;
pub use route_manager::RouteManager;
pub use transport::UdpTransport;

pub const DEFAULT_PORT: u16 = 9993;
pub const DEFAULT_CONTROL_PORT: u16 = 9993;
pub const PROTOCOL_VERSION: u32 = 12;
pub const CLIENT_VERSION: &str = "1.3.0";
