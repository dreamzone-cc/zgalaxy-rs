//! End-to-end mesh engine test over QUIC — scenario in `common/mod.rs`.

mod common;

use zgalaxy_rs::mesh::TransportKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_authorize_assign_and_forward_over_quic() {
    common::full_cycle(TransportKind::Quic).await;
}
