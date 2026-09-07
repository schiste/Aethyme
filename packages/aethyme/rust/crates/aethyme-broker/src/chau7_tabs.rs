//! Resolving a broker session to the Chau7 tab that is running it.
//!
//! The delivery outbox is adapter-neutral: `DeliverySubscription.target` is an
//! opaque string, and nothing in broker core knows what a tab is. This module
//! is the Chau7 side of that seam, kept deliberately free of any transport --
//! it decides *which* tab, and says why, from a snapshot someone else fetched.
//!
//! The rule it enforces is that ambiguity refuses. A missed notification is
//! recoverable; a review comment injected into an unrelated agent's terminal
//! is not, and with several agents on one branch that is a live risk rather
//! than a theoretical one (#150).

use serde::{Deserialize, Serialize};

/// One tab as Chau7 reports it. Extra fields in the payload are ignored, so a
/// Chau7 upgrade that adds fields does not break resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chau7Tab {
    pub tab_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub repo_root: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub ai_provider: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub is_mcp_controlled: Option<bool>,
}

/// Whether a resolved tab can take a message right now.
///
/// A tab mid-turn is not a delivery failure -- it is a later delivery, which
/// is what the outbox's `retry` outcome exists for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chau7TabReadiness {
    /// At a prompt: deliver now.
    Ready,
    /// Mid-turn: defer and retry rather than interrupt.
    Busy,
    /// Terminal or unknown state: do not deliver.
    Unavailable,
}

impl Chau7TabReadiness {
    fn from_status(status: Option<&str>) -> Self {
        match status {
            Some("waitingForInput") | Some("idle") => Self::Ready,
            Some("running") => Self::Busy,
            _ => Self::Unavailable,
        }
    }
}

/// Why a resolution refused. Each names what a human would need to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Chau7ResolutionRefusal {
    /// The session's worktree matches no tab: nothing is running it.
    NoTabForWorktree { worktree: String },
    /// Several tabs claim the same worktree. Choosing would be a guess.
    AmbiguousWorktree {
        worktree: String,
        candidates: Vec<String>,
    },
    /// A tab holds the worktree but has moved to another branch, so it is no
    /// longer doing the work this pull request came from.
    BranchDiverged {
        tab_id: String,
        expected_branch: String,
        observed_branch: Option<String>,
    },
}

/// A tab this session's activity may be delivered to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chau7Resolution {
    pub tab_id: String,
    pub readiness: Chau7TabReadiness,
    /// True when the tab is driven by automation rather than a person.
    pub mcp_controlled: bool,
}

/// Resolve the tab running `worktree_path` on `branch`.
///
/// Worktree identity carries the decision. Broker worktrees are unique per
/// session, whereas a branch is not: several agents routinely share one branch
/// across separate checkouts, so branch-first matching would deliver to an
/// arbitrary sibling. Branch is checked afterwards, as a staleness guard --
/// a tab that has moved on is no longer the right recipient.
pub fn resolve_session_tab(
    tabs: &[Chau7Tab],
    worktree_path: &str,
    branch: &str,
) -> Result<Chau7Resolution, Chau7ResolutionRefusal> {
    let wanted = normalize_path(worktree_path);
    let matches: Vec<&Chau7Tab> = tabs
        .iter()
        .filter(|tab| {
            tab.cwd
                .as_deref()
                .map(|cwd| normalize_path(cwd) == wanted)
                .unwrap_or(false)
        })
        .collect();

    let tab = match matches.as_slice() {
        [] => {
            return Err(Chau7ResolutionRefusal::NoTabForWorktree {
                worktree: worktree_path.to_string(),
            });
        }
        [single] => *single,
        several => {
            return Err(Chau7ResolutionRefusal::AmbiguousWorktree {
                worktree: worktree_path.to_string(),
                candidates: several.iter().map(|tab| tab.tab_id.clone()).collect(),
            });
        }
    };

    if tab.git_branch.as_deref() != Some(branch) {
        return Err(Chau7ResolutionRefusal::BranchDiverged {
            tab_id: tab.tab_id.clone(),
            expected_branch: branch.to_string(),
            observed_branch: tab.git_branch.clone(),
        });
    }

    Ok(Chau7Resolution {
        tab_id: tab.tab_id.clone(),
        readiness: Chau7TabReadiness::from_status(tab.status.as_deref()),
        mcp_controlled: tab.is_mcp_controlled.unwrap_or(false),
    })
}

/// Trailing separators only. Anything cleverer -- symlink resolution, case
/// folding -- would compare paths the filesystem may not agree are the same,
/// and this comparison decides who receives someone else's review.
fn normalize_path(path: &str) -> &str {
    path.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, cwd: &str, branch: &str, status: &str) -> Chau7Tab {
        Chau7Tab {
            tab_id: id.into(),
            cwd: Some(cwd.into()),
            repo_root: Some(cwd.into()),
            git_branch: Some(branch.into()),
            ai_provider: Some("claude".into()),
            status: Some(status.into()),
            is_mcp_controlled: Some(false),
        }
    }

    #[test]
    fn the_tab_holding_the_worktree_on_the_expected_branch_resolves() {
        let tabs = vec![
            tab("tab_1", "/w/other", "agent/other", "running"),
            tab("tab_7", "/w/mine", "agent/mine", "waitingForInput"),
        ];
        let resolved = resolve_session_tab(&tabs, "/w/mine", "agent/mine").unwrap();
        assert_eq!(resolved.tab_id, "tab_7");
        assert_eq!(resolved.readiness, Chau7TabReadiness::Ready);
    }

    /// The case that makes worktree-first matching necessary: six tabs sharing
    /// one branch was observed live. Branch-first would pick an arbitrary one.
    #[test]
    fn a_shared_branch_does_not_attract_another_sessions_delivery() {
        let tabs = vec![
            tab("tab_1", "/w/a", "feat/shared", "idle"),
            tab("tab_2", "/w/b", "feat/shared", "idle"),
            tab("tab_3", "/w/c", "feat/shared", "idle"),
        ];
        let resolved = resolve_session_tab(&tabs, "/w/b", "feat/shared").unwrap();
        assert_eq!(resolved.tab_id, "tab_2");
    }

    #[test]
    fn two_tabs_on_one_worktree_refuse_rather_than_guess() {
        let tabs = vec![
            tab("tab_1", "/w/mine", "agent/mine", "idle"),
            tab("tab_2", "/w/mine", "agent/mine", "idle"),
        ];
        let refusal = resolve_session_tab(&tabs, "/w/mine", "agent/mine").unwrap_err();
        match refusal {
            Chau7ResolutionRefusal::AmbiguousWorktree { candidates, .. } => {
                assert_eq!(candidates, vec!["tab_1", "tab_2"]);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_tab_that_moved_to_another_branch_is_no_longer_the_recipient() {
        let tabs = vec![tab("tab_7", "/w/mine", "agent/something-else", "idle")];
        let refusal = resolve_session_tab(&tabs, "/w/mine", "agent/mine").unwrap_err();
        assert!(matches!(
            refusal,
            Chau7ResolutionRefusal::BranchDiverged { ref tab_id, .. } if tab_id == "tab_7"
        ));
    }

    #[test]
    fn no_tab_for_the_worktree_refuses_by_name() {
        let refusal = resolve_session_tab(&[], "/w/gone", "agent/mine").unwrap_err();
        assert_eq!(
            refusal,
            Chau7ResolutionRefusal::NoTabForWorktree {
                worktree: "/w/gone".into()
            }
        );
    }

    #[test]
    fn a_running_tab_defers_instead_of_interrupting() {
        let tabs = vec![tab("tab_7", "/w/mine", "agent/mine", "running")];
        let resolved = resolve_session_tab(&tabs, "/w/mine", "agent/mine").unwrap();
        assert_eq!(resolved.readiness, Chau7TabReadiness::Busy);
    }

    #[test]
    fn a_finished_tab_is_unavailable_rather_than_ready() {
        let tabs = vec![tab("tab_7", "/w/mine", "agent/mine", "done")];
        let resolved = resolve_session_tab(&tabs, "/w/mine", "agent/mine").unwrap();
        assert_eq!(resolved.readiness, Chau7TabReadiness::Unavailable);
    }

    #[test]
    fn trailing_separators_do_not_split_one_worktree_into_two() {
        let tabs = vec![tab("tab_7", "/w/mine/", "agent/mine", "idle")];
        assert!(resolve_session_tab(&tabs, "/w/mine", "agent/mine").is_ok());
    }

    /// Chau7 adding fields must not break resolution.
    #[test]
    fn unknown_fields_in_a_tab_payload_are_ignored() {
        let payload = r#"{"tab_id":"tab_7","cwd":"/w/mine","git_branch":"agent/mine",
                          "status":"idle","brand_new_field":42}"#;
        let parsed: Chau7Tab = serde_json::from_str(payload).unwrap();
        assert_eq!(parsed.tab_id, "tab_7");
    }
}

/// What an adapter should do with a claimed delivery.
///
/// The broker decides; the caller performs the transport. Keeping the decision
/// here means the "is this tab the right one, and can it take a message now"
/// judgement is tested without Chau7 running, and cannot drift between adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Chau7DispatchAction {
    /// Send `prompt` to `tab_id`, then complete the delivery as delivered.
    Send {
        tab_id: String,
        prompt: String,
        mcp_controlled: bool,
    },
    /// The tab is mid-turn. Leave the delivery for a later tick.
    Defer { tab_id: String, why: String },
    /// The tab cannot be identified or is gone. The delivery cannot succeed.
    Abandon { why: String },
}

/// Decide what to do with one claimed delivery for a Chau7 subscription.
///
/// `Defer` and `Abandon` are distinct on purpose: deferring keeps the delivery
/// for the next tick, whereas abandoning admits it will never land. Collapsing
/// them would either retry forever against a closed tab or discard a message
/// because an agent happened to be busy.
pub fn dispatch_action(
    tabs: &[Chau7Tab],
    worktree_path: &str,
    branch: &str,
    prompt: &str,
) -> Chau7DispatchAction {
    match resolve_session_tab(tabs, worktree_path, branch) {
        Ok(resolution) => match resolution.readiness {
            Chau7TabReadiness::Ready => Chau7DispatchAction::Send {
                tab_id: resolution.tab_id,
                prompt: prompt.to_string(),
                mcp_controlled: resolution.mcp_controlled,
            },
            Chau7TabReadiness::Busy => Chau7DispatchAction::Defer {
                tab_id: resolution.tab_id,
                why: "tab is mid-turn; delivering now would interrupt it".into(),
            },
            Chau7TabReadiness::Unavailable => Chau7DispatchAction::Abandon {
                why: format!("tab {} is not accepting input", resolution.tab_id),
            },
        },
        // A tab that is merely absent may come back -- an agent restarting
        // between ticks is ordinary -- so this defers rather than abandons.
        Err(Chau7ResolutionRefusal::NoTabForWorktree { worktree }) => Chau7DispatchAction::Defer {
            tab_id: String::new(),
            why: format!("no tab is running {worktree}"),
        },
        Err(refusal) => Chau7DispatchAction::Abandon {
            why: format!("{refusal:?}"),
        },
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    fn tab(id: &str, cwd: &str, branch: &str, status: &str) -> Chau7Tab {
        Chau7Tab {
            tab_id: id.into(),
            cwd: Some(cwd.into()),
            repo_root: Some(cwd.into()),
            git_branch: Some(branch.into()),
            ai_provider: Some("claude".into()),
            status: Some(status.into()),
            is_mcp_controlled: Some(false),
        }
    }

    #[test]
    fn a_ready_tab_receives_the_prompt_unchanged() {
        let tabs = vec![tab("tab_7", "/w/mine", "agent/mine", "waitingForInput")];
        let action = dispatch_action(&tabs, "/w/mine", "agent/mine", "review on PR 151");
        assert_eq!(
            action,
            Chau7DispatchAction::Send {
                tab_id: "tab_7".into(),
                prompt: "review on PR 151".into(),
                mcp_controlled: false,
            }
        );
    }

    #[test]
    fn a_busy_tab_defers_and_keeps_the_delivery() {
        let tabs = vec![tab("tab_7", "/w/mine", "agent/mine", "running")];
        assert!(matches!(
            dispatch_action(&tabs, "/w/mine", "agent/mine", "x"),
            Chau7DispatchAction::Defer { .. }
        ));
    }

    /// An agent restarting between ticks must not lose its review notification.
    #[test]
    fn an_absent_tab_defers_rather_than_discarding_the_message() {
        assert!(matches!(
            dispatch_action(&[], "/w/mine", "agent/mine", "x"),
            Chau7DispatchAction::Defer { .. }
        ));
    }

    #[test]
    fn ambiguity_abandons_rather_than_delivering_to_a_guess() {
        let tabs = vec![
            tab("tab_1", "/w/mine", "agent/mine", "idle"),
            tab("tab_2", "/w/mine", "agent/mine", "idle"),
        ];
        assert!(matches!(
            dispatch_action(&tabs, "/w/mine", "agent/mine", "x"),
            Chau7DispatchAction::Abandon { .. }
        ));
    }

    #[test]
    fn a_finished_tab_abandons_instead_of_retrying_forever() {
        let tabs = vec![tab("tab_7", "/w/mine", "agent/mine", "done")];
        assert!(matches!(
            dispatch_action(&tabs, "/w/mine", "agent/mine", "x"),
            Chau7DispatchAction::Abandon { .. }
        ));
    }
}
