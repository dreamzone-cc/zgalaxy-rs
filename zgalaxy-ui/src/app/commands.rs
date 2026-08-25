//! Core commands (spec §11.1): the only place that talks to the daemon on
//! behalf of UI actions. Every command is fire-and-forget on a worker
//! thread — the UI thread never blocks on network I/O (spec §21.3), and
//! join/leave are idempotent server-side.

use crate::network::client::DaemonClient;
use std::sync::Arc;

/// Same rule the daemon enforces on POST /network/{nwid}: 16 hex chars.
/// Validating here lets the Join dialog give instant feedback instead of a
/// silent server-side rejection.
pub fn is_valid_nwid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn join(client: Arc<DaemonClient>, nwid: String) {
    std::thread::spawn(move || {
        client.post_network(&nwid.to_lowercase());
    });
}

pub fn leave(client: Arc<DaemonClient>, nwid: String) {
    std::thread::spawn(move || {
        client.delete_network(&nwid);
    });
}

pub fn update_flags(client: Arc<DaemonClient>, nwid: String, body: serde_json::Value) {
    std::thread::spawn(move || {
        client.post_network_settings(&nwid, &body);
    });
}

#[cfg(test)]
mod tests {
    use super::is_valid_nwid;

    #[test]
    fn nwid_validation_matches_daemon_contract() {
        assert!(is_valid_nwid("74b4ea0e6d000001"));
        assert!(is_valid_nwid(" 74B4EA0E6D000001 "));
        assert!(!is_valid_nwid("1234"), "too short");
        assert!(!is_valid_nwid("zzzzzzzzzzzzzzzz"), "not hex");
        assert!(!is_valid_nwid("74b4ea0e6d000001x"), "too long");
        assert!(!is_valid_nwid(""));
    }
}
