//! Pure domain state model (spec §10): no UI types, no I/O — fully testable.
//! UI-visible labels/emoji/colors derive from here so Tray, windows and
//! notifications can never disagree about what a status means.

/// Per-network state, mapped from the daemon's ZeroTier-style status string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkState {
    Connected,
    WaitingApproval,
    Connecting,
    Problem,
    Disconnected,
}

impl NetworkState {
    pub fn from_status(status: &str) -> Self {
        match status {
            "OK" => NetworkState::Connected,
            "ACCESS_DENIED" => NetworkState::WaitingApproval,
            "NOT_FOUND" | "PORT_ERROR" => NetworkState::Problem,
            "" => NetworkState::Disconnected,
            _ => NetworkState::Connecting,
        }
    }

    /// Human wording per the terminology system (spec §7): gamer-first,
    /// technical terms stay out of the basic UI.
    pub fn label(&self) -> &'static str {
        match self {
            NetworkState::Connected => "Ready to play",
            NetworkState::WaitingApproval => "Waiting for approval",
            NetworkState::Connecting => "Connecting…",
            NetworkState::Problem => "Connection problem",
            NetworkState::Disconnected => "Disconnected",
        }
    }

    /// Semantic color bucket for the UI (success/warning/error/neutral).
    pub fn kind(&self) -> &'static str {
        match self {
            NetworkState::Connected => "success",
            NetworkState::WaitingApproval | NetworkState::Connecting => "warning",
            NetworkState::Problem => "error",
            NetworkState::Disconnected => "neutral",
        }
    }

    /// Emoji carries meaning together with the text — never color alone (spec §15).
    pub fn emoji(&self) -> &'static str {
        match self {
            NetworkState::Connected => "🟢",
            NetworkState::WaitingApproval => "🟠",
            NetworkState::Connecting => "🟡",
            NetworkState::Problem => "🔴",
            NetworkState::Disconnected => "⚪",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceHealth {
    Online,
    Offline,
}

/// The single top-level state a user should understand within two seconds
/// (spec §4.1), derived centrally from service health + networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverallState {
    ConnectionProblem,
    NotConnected,
    ReadyToPlay,
    WaitingApproval,
    Connecting,
}

impl OverallState {
    pub fn derive(service: Option<ServiceHealth>, nets: &[NetworkState]) -> Self {
        match service {
            None => OverallState::ConnectionProblem,
            Some(ServiceHealth::Offline) => OverallState::ConnectionProblem,
            Some(ServiceHealth::Online) => {
                if nets.is_empty() {
                    OverallState::NotConnected
                } else if nets.contains(&NetworkState::Connected) {
                    OverallState::ReadyToPlay
                } else if nets.contains(&NetworkState::WaitingApproval) {
                    OverallState::WaitingApproval
                } else {
                    OverallState::Connecting
                }
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            OverallState::ConnectionProblem => "Connection problem",
            OverallState::NotConnected => "Not connected",
            OverallState::ReadyToPlay => "Ready to play",
            OverallState::WaitingApproval => "Waiting for approval",
            OverallState::Connecting => "Connecting…",
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            OverallState::ConnectionProblem => "error",
            OverallState::NotConnected => "neutral",
            OverallState::ReadyToPlay => "success",
            OverallState::WaitingApproval | OverallState::Connecting => "warning",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            OverallState::ConnectionProblem => "🔴",
            OverallState::NotConnected => "⚪",
            OverallState::ReadyToPlay => "🟢",
            OverallState::WaitingApproval => "🟠",
            OverallState::Connecting => "🟡",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_status_mapping_matches_spec_tables() {
        assert_eq!(NetworkState::from_status("OK"), NetworkState::Connected);
        assert_eq!(
            NetworkState::from_status("ACCESS_DENIED"),
            NetworkState::WaitingApproval
        );
        assert_eq!(
            NetworkState::from_status("REQUESTING_CONFIGURATION"),
            NetworkState::Connecting
        );
        assert_eq!(NetworkState::from_status("PORT_ERROR"), NetworkState::Problem);
        assert_eq!(NetworkState::from_status(""), NetworkState::Disconnected);

        assert_eq!(NetworkState::Connected.label(), "Ready to play");
        assert_eq!(NetworkState::WaitingApproval.label(), "Waiting for approval");
        assert_eq!(NetworkState::Connecting.label(), "Connecting…");
    }

    #[test]
    fn overall_state_priority_problem_first_then_empty_then_connected() {
        use NetworkState::*;

        assert_eq!(
            OverallState::derive(None, &[Connected]),
            OverallState::ConnectionProblem
        );
        assert_eq!(
            OverallState::derive(Some(ServiceHealth::Offline), &[Connected]),
            OverallState::ConnectionProblem
        );
        assert_eq!(
            OverallState::derive(Some(ServiceHealth::Online), &[]),
            OverallState::NotConnected
        );
        assert_eq!(
            OverallState::derive(Some(ServiceHealth::Online), &[Connected, Connecting]),
            OverallState::ReadyToPlay
        );
        assert_eq!(
            OverallState::derive(
                Some(ServiceHealth::Online),
                &[WaitingApproval, Connecting]
            ),
            OverallState::WaitingApproval
        );
        assert_eq!(
            OverallState::derive(Some(ServiceHealth::Online), &[Connecting]),
            OverallState::Connecting
        );
    }

    #[test]
    fn emoji_and_label_never_empty_for_any_state() {
        for (state, kind) in [
            (NetworkState::Connected, "success"),
            (NetworkState::WaitingApproval, "warning"),
            (NetworkState::Connecting, "warning"),
            (NetworkState::Problem, "error"),
            (NetworkState::Disconnected, "neutral"),
        ] {
            assert_eq!(state.kind(), kind);
            assert!(!state.label().is_empty());
            assert!(!state.emoji().is_empty());
        }
        for overall in [
            OverallState::ConnectionProblem,
            OverallState::NotConnected,
            OverallState::ReadyToPlay,
            OverallState::WaitingApproval,
            OverallState::Connecting,
        ] {
            assert!(!overall.label().is_empty());
            assert!(!overall.emoji().is_empty());
        }
    }
}
