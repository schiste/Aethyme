//! Named ownership claims: which session is driving a repository-wide operation.
//!
//! A lease names a path. A release, a merge chain or a migration has none: it
//! is orchestration, and two sessions driving one release race each other's
//! merges and tags. Observed on 2026-10-02/03: an agent hit its usage limit
//! half-way through v0.8.15, a second session took the release over, and when
//! the first came back the only way to learn who was driving, and whether they
//! still were, was to read the coordinated-operation journal by hand.
//!
//! A claim makes that one `status` line. It informs, and like leases since
//! #489 it refuses exactly one case: the name is held by another session that
//! is working right now -- live activity, or a coordinated operation, inside
//! the idle window. Anything quieter is a takeover that succeeds and says whose
//! claim it replaced and how long that session had been silent.
//!
//! Claims of finished sessions stop counting when the session closes: every
//! read joins live sessions, the rule leases and scopes already follow.

use crate::{AgentView, Broker, BrokerOpError, SessionStatus, StatusAdvice, StatusAdviceSeverity};

/// Longest claim name accepted. Names are labels such as `release v0.8.15`,
/// shown on one status line; anything longer is a sentence, which belongs in
/// the claim's purpose.
pub const MAX_OWNERSHIP_CLAIM_NAME_CHARS: usize = 80;

/// One open claim as stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OwnershipClaim {
    pub id: i64,
    /// What is being driven, e.g. `release v0.8.15`.
    pub name: String,
    pub session_id: i64,
    /// Why, in the claimant's words (the `--reason` it claimed with).
    pub purpose: String,
    pub claimed_at: i64,
    /// The session whose claim this one replaced, when it was a takeover.
    pub taken_over_from: Option<i64>,
}

/// A claim joined to what its holder is doing now.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OwnershipClaimView {
    #[serde(flatten)]
    pub claim: OwnershipClaim,
    /// The holder's derived liveness, as `agents` reports it.
    pub holder_status: SessionStatus,
    pub holder_agent: Option<String>,
    pub holder_short_name: Option<String>,
    /// Latest of the holder's session activity and its last coordinated
    /// operation: a release driver's last act is often a merge or a tag.
    pub last_active_at: i64,
    pub last_operation_at: Option<i64>,
    pub last_operation_reason: Option<String>,
    /// Whether the holder counts as working, the one state that refuses a
    /// competing claim.
    pub working: bool,
}

/// What `ownership claim` did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OwnershipClaimReport {
    pub name: String,
    pub session_id: i64,
    /// The session already held this name; only its purpose was updated.
    pub already_held: bool,
    /// The claim this one took over, as it stood just before.
    pub replaced: Option<OwnershipClaimView>,
}

/// Trimmed name, or why it is not one.
pub(crate) fn normalize_claim_name(name: &str) -> Result<String, BrokerOpError> {
    let name = name.trim();
    let reason = if name.is_empty() {
        Some("a claim name cannot be empty")
    } else if name.chars().count() > MAX_OWNERSHIP_CLAIM_NAME_CHARS {
        Some("a claim name is a short label (at most 80 characters); put the detail in --reason")
    } else if name.chars().any(char::is_control) {
        Some("a claim name cannot contain control characters")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(BrokerOpError::InvalidOwnershipClaim {
            name: name.to_string(),
            reason: reason.to_string(),
        }),
        None => Ok(name.to_string()),
    }
}

/// `3h05m ago`-style age for status lines; claims live for hours, so the
/// seconds a submit label carries would only be noise.
pub(crate) fn age_label(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    match minutes {
        0 => "under a minute".to_string(),
        1..=59 => format!("{minutes}m"),
        _ => format!("{}h{:02}m", minutes / 60, minutes % 60),
    }
}

impl Broker {
    /// Claim `name` for `session_id`; see the module docs for the rule.
    pub fn claim_ownership(
        &mut self,
        session_id: i64,
        name: &str,
        purpose: &str,
    ) -> Result<OwnershipClaimReport, BrokerOpError> {
        let name = normalize_claim_name(name)?;
        let purpose = purpose.trim();
        if purpose.is_empty() {
            return Err(BrokerOpError::InvalidOwnershipClaim {
                name,
                reason: "--reason must say what the claim is for".into(),
            });
        }
        let session = self.store_ref().session(session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        let now = crate::clock::epoch_ms();
        let agents = self.agents(now)?;
        let current = self
            .ownership_claim_views(&agents, now)?
            .into_iter()
            .find(|view| view.claim.name == name);
        let replaced = match current {
            Some(view) if view.claim.session_id == session_id => {
                self.store()
                    .take_ownership_claim(&name, session_id, purpose, None, now)?;
                return Ok(OwnershipClaimReport {
                    name,
                    session_id,
                    already_held: true,
                    replaced: None,
                });
            }
            Some(view) if view.working => {
                return Err(BrokerOpError::OwnershipClaimHeld {
                    name,
                    session_id,
                    holder: Box::new(view),
                });
            }
            other => other,
        };
        let taken = self.store().take_ownership_claim(
            &name,
            session_id,
            purpose,
            replaced.as_ref().map(|view| view.claim.session_id),
            now,
        )?;
        if !taken {
            // Someone claimed the name between the read above and the write.
            let agents = self.agents(now)?;
            let holder = self
                .ownership_claim_views(&agents, now)?
                .into_iter()
                .find(|view| view.claim.name == name);
            return match holder {
                Some(holder) => Err(BrokerOpError::OwnershipClaimHeld {
                    name,
                    session_id,
                    holder: Box::new(holder),
                }),
                None => Err(BrokerOpError::InvalidOwnershipClaim {
                    name,
                    reason: "the claim changed hands while it was being taken; retry".into(),
                }),
            };
        }
        Ok(OwnershipClaimReport {
            name,
            session_id,
            already_held: false,
            replaced,
        })
    }

    /// Release `session_id`'s claim on `name`.
    pub fn release_ownership(&mut self, session_id: i64, name: &str) -> Result<(), BrokerOpError> {
        let name = normalize_claim_name(name)?;
        self.store().session(session_id)?;
        if self
            .store()
            .release_ownership_claim(&name, session_id, crate::clock::epoch_ms())?
        {
            Ok(())
        } else {
            Err(BrokerOpError::InvalidOwnershipClaim {
                name,
                reason: format!("session {session_id} holds no claim by that name"),
            })
        }
    }

    /// Every open claim, with its holder's liveness, against `agents`.
    pub fn ownership_claims(&mut self) -> Result<Vec<OwnershipClaimView>, BrokerOpError> {
        let now = crate::clock::epoch_ms();
        let agents = self.agents(now)?;
        self.ownership_claim_views(&agents, now)
    }

    pub(crate) fn ownership_claim_views(
        &self,
        agents: &[AgentView],
        now_ms: i64,
    ) -> Result<Vec<OwnershipClaimView>, BrokerOpError> {
        let mut views = Vec::new();
        for claim in self.store_ref().active_ownership_claims()? {
            let agent = agents
                .iter()
                .find(|agent| agent.session.id == claim.session_id);
            let session = match agent {
                Some(agent) => agent.session.clone(),
                None => self.store_ref().session(claim.session_id)?,
            };
            let holder_status = agent.map_or(session.status, |agent| agent.derived_status);
            let activity_at = agent.map_or(session.last_activity_at, |agent| agent.activity_at);
            let (last_operation_at, last_operation_reason) = self
                .store_ref()
                .latest_coordinated_operation(claim.session_id)?
                .map_or((None, None), |(at, reason)| (Some(at), reason));
            let last_active_at = last_operation_at.unwrap_or(0).max(activity_at);
            let working = holder_status == SessionStatus::Active
                || last_operation_at
                    .is_some_and(|at| now_ms.saturating_sub(at) <= crate::broker::IDLE_AFTER_MS);
            views.push(OwnershipClaimView {
                claim,
                holder_status,
                holder_agent: session.agent_identity,
                holder_short_name: session.short_name,
                last_active_at,
                last_operation_at,
                last_operation_reason,
                working,
            });
        }
        Ok(views)
    }
}

/// One status line per open claim: who drives what, and whether they still
/// are. Informational -- the claim itself is the coordination.
pub(crate) fn ownership_claim_advice(
    claims: &[OwnershipClaimView],
    now_ms: i64,
) -> Vec<StatusAdvice> {
    claims
        .iter()
        .map(|view| {
            let holder = describe_holder(view);
            let quiet = age_label(now_ms.saturating_sub(view.last_active_at));
            let mut evidence = vec![format!("purpose: {}", view.claim.purpose)];
            if let Some(reason) = view.last_operation_reason.as_deref() {
                let at = view.last_operation_at.unwrap_or(view.last_active_at);
                evidence.push(format!(
                    "last coordinated operation {} ago: {reason}",
                    age_label(now_ms.saturating_sub(at))
                ));
            }
            if let Some(previous) = view.claim.taken_over_from {
                evidence.push(format!("taken over from session {previous}"));
            }
            StatusAdvice {
                id: "ownership.claimed",
                severity: StatusAdviceSeverity::Info,
                reason: "a session declared it is driving this operation",
                summary: if view.working {
                    format!(
                        "\"{}\" is being driven by {holder} (working, last active {quiet} ago); \
                         coordinate with it before acting on the same operation",
                        view.claim.name
                    )
                } else {
                    format!(
                        "\"{}\" is held by {holder}, {} and quiet for {quiet}; a claim from \
                         another session takes it over",
                        view.claim.name,
                        view.holder_status.as_str()
                    )
                },
                session_id: Some(view.claim.session_id),
                queue_entry_id: None,
                evidence,
                commands: vec![if view.working {
                    format!(
                        "aethyme broker advanced note send --session <id> --to-session {} \
                         --message \"…\"",
                        view.claim.session_id
                    )
                } else {
                    format!(
                        "aethyme broker advanced ownership claim {} --session <id> --reason \"…\"",
                        shell_word(&view.claim.name)
                    )
                }],
            }
        })
        .collect()
}

/// `session 12 [Codex <x@y>] (release-cut)`, as much as is known.
pub(crate) fn describe_holder(view: &OwnershipClaimView) -> String {
    let mut text = format!("session {}", view.claim.session_id);
    if let Some(agent) = view.holder_agent.as_deref() {
        text.push_str(&format!(" [{agent}]"));
    }
    if let Some(short) = view.holder_short_name.as_deref() {
        text.push_str(&format!(" ({short})"));
    }
    text
}

fn shell_word(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_short_trimmed_labels() {
        assert_eq!(
            normalize_claim_name("  release v0.8.15 ").unwrap(),
            "release v0.8.15"
        );
        assert!(normalize_claim_name("   ").is_err());
        assert!(normalize_claim_name(&"x".repeat(81)).is_err());
        assert!(normalize_claim_name("a\nb").is_err());
    }

    #[test]
    fn ages_read_in_hours_and_minutes() {
        assert_eq!(age_label(30_000), "under a minute");
        assert_eq!(age_label(5 * 60_000), "5m");
        assert_eq!(age_label(7 * 3_600_000 + 5 * 60_000), "7h05m");
    }

    #[test]
    fn takeover_commands_quote_names_with_spaces() {
        assert_eq!(shell_word("release-v1"), "release-v1");
        assert_eq!(shell_word("release v1"), "'release v1'");
    }
}
