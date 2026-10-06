//! Whether a lease's holder is still working (#360).
//!
//! A lease belongs to a session, and a session belongs to the agent process
//! that started or adopted it (#542, `session_holder`). So a lease is as live
//! as that process: running means `active` (or `idle` when it has issued no
//! broker command for a while), gone means `stale`. A stale lease keeps its
//! hold for a visible grace period, so a holder that crashed mid-edit can come
//! back, and only past that grace does it stop counting as a conflict. It is
//! still listed either way.
//!
//! When the broker cannot tell -- no recorded holder, no process table, an
//! unidentified caller -- the lease is `unknown`, and unknown is never read as
//! dead: it keeps whatever hold the session's own status gives it.

use std::path::Path;

/// Default for `[leases] idle_minutes`.
pub const DEFAULT_IDLE_MINUTES: i64 = 30;
/// Default for `[leases] stale_grace_minutes`.
pub const DEFAULT_STALE_GRACE_MINUTES: i64 = 15;

/// Liveness thresholds, from `.aethyme/config.toml` `[leases]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseLivenessPolicy {
    pub idle_ms: i64,
    pub stale_grace_ms: i64,
}

impl Default for LeaseLivenessPolicy {
    fn default() -> Self {
        Self {
            idle_ms: DEFAULT_IDLE_MINUTES * 60_000,
            stale_grace_ms: DEFAULT_STALE_GRACE_MINUTES * 60_000,
        }
    }
}

impl LeaseLivenessPolicy {
    /// Read like the other `[leases]` keys: committed config on the default
    /// branch, else the main checkout's file. A missing, unparseable or
    /// negative value keeps the default.
    pub fn load(main_root: &Path) -> Self {
        crate::merge::repository_config_text(main_root)
            .map(|text| Self::from_config_text(&text))
            .unwrap_or_default()
    }

    pub fn from_config_text(text: &str) -> Self {
        let mut policy = Self::default();
        let Ok(value) = text.parse::<toml::Value>() else {
            return policy;
        };
        let minutes = |key: &str| {
            value
                .get("leases")
                .and_then(|leases| leases.get(key))
                .and_then(toml::Value::as_integer)
                .filter(|minutes| *minutes >= 0)
        };
        if let Some(idle) = minutes("idle_minutes") {
            policy.idle_ms = idle.saturating_mul(60_000);
        }
        if let Some(grace) = minutes("stale_grace_minutes") {
            policy.stale_grace_ms = grace.saturating_mul(60_000);
        }
        policy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseLiveness {
    /// The holder process is running and used the broker recently.
    Active,
    /// The holder process is running but quiet past `idle_minutes`.
    /// Informational: an idle lease holds exactly like an active one.
    Idle,
    /// The holder process is gone. Within the grace period the lease still
    /// holds; past it, it is no longer a conflict.
    Stale,
    Released,
    /// Liveness could not be established. Never treated as dead.
    Unknown,
}

impl LeaseLiveness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Stale => "stale",
            Self::Released => "released",
            Self::Unknown => "unknown",
        }
    }
}

/// What the liveness verdict rests on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseLivenessEvidence {
    /// One of `holder_running`, `holder_gone`, `released`, `no_recorded_holder`,
    /// `process_table_unavailable`.
    pub basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<i64>,
    /// Latest broker activity of the holding session, epoch ms.
    pub last_activity_at: i64,
    /// When the broker first saw the holder gone, epoch ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_since: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<i64>,
    /// Whether the lease still counts as a conflict for another session.
    pub holds: bool,
}

/// One lease with its liveness, as `broker status --json` and
/// `leases explain` report it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseLivenessView {
    pub lease_id: i64,
    pub session_id: i64,
    pub path: String,
    pub kind: crate::LeaseKind,
    pub liveness: LeaseLiveness,
    pub liveness_evidence: LeaseLivenessEvidence,
}

/// What the broker observed about a lease's holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderObservation {
    Running,
    /// Gone; `since` is when the broker first saw it gone (epoch ms).
    Gone {
        since: i64,
    },
    NoRecordedHolder,
    ProcessTableUnavailable,
}

/// Classify one lease. Pure, so every state is testable without processes.
pub fn classify(
    released: bool,
    holder: &HolderObservation,
    holder_pid: Option<i64>,
    last_activity_at: i64,
    now_ms: i64,
    policy: LeaseLivenessPolicy,
) -> (LeaseLiveness, LeaseLivenessEvidence) {
    let evidence = |basis, stale_since, grace_ends_at, holds| LeaseLivenessEvidence {
        basis,
        holder_pid,
        last_activity_at,
        stale_since,
        grace_ends_at,
        holds,
    };
    if released {
        return (
            LeaseLiveness::Released,
            evidence("released", None, None, false),
        );
    }
    match holder {
        HolderObservation::Running => {
            let quiet = now_ms.saturating_sub(last_activity_at) > policy.idle_ms;
            let liveness = if quiet {
                LeaseLiveness::Idle
            } else {
                LeaseLiveness::Active
            };
            (liveness, evidence("holder_running", None, None, true))
        }
        HolderObservation::Gone { since } => {
            let grace_ends_at = since.saturating_add(policy.stale_grace_ms);
            (
                LeaseLiveness::Stale,
                evidence(
                    "holder_gone",
                    Some(*since),
                    Some(grace_ends_at),
                    now_ms < grace_ends_at,
                ),
            )
        }
        HolderObservation::NoRecordedHolder => (
            LeaseLiveness::Unknown,
            evidence("no_recorded_holder", None, None, true),
        ),
        HolderObservation::ProcessTableUnavailable => (
            LeaseLiveness::Unknown,
            evidence("process_table_unavailable", None, None, true),
        ),
    }
}

/// What the broker can see of a session's holder right now, without writing.
/// A holder seen gone for the first time reads as gone since `now_ms`; the
/// grace clock only advances once [`record_gone_holders`] has persisted it.
pub fn observe_holder(
    store: &crate::BrokerStore,
    session_id: i64,
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
) -> Result<(HolderObservation, Option<i64>), crate::BrokerOpError> {
    let Some(holder) = crate::session_holder::recorded_holder(store, session_id)? else {
        return Ok((HolderObservation::NoRecordedHolder, None));
    };
    let Some(table) = table else {
        return Ok((HolderObservation::ProcessTableUnavailable, Some(holder.pid)));
    };
    if table.is_alive(&holder) {
        return Ok((HolderObservation::Running, Some(holder.pid)));
    }
    let since = recorded_gone_since(store, session_id, &holder)?.unwrap_or(now_ms);
    Ok((HolderObservation::Gone { since }, Some(holder.pid)))
}

/// When the broker first recorded this exact holder gone, if it has. A gone
/// event for an earlier holder, or one recorded before the current binding,
/// does not count.
fn recorded_gone_since(
    store: &crate::BrokerStore,
    session_id: i64,
    holder: &crate::session_holder::AgentProcess,
) -> Result<Option<i64>, crate::BrokerOpError> {
    let Some(gone) = store.latest_session_holder_gone_event(session_id)? else {
        return Ok(None);
    };
    let bound_id = store
        .latest_session_holder_event(session_id)?
        .map(|event| event.id)
        .unwrap_or(0);
    let same_holder = gone
        .payload_json
        .as_deref()
        .and_then(|payload| {
            serde_json::from_str::<crate::session_holder::AgentProcess>(payload).ok()
        })
        .is_some_and(|recorded| &recorded == holder);
    Ok((gone.id > bound_id && same_holder).then_some(gone.ts))
}

/// Persist the first sighting of each live session's holder being gone, so
/// the stale grace runs from that moment rather than restarting on every
/// read. Returns the sessions recorded now.
pub fn record_gone_holders(
    store: &mut crate::BrokerStore,
    session_ids: &[i64],
    table: &crate::session_holder::ProcessTable,
) -> Result<Vec<i64>, crate::BrokerOpError> {
    let mut recorded = Vec::new();
    for &session_id in session_ids {
        let Some(holder) = crate::session_holder::recorded_holder(store, session_id)? else {
            continue;
        };
        if table.is_alive(&holder) || recorded_gone_since(store, session_id, &holder)?.is_some() {
            continue;
        }
        store.append_event(
            crate::events::SESSION_HOLDER_GONE,
            Some(session_id),
            Some(&crate::events::session_holder_gone_payload(&holder)),
        )?;
        recorded.push(session_id);
    }
    Ok(recorded)
}

/// Liveness of every given lease, one holder lookup per session.
pub fn assess(
    store: &crate::BrokerStore,
    leases: &[crate::Lease],
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
    policy: LeaseLivenessPolicy,
) -> Result<Vec<LeaseLivenessView>, crate::BrokerOpError> {
    let mut holders = std::collections::HashMap::new();
    let mut views = Vec::with_capacity(leases.len());
    for lease in leases {
        let ((observation, pid), activity) = match holders.entry(lease.session_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert((
                observe_holder(store, lease.session_id, table, now_ms)?,
                store.session(lease.session_id)?.last_activity_at,
            )),
        };
        let (liveness, liveness_evidence) = classify(
            lease.released_at.is_some(),
            observation,
            *pid,
            *activity,
            now_ms,
            policy,
        );
        views.push(LeaseLivenessView {
            lease_id: lease.id,
            session_id: lease.session_id,
            path: lease.path.clone(),
            kind: lease.kind,
            liveness,
            liveness_evidence,
        });
    }
    Ok(views)
}

/// Sessions whose holder is gone past the grace: their leases no longer
/// count as conflicts.
pub fn released_by_grace(views: &[LeaseLivenessView]) -> std::collections::HashSet<i64> {
    views
        .iter()
        .filter(|view| view.liveness == LeaseLiveness::Stale && !view.liveness_evidence.holds)
        .map(|view| view.session_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;

    fn policy() -> LeaseLivenessPolicy {
        LeaseLivenessPolicy {
            idle_ms: 30 * MINUTE,
            stale_grace_ms: 15 * MINUTE,
        }
    }

    #[test]
    fn a_running_holder_is_active_then_idle_and_always_holds() {
        let now = 100 * MINUTE;
        let (fresh, evidence) = classify(
            false,
            &HolderObservation::Running,
            Some(7),
            now - MINUTE,
            now,
            policy(),
        );
        assert_eq!(fresh, LeaseLiveness::Active);
        assert!(evidence.holds);
        let (quiet, evidence) = classify(
            false,
            &HolderObservation::Running,
            Some(7),
            now - 31 * MINUTE,
            now,
            policy(),
        );
        assert_eq!(quiet, LeaseLiveness::Idle);
        assert!(evidence.holds, "idle is informational and keeps the hold");
    }

    #[test]
    fn a_gone_holder_holds_until_the_grace_ends() {
        let since = 50 * MINUTE;
        let gone = HolderObservation::Gone { since };
        let (within, evidence) = classify(false, &gone, Some(7), 0, since + MINUTE, policy());
        assert_eq!(within, LeaseLiveness::Stale);
        assert!(evidence.holds);
        assert_eq!(evidence.stale_since, Some(since));
        assert_eq!(evidence.grace_ends_at, Some(since + 15 * MINUTE));
        let (past, evidence) = classify(false, &gone, Some(7), 0, since + 15 * MINUTE, policy());
        assert_eq!(past, LeaseLiveness::Stale);
        assert!(
            !evidence.holds,
            "past the grace a stale lease no longer holds"
        );
    }

    #[test]
    fn unknown_liveness_is_never_treated_as_dead() {
        for observation in [
            HolderObservation::NoRecordedHolder,
            HolderObservation::ProcessTableUnavailable,
        ] {
            let (liveness, evidence) =
                classify(false, &observation, None, 0, i64::MAX / 2, policy());
            assert_eq!(liveness, LeaseLiveness::Unknown);
            assert!(evidence.holds, "{observation:?} must keep the hold");
        }
    }

    #[test]
    fn a_released_lease_is_released_whatever_the_holder() {
        let (liveness, evidence) =
            classify(true, &HolderObservation::Running, Some(7), 0, 0, policy());
        assert_eq!(liveness, LeaseLiveness::Released);
        assert!(!evidence.holds);
    }

    #[test]
    fn config_keys_set_the_thresholds_and_bad_values_keep_defaults() {
        let policy = LeaseLivenessPolicy::from_config_text(
            "[leases]\nidle_minutes = 5\nstale_grace_minutes = 0\n",
        );
        assert_eq!(policy.idle_ms, 5 * MINUTE);
        assert_eq!(policy.stale_grace_ms, 0);
        let fallback = LeaseLivenessPolicy::from_config_text(
            "[leases]\nidle_minutes = -1\nstale_grace_minutes = \"x\"\n",
        );
        assert_eq!(fallback, LeaseLivenessPolicy::default());
        assert_eq!(
            LeaseLivenessPolicy::from_config_text("not toml ["),
            LeaseLivenessPolicy::default()
        );
    }
}
