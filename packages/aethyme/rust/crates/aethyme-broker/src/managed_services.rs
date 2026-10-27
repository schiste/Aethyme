//! Broker-created service lifecycle data. These values describe observations;
//! neither a phase nor a serialized record grants container-control authority.
//!
//! Dispatch must additionally hold the checkout lifecycle claim, match the
//! immutable spawn witness and daemon, and retain generation-bound host capacity.
use serde::{Deserialize, Serialize};

/// A durable phase is written before each operation that can outlive its caller.
/// Pending creation deliberately has no transition to absence or settlement:
/// a daemon may finish creation after its client has disappeared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedServicePhase {
    Intent,
    AdmissionPending,
    Admitted,
    CreatePending,
    Created,
    StartPending,
    Running,
    RemovalPending,
    Removed,
    Settled,
}

impl ManagedServicePhase {
    /// Topology only. The store/controller must separately verify the evidence
    /// for the transition; this method is never a permission to dispatch.
    pub fn may_follow(self, previous: Self) -> bool {
        use ManagedServicePhase::*;
        matches!(
            (previous, self),
            (Intent, AdmissionPending | Settled)
                | (AdmissionPending, Admitted | Settled)
                | (Admitted, CreatePending | Removed)
                | (CreatePending, Created)
                | (Created, StartPending | RemovalPending)
                | (StartPending, Running | RemovalPending)
                | (Running, RemovalPending)
                | (RemovalPending, Removed)
                | (Removed, Settled)
        )
    }

    pub fn retains_worktree(self) -> bool {
        self != Self::Settled
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::AdmissionPending => "admission_pending",
            Self::Admitted => "admitted",
            Self::CreatePending => "create_pending",
            Self::Created => "created",
            Self::StartPending => "start_pending",
            Self::Running => "running",
            Self::RemovalPending => "removal_pending",
            Self::Removed => "removed",
            Self::Settled => "settled",
        }
    }
}

/// Public inventory intentionally omits ownership generations, host tokens,
/// environment values and service command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedServiceView {
    pub service_id: String,
    pub session_id: i64,
    pub name: String,
    pub phase: ManagedServicePhase,
    pub container_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// An exact object identifier, never a container name or abbreviated ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullContainerId(String);

impl FullContainerId {
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_create_never_becomes_absent_or_retried() {
        let pending = ManagedServicePhase::CreatePending;
        assert!(!ManagedServicePhase::Settled.may_follow(pending));
        assert!(!ManagedServicePhase::Removed.may_follow(pending));
        assert!(!ManagedServicePhase::AdmissionPending.may_follow(pending));
        assert!(!ManagedServicePhase::CreatePending.may_follow(pending));
        assert!(!ManagedServicePhase::RemovalPending.may_follow(pending));
        assert!(ManagedServicePhase::Created.may_follow(pending));
        assert!(pending.retains_worktree());
    }

    #[test]
    fn every_external_operation_has_a_prior_durable_pending_phase() {
        use ManagedServicePhase::*;
        let successful = [
            Intent,
            AdmissionPending,
            Admitted,
            CreatePending,
            Created,
            StartPending,
            Running,
            RemovalPending,
            Removed,
            Settled,
        ];
        for phases in successful.windows(2) {
            assert!(phases[1].may_follow(phases[0]));
        }
        assert!(!Running.may_follow(Admitted));
        assert!(!Created.may_follow(Intent));
        assert!(!Settled.may_follow(Running));
        assert!(!Settled.may_follow(RemovalPending));
        assert!(
            Removed.retains_worktree(),
            "capacity still needs settlement"
        );
        assert!(!Settled.retains_worktree());
        assert!(!Intent.may_follow(Settled));
    }

    #[test]
    fn object_identity_refuses_names_abbreviations_and_cli_flags() {
        for value in [
            "postgres",
            "abcdef123456",
            "--all",
            "",
            &"A".repeat(64),
            &format!("{}\n", "a".repeat(64)),
            &"g".repeat(64),
        ] {
            assert_eq!(FullContainerId::parse(value), None, "{value:?}");
        }
        let exact = "0123456789abcdef".repeat(4);
        assert_eq!(
            FullContainerId::parse(&exact).unwrap().as_str(),
            exact.as_str()
        );
    }
}
