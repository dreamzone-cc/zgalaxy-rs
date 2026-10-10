//! Mesh transport abstraction — the seam where GTP plugs in.
//!
//! ```text
//!   QuicTransport ──┐                    ┌── Frame ───► TAP
//!                   ├─► TransportEvent ──┤
//!   (GtpTransport)──┘                    └── Control ─► ControlEngine
//! ```
//!
//! - [`MeshTransport`]: what the engine needs from any backend.
//! - [`TransportEvent`]: what every backend delivers.
//! - [`TransportCapabilities`]: per-backend limits (MTU, reliability).
//! - [`PeerSessions`]: identity binding shared by all backends.
//!
//! `src/transport.rs` is a different thing — the legacy plaintext UDP wire
//! bridge — and is intentionally left where it is.

pub mod capabilities;
pub mod events;
pub mod session;
pub mod traits;

pub use capabilities::{PathMode, TransportCapabilities, TransportKind};
pub use events::{SessionGeneration, TransportEvent};
pub use session::PeerSessions;
pub use traits::MeshTransport;
