//! End-to-end mesh engine test over GTP — the same scenario as the QUIC one
//! (`common/mod.rs`), proving the engine is transport-independent.

#![cfg(feature = "gtp")]

mod common;

use zgalaxy_rs::mesh::TransportKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_authorize_assign_and_forward_over_gtp() {
    common::full_cycle(TransportKind::Gtp).await;
}
