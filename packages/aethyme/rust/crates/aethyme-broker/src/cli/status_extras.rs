//! What `broker status` adds around its two documents: the `form` marker, the
//! advice-evidence cap shared by both forms, and the `--session` block.

use serde_json::{Value, json};

/// Largest serialized `evidence` one advice entry may carry before it is cut
/// to a count, its first items and the command that prints the rest.
pub(super) const EVIDENCE_CAP_BYTES: usize = 8 * 1024;

/// Budget for the items kept from a truncated `evidence` list.
const KEPT_EVIDENCE_BYTES: usize = 4 * 1024;

/// Most items kept from a truncated `evidence` list.
const KEPT_EVIDENCE_ITEMS: usize = 20;

/// Longest single evidence item kept from a truncated list.
const KEPT_ITEM_BYTES: usize = 1024;

/// Unset (or anything but an off value), the cap applies.
const EVIDENCE_CAP_ENV: &str = "AETHYME_STATUS_EVIDENCE_CAP";

/// The command that prints every evidence item the cap left out.
pub(super) const UNCAPPED_EVIDENCE_COMMAND: &str =
    "AETHYME_STATUS_EVIDENCE_CAP=off aethyme broker status --full --json";

/// The one-line pointer compact JSON carries to the full document.
pub(super) const COMPACT_HINT: &str = "Compact form (the default). Run `aethyme broker status --full --json` only for exact session or lease-path evidence.";

fn evidence_cap_enabled() -> bool {
    std::env::var_os(EVIDENCE_CAP_ENV).is_none_or(|value| {
        let value = value.to_string_lossy().to_ascii_lowercase();
        !matches!(value.as_str(), "off" | "0" | "false" | "no")
    })
}

/// Cap the evidence of every advice entry in `document["advice"]` (the full
/// and `--summary` documents) and in `document["session"]["advice"]`.
/// Returns how many entries were truncated.
pub(crate) fn cap_document_advice_evidence(document: &mut Value) -> usize {
    if !evidence_cap_enabled() {
        return 0;
    }
    let mut truncated = 0;
    if let Some(advice) = document.get_mut("advice").and_then(Value::as_array_mut) {
        truncated += cap_advice_list(advice);
    }
    if let Some(advice) = document
        .get_mut("session")
        .and_then(|session| session.get_mut("advice"))
        .and_then(Value::as_array_mut)
    {
        truncated += cap_advice_list(advice);
    }
    truncated
}

fn cap_advice_list(advice: &mut [Value]) -> usize {
    advice
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .map(|entry| usize::from(cap_entry(entry)))
        .sum()
}

/// Truncate one entry's `evidence` in place when it is over the cap. The
/// field keeps its name and type (an array of strings); the sibling
/// `evidence_truncation` says how many there were and how to see them all.
fn cap_entry(entry: &mut serde_json::Map<String, Value>) -> bool {
    let Some(evidence) = entry.get("evidence").and_then(Value::as_array) else {
        return false;
    };
    let bytes = serde_json::to_vec(evidence).map_or(0, |encoded| encoded.len());
    if bytes <= EVIDENCE_CAP_BYTES {
        return false;
    }
    let count = evidence.len();
    let mut kept = Vec::new();
    let mut used = 0;
    for item in evidence {
        if kept.len() == KEPT_EVIDENCE_ITEMS {
            break;
        }
        let item = match item.as_str() {
            Some(text) => Value::String(truncate_text(text, KEPT_ITEM_BYTES)),
            None => item.clone(),
        };
        let size = serde_json::to_vec(&item).map_or(0, |encoded| encoded.len()) + 1;
        if !kept.is_empty() && used + size > KEPT_EVIDENCE_BYTES {
            break;
        }
        used += size;
        kept.push(item);
    }
    let shown = kept.len();
    entry.insert("evidence".into(), Value::Array(kept));
    entry.insert(
        "evidence_truncation".into(),
        json!({
            "truncated": true,
            "count": count,
            "shown": shown,
            "omitted": count - shown,
            "evidence_bytes": bytes,
            "cap_bytes": EVIDENCE_CAP_BYTES,
            "command": UNCAPPED_EVIDENCE_COMMAND,
        }),
    );
    true
}

/// `text` cut to at most `limit` bytes on a character boundary, marked when cut.
fn truncate_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... ({} bytes)", &text[..end], text.len())
}

/// The `session` block `status --session <id> --json` adds: everything an
/// agent polls for about its own session, from local state only. Nothing here
/// reaches the network: the pull request comes from the broker's own watch
/// records, and mergeability and required checks, which it does not record,
/// are reported unknown rather than fetched.
pub(crate) fn session_block(
    broker: &crate::Broker,
    session_id: i64,
    session_advice: &[crate::StatusAdvice],
) -> Value {
    let session = match broker.store_ref().session(session_id) {
        Ok(session) => session,
        Err(error) => {
            return json!({
                "id": session_id,
                "error": format!("session record could not be read: {error}"),
                "pr": Value::Null,
                "pr_state": "unknown",
                "next_action": Value::Null,
            });
        }
    };
    let push = broker.session_push_state(session_id);
    let (head_oid, remote_oid, unpushed_commits, push_state_error) = match &push {
        Ok(state) => (
            json!(state.head_oid),
            json!(state.remote_oid),
            json!(state.unpushed_commits),
            Value::Null,
        ),
        Err(error) => (
            Value::Null,
            Value::Null,
            Value::Null,
            json!(error.to_string()),
        ),
    };
    let watch = broker
        .store_ref()
        .session_pull_request_watch(session_id)
        .ok()
        .flatten();
    let (pr, pr_state) = match &watch {
        Some(watch) => (
            json!({
                "number": watch.pr_number,
                "repository": watch.display_repository,
                "draft": watch.is_draft,
                "head_sha": watch.head_sha,
                "head_matches_local": push
                    .as_ref()
                    .ok()
                    .map(|state| state.head_oid == watch.head_sha),
                "mergeable": Value::Null,
                "merge_state": Value::Null,
                "required_checks": Value::Null,
                "unknown_fields": ["mergeable", "merge_state", "required_checks"],
                "watch_id": watch.id,
                "watch_status": watch.status.as_str(),
                "observed_at_ms": watch.last_polled_at,
                "source": "pr_watch",
            }),
            "recorded",
        ),
        None => (Value::Null, "unknown"),
    };
    let preparation = broker.preparation_status(session_id);
    let preparation_value = match &preparation {
        Ok(status) => json!({
            "state": status.state.as_str(),
            "reason": status.reason,
            "next_action": status.next_action,
        }),
        Err(error) => json!({
            "state": "unknown",
            "error": error.to_string(),
        }),
    };
    let advice: Vec<Value> = session_advice
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .collect();
    let blockers: Vec<Value> = session_advice
        .iter()
        .filter(|item| item.severity == crate::StatusAdviceSeverity::Blocked)
        .map(|item| json!({"id": item.id, "summary": item.summary}))
        .collect();
    let next_action = next_action(NextActionFacts {
        session_id,
        closed: session.status.is_closed(),
        blocker_command: session_advice
            .iter()
            .filter(|item| item.severity == crate::StatusAdviceSeverity::Blocked)
            .find_map(|item| item.commands.first().cloned()),
        preparation_command: preparation.as_ref().ok().and_then(|status| {
            matches!(
                status.state,
                crate::PreparationState::Required
                    | crate::PreparationState::Stale
                    | crate::PreparationState::InProgress
                    | crate::PreparationState::Interrupted
                    | crate::PreparationState::Failed
                    | crate::PreparationState::Invalid
            )
            .then(|| status.next_action.clone())
            .flatten()
        }),
        unpushed_commits: push.as_ref().ok().map(|state| state.unpushed_commits),
        pr_recorded: watch.is_some(),
        push_enabled: crate::session_push::session_push_enabled(broker.repo_handle()),
        inspect_worktree: format!(
            "git -C '{}' status --short",
            session.worktree_path.replace('\'', "'\\''")
        ),
        untouched_branch: push.as_ref().is_ok_and(|state| {
            state.remote_oid.is_none()
                && session.diff_base.as_deref() == Some(state.head_oid.as_str())
        }),
    });
    json!({
        "id": session.id,
        "status": session.status.as_str(),
        "branch": session.branch,
        "worktree": session.worktree_path,
        "head_oid": head_oid,
        "remote_oid": remote_oid,
        "unpushed_commits": unpushed_commits,
        "push_state_error": push_state_error,
        "pr": pr,
        "pr_state": pr_state,
        "preparation": preparation_value,
        "blockers": blockers,
        "advice": advice,
        "next_action": next_action,
    })
}

/// Routine advice is scoped to the current task; raw evidence is on demand.
pub(crate) fn routine_session_block(
    broker: &crate::Broker,
    session_id: i64,
    coordination: &[crate::Blocker],
) -> Result<Value, crate::BrokerOpError> {
    use crate::blockers::BlockerKind;

    let advice = broker.session_status_advice(session_id, crate::clock::epoch_ms())?;
    let mut block = session_block(broker, session_id, &advice);
    for item in block["advice"].as_array_mut().into_iter().flatten() {
        if let Some(item) = item.as_object_mut() {
            item.remove("evidence");
            item.remove("commands");
        }
    }
    let Some(blockers) = block["blockers"].as_array_mut() else {
        return Ok(block);
    };
    blockers.extend(coordination.iter().take(10).map(|item| json!({
        "id": item.id,
        "summary": match item.kind {
            BlockerKind::Operation => "A write from this session has an unknown outcome",
            BlockerKind::ResourceLease => "A resource associated with this session needs recovery",
            BlockerKind::Pidfile => "A gate ran from this session and left a stale pidfile",
            BlockerKind::ActionRequired => "Read this worktree's .aethyme/broker-action-required.md",
            _ => "A coordination record for this session needs inspection",
        },
    })));
    if let Some(first) = coordination.first()
        && block["status"] != "cleaned"
        && block["status"] != "closed"
    {
        block["next_action"] = json!({
            "command": if first.kind == BlockerKind::ActionRequired {
                let worktree = block["worktree"].as_str().unwrap_or_default().replace('\'', "'\\''");
                format!("cat '{worktree}'/.aethyme/broker-action-required.md")
            } else {
                "aethyme broker unblock".to_string()
            },
            "reason": "inspect recovery evidence for this session",
        });
    }
    Ok(block)
}

pub(crate) fn render_routine_session(block: &Value, show_worktree: bool) -> String {
    use std::fmt::Write as _;
    let mut text = format!(
        "Aethyme session {} ({})",
        block["id"],
        block["status"].as_str().unwrap_or("unknown")
    );
    if show_worktree && let Some(worktree) = block["worktree"].as_str() {
        let _ = write!(text, "\nWorktree: {worktree}");
    }
    if let Some(count) = block["unpushed_commits"].as_u64() {
        let _ = write!(text, "\nUnpushed commits: {count}");
    }
    if let Some(number) = block["pr"]["number"].as_u64() {
        let kind = if block["pr"]["draft"] == true {
            "draft "
        } else {
            ""
        };
        let head = if block["pr"]["head_matches_local"] == true {
            ""
        } else {
            " (recorded head differs or is unknown)"
        };
        let _ = write!(text, "\nPull request: {kind}#{number}{head}");
    }
    for blocker in block["blockers"].as_array().into_iter().flatten() {
        if let Some(summary) = blocker["summary"].as_str() {
            let _ = write!(text, "\nAction required: {summary}");
        }
    }
    if let (Some(command), Some(reason)) = (
        block["next_action"]["command"].as_str(),
        block["next_action"]["reason"].as_str(),
    ) {
        let _ = write!(text, "\nNext: {command} ({reason})");
    }
    text
}

/// The facts the session block's `next_action` is decided from.
pub(super) struct NextActionFacts {
    pub(super) session_id: i64,
    pub(super) closed: bool,
    pub(super) blocker_command: Option<String>,
    pub(super) preparation_command: Option<String>,
    pub(super) unpushed_commits: Option<u32>,
    pub(super) pr_recorded: bool,
    pub(super) push_enabled: bool,
    pub(super) inspect_worktree: String,
    pub(super) untouched_branch: bool,
}

/// One exact command and why, in priority order: a closed session, a
/// blocker, missing preparation, unpushed commits, then the PR lifecycle.
pub(super) fn next_action(facts: NextActionFacts) -> Value {
    let id = facts.session_id;
    let (command, reason) = if facts.closed {
        (
            format!("aethyme broker advanced handoff --session {id}"),
            "the session is closed",
        )
    } else if let Some(command) = facts.blocker_command {
        (command, "a blocker names this session")
    } else if let Some(command) = facts.preparation_command {
        (command, "the worktree's dependencies are not prepared")
    } else if facts.untouched_branch && !facts.pr_recorded {
        (
            facts.inspect_worktree,
            "inspect worktree changes before committing",
        )
    } else if !facts.push_enabled {
        (
            format!("aethyme broker submit --session {id}"),
            "validate the committed session head under the repository's delivery policy",
        )
    } else if facts.unpushed_commits.is_some_and(|count| count > 0) {
        if facts.pr_recorded {
            (
                format!("aethyme broker push --session {id}"),
                "commits exist only on this machine",
            )
        } else {
            (
                format!("aethyme broker push --session {id} --pr"),
                "commits exist only on this machine and no pull request is recorded",
            )
        }
    } else if facts.unpushed_commits.is_none() {
        (
            format!("aethyme broker push --session {id} --plan"),
            "the branch could not be compared with its remote",
        )
    } else if facts.pr_recorded {
        (
            format!("aethyme broker submit --session {id}"),
            "everything is pushed and a pull request is recorded",
        )
    } else {
        (
            format!("aethyme broker push --session {id} --pr"),
            "everything is pushed but no pull request is recorded",
        )
    };
    json!({"command": command, "reason": reason})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advice_with(evidence: Vec<String>) -> Value {
        json!({
            "id": "storage.over-budget",
            "severity": "notice",
            "summary": "1 storage budget(s) exceeded",
            "evidence": evidence,
            "commands": ["aethyme storage status"],
        })
    }

    #[test]
    fn evidence_under_the_cap_is_unchanged() {
        let mut document = json!({"advice": [advice_with(vec!["small".into(); 10])]});
        let before = document.clone();
        assert_eq!(cap_document_advice_evidence(&mut document), 0);
        assert_eq!(document, before);
    }

    #[test]
    fn evidence_over_the_cap_keeps_a_count_the_first_items_and_a_command() {
        let items: Vec<String> = (0..5_000)
            .map(|index| format!("recovery archive {index:05} holds unlanded work and is kept"))
            .collect();
        let mut document = json!({
            "advice": [advice_with(items.clone())],
            "session": {"advice": [advice_with(items)]},
        });
        assert_eq!(cap_document_advice_evidence(&mut document), 2);
        for entry in [&document["advice"][0], &document["session"]["advice"][0]] {
            let evidence = entry["evidence"].as_array().unwrap();
            assert!(!evidence.is_empty() && evidence.len() <= KEPT_EVIDENCE_ITEMS);
            assert_eq!(
                evidence[0],
                "recovery archive 00000 holds unlanded work and is kept"
            );
            assert!(serde_json::to_vec(evidence).unwrap().len() <= EVIDENCE_CAP_BYTES);
            let truncation = &entry["evidence_truncation"];
            assert_eq!(truncation["truncated"], true);
            assert_eq!(truncation["count"], 5_000);
            assert_eq!(truncation["shown"], evidence.len());
            assert_eq!(truncation["omitted"], 5_000 - evidence.len());
            assert_eq!(truncation["command"], UNCAPPED_EVIDENCE_COMMAND);
            // The other fields keep their names and values.
            assert_eq!(entry["commands"][0], "aethyme storage status");
        }
    }

    #[test]
    fn one_oversized_item_is_cut_on_a_character_boundary() {
        let huge = "é".repeat(10_000);
        let mut document = json!({"advice": [advice_with(vec![huge])]});
        assert_eq!(cap_document_advice_evidence(&mut document), 1);
        let kept = document["advice"][0]["evidence"][0].as_str().unwrap();
        assert!(kept.len() < KEPT_ITEM_BYTES + 64);
        assert!(kept.ends_with("(20000 bytes)"));
        assert_eq!(document["advice"][0]["evidence_truncation"]["shown"], 1);
    }

    fn facts() -> NextActionFacts {
        NextActionFacts {
            session_id: 7,
            closed: false,
            blocker_command: None,
            preparation_command: None,
            unpushed_commits: Some(0),
            pr_recorded: true,
            push_enabled: true,
            inspect_worktree: "git -C /fixture status --short".into(),
            untouched_branch: false,
        }
    }

    #[test]
    fn next_action_is_one_exact_command_in_priority_order() {
        let command = |facts| next_action(facts)["command"].as_str().unwrap().to_string();
        assert_eq!(
            command(NextActionFacts {
                closed: true,
                blocker_command: Some("aethyme broker unblock op:1".into()),
                ..facts()
            }),
            "aethyme broker advanced handoff --session 7"
        );
        assert_eq!(
            command(NextActionFacts {
                blocker_command: Some("aethyme broker unblock op:1".into()),
                preparation_command: Some("aethyme broker submit prepare --session 7".into()),
                ..facts()
            }),
            "aethyme broker unblock op:1"
        );
        assert_eq!(
            command(NextActionFacts {
                preparation_command: Some("aethyme broker submit prepare --session 7".into()),
                unpushed_commits: Some(2),
                ..facts()
            }),
            "aethyme broker submit prepare --session 7"
        );
        assert_eq!(
            command(NextActionFacts {
                unpushed_commits: Some(2),
                ..facts()
            }),
            "aethyme broker push --session 7"
        );
        assert_eq!(
            command(NextActionFacts {
                unpushed_commits: Some(2),
                pr_recorded: false,
                ..facts()
            }),
            "aethyme broker push --session 7 --pr"
        );
        assert_eq!(
            command(NextActionFacts {
                unpushed_commits: None,
                ..facts()
            }),
            "aethyme broker push --session 7 --plan"
        );
        assert_eq!(
            command(NextActionFacts {
                push_enabled: false,
                ..facts()
            }),
            "aethyme broker submit --session 7"
        );
        assert_eq!(command(facts()), "aethyme broker submit --session 7");
        assert_eq!(
            command(NextActionFacts {
                pr_recorded: false,
                untouched_branch: true,
                ..facts()
            }),
            "git -C /fixture status --short"
        );
    }
}
