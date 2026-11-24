//! What an automatic session close records, and what it tells the agent.
//!
//! The broker closes an open session without `finish` in two lanes, both in
//! `Broker::try_close_session_inner`: the landed lane (the session's work is
//! proven on the fetched default branch, its checkout is clean and nothing
//! runs for it; earliest after `session_merged_after_hours` when every
//! recorded pull request merged) and the abandonment window
//! (`session_abandoned_after_hours`). Either close is the state transition a
//! successful `finish` makes: nothing is deleted, the checkout waits out its
//! closed-checkout grace and automatic removal judges it on its own proofs.
//!
//! An agent that comes back to such a session must learn why it is closed
//! and how to continue, instead of a bare refusal. This module owns the
//! evidence written with the close (`session.abandoned` payload fields
//! `close_reason`, `pull_requests`, `head`, ...), the note left for the
//! session, and the explanation a refused session command prints.

use serde_json::{Value, json};

/// Sender of the note left for an automatically closed session.
pub(crate) const NOTE_SENDER: &str = "automation:session-auto-close";
/// One unread close note per session at most.
pub(crate) const NOTE_FINGERPRINT: &str = "session-auto-close";

/// Every recorded pull request merged and the HEAD is proven landed.
pub(crate) const CLOSE_REASON_MERGED_PULL_REQUEST: &str = "merged_pull_request_landed";
/// The HEAD is proven landed (by ancestry, a representation or content).
pub(crate) const CLOSE_REASON_LANDED: &str = "landed";
/// The abandonment window elapsed and no pull request was ever recorded.
pub(crate) const CLOSE_REASON_INACTIVE_NO_PULL_REQUEST: &str = "inactive_no_pull_request";
/// The abandonment window elapsed for a session with a pull request.
pub(crate) const CLOSE_REASON_INACTIVE: &str = "inactive";
/// Closed by `session_no_work_abandoned_after_hours`: the session never did
/// any work (HEAD still its diff base, clean checkout) and its holder is gone.
pub(crate) const CLOSE_REASON_INACTIVE_NO_WORK: &str = "inactive_no_work";

/// `payload` with its `close_reason` replaced by `close_reason`.
pub(crate) fn with_close_reason(payload: &str, close_reason: &str) -> String {
    let mut value: Value =
        serde_json::from_str(payload).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Value::Object(fields) = &mut value {
        fields.insert("close_reason".into(), json!(close_reason));
    }
    value.to_string()
}

/// `payload` with `work_class` set: what the abandonment window's per-class
/// gate found the session's work to be when it closed (`landed`,
/// `pushed_unmerged`, `unpushed`).
pub(crate) fn with_work_class(payload: &str, work_class: &str) -> String {
    let mut value: Value =
        serde_json::from_str(payload).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Value::Object(fields) = &mut value {
        fields.insert("work_class".into(), json!(work_class));
    }
    value.to_string()
}

/// The checks of the early-close rule a close before the class window
/// passed, in the order they ran, as recorded on its `session.abandoned`
/// event (`early_close`).
pub(crate) const EARLY_CLOSE_CHECKS: [&str; 7] = [
    "no_operation_in_progress",
    "no_stash_on_branch",
    "head_and_branch_contained",
    "no_staged_change",
    "no_unstaged_change",
    "no_hidden_index_entries",
    "no_untracked_file",
];

/// Mark a close payload as an early close: before the work-class window,
/// because nothing on the session could be lost.
pub(crate) fn with_early_close(payload: &str) -> String {
    let mut value: Value =
        serde_json::from_str(payload).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Value::Object(fields) = &mut value {
        fields.insert(
            "early_close".into(),
            json!({
                "rule": "nothing_on_the_session_can_be_lost",
                "checks": EARLY_CLOSE_CHECKS,
            }),
        );
    }
    value.to_string()
}

/// The pull requests recorded for one session, from
/// `BrokerStore::session_pull_request_merges`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PullRequestMerges {
    pub links: Vec<(String, i64, Option<i64>)>,
}

impl PullRequestMerges {
    pub(crate) fn read(store: &crate::BrokerStore, session_id: i64) -> Option<Self> {
        store
            .session_pull_request_merges(session_id)
            .ok()
            .map(|links| Self { links })
    }

    /// At least one pull request is recorded, and every one of them merged.
    pub(crate) fn all_merged(&self) -> bool {
        !self.links.is_empty() && self.links.iter().all(|(_, _, merged)| merged.is_some())
    }

    /// When the last recorded pull request merged; `None` unless all merged.
    pub(crate) fn latest_merged_at(&self) -> Option<i64> {
        if !self.all_merged() {
            return None;
        }
        self.links.iter().filter_map(|(_, _, merged)| *merged).max()
    }

    /// A whitespace-free digest of every recorded pull request and its merge
    /// time: a change signal for memos that depend on them.
    pub(crate) fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for (repository, number, merged_at) in &self.links {
            hasher.update(format!("{repository}#{number}@{merged_at:?};").as_bytes());
        }
        let digest = hasher.finalize();
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub(crate) fn to_json(&self) -> Value {
        Value::Array(
            self.links
                .iter()
                .map(|(repository, number, merged_at)| {
                    json!({
                        "repository": repository,
                        "number": number,
                        "merged_at": merged_at,
                    })
                })
                .collect(),
        )
    }
}

/// The `session.abandoned` payload for a landed-lane close.
#[allow(clippy::too_many_arguments)]
pub(crate) fn landed_close_payload(
    close_reason: &str,
    idle_ms: i64,
    window_ms: i64,
    pid_alive: Option<bool>,
    retirement_basis: &str,
    proof: &crate::broker::LandedProof,
    pull_requests: &PullRequestMerges,
    holder_evidence: &str,
    moved_on_to: Option<i64>,
) -> String {
    json!({
        "holder_evidence": holder_evidence,
        "holder_moved_on_to_session": moved_on_to,
        "idle_ms": idle_ms,
        "abandon_after_ms": window_ms,
        "pid_alive": pid_alive,
        "retirement_basis": retirement_basis,
        "close_reason": close_reason,
        "head": proof.head,
        "upstream_tip": proof.upstream_tip,
        "landing_evidence": proof.evidence,
        "pull_requests": pull_requests.to_json(),
        "checkout_deleted": false,
    })
    .to_string()
}

/// The `session.abandoned` payload for an abandonment-window close: the
/// existing fields plus the close reason and whether the session's HEAD
/// held commits the default branch does not (`None`: not established).
pub(crate) fn abandonment_close_payload(
    base_payload: &str,
    pull_requests: Option<&PullRequestMerges>,
    head: Option<&str>,
    unlanded_commits: Option<bool>,
    holder_evidence: &str,
    moved_on_to: Option<i64>,
) -> String {
    let mut payload: Value =
        serde_json::from_str(base_payload).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Value::Object(fields) = &mut payload {
        let close_reason = match pull_requests {
            Some(merges) if merges.links.is_empty() => CLOSE_REASON_INACTIVE_NO_PULL_REQUEST,
            _ => CLOSE_REASON_INACTIVE,
        };
        fields.insert("close_reason".into(), json!(close_reason));
        if let Some(merges) = pull_requests {
            fields.insert("pull_requests".into(), merges.to_json());
        }
        fields.insert("head".into(), json!(head));
        fields.insert("unlanded_commits".into(), json!(unlanded_commits));
        fields.insert("checkout_deleted".into(), json!(false));
        fields.insert("holder_evidence".into(), json!(holder_evidence));
        fields.insert("holder_moved_on_to_session".into(), json!(moved_on_to));
    }
    payload.to_string()
}

/// Why the payload says the session was closed, in words.
fn close_reason_text(payload: &Value) -> String {
    let reason = close_reason_basis(payload);
    match (
        payload.get("holder_evidence").and_then(Value::as_str),
        payload
            .get("holder_moved_on_to_session")
            .and_then(Value::as_i64),
    ) {
        (Some("moved_on"), Some(newer)) => format!(
            "{reason}; its agent process was still running but had moved on to session \
             {newer}, and nothing was using this checkout"
        ),
        (Some("lease_expired"), _) => {
            format!("{reason}; its agent process was still running but its task lease had expired")
        }
        _ => reason,
    }
}

fn close_reason_basis(payload: &Value) -> String {
    let idle_hours = payload
        .get("idle_ms")
        .and_then(Value::as_i64)
        .map(|ms| ms / 3_600_000);
    let idle = match idle_hours {
        Some(hours) if hours >= 48 => format!("{} days", hours / 24),
        Some(hours) => format!("{hours} hours"),
        None => "a long time".into(),
    };
    let pull_requests = payload
        .get("pull_requests")
        .and_then(Value::as_array)
        .map(|links| {
            links
                .iter()
                .filter_map(|link| link.get("number").and_then(Value::as_i64))
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let head = payload
        .get("head")
        .and_then(Value::as_str)
        .map(|head| head.chars().take(12).collect::<String>());
    match payload.get("close_reason").and_then(Value::as_str) {
        Some(CLOSE_REASON_MERGED_PULL_REQUEST) => format!(
            "every pull request recorded for it merged ({pull_requests}), its HEAD {} is on \
             the default branch, its checkout was clean and it was inactive for {idle}",
            head.unwrap_or_default()
        ),
        Some(CLOSE_REASON_LANDED) => format!(
            "its HEAD {} is on the default branch, its checkout was clean and it was inactive \
             for {idle}",
            head.unwrap_or_default()
        ),
        Some(CLOSE_REASON_INACTIVE_NO_PULL_REQUEST) => format!(
            "no pull request was recorded for it and it was inactive for {idle} with no live \
             agent{}",
            if payload.get("unlanded_commits").and_then(Value::as_bool) == Some(true) {
                "; its commits are not on the default branch yet, so its branch is kept and \
                 they are bundled into a recovery archive before its checkout is retired"
            } else {
                ""
            }
        ),
        Some(CLOSE_REASON_INACTIVE_NO_WORK) => format!(
            "it never committed past its diff base, its checkout was clean, its agent was \
             gone and it was inactive for {idle}"
        ),
        _ => format!("it was inactive for {idle} with no live agent"),
    }
}

/// How to go on after an automatic close, depending on whether the checkout
/// is still on disk. The checkout gets no grace period: the broker retires
/// it soon after the close (committed work stays on the session branch;
/// anything else goes into a verified recovery archive first).
fn continuation(worktree_path: &str) -> String {
    match std::fs::symlink_metadata(worktree_path) {
        Ok(_) => format!(
            "Its checkout {worktree_path} is still on disk for now; the broker retires it shortly \
             (committed work stays on the session branch, anything uncommitted goes into a \
             recovery archive first). To continue there, run \
             `aethyme broker start --reuse --task \"<task>\" --short-name \"<name>\"` from \
             inside that checkout"
        ),
        Err(_) => format!(
            "Its checkout has been retired. Committed work is on the session branch; anything \
             uncommitted was kept in a recovery archive. To continue, run \
             `aethyme broker start --reuse {worktree_path} --task \"<task>\" --short-name \
             \"<name>\"`: it recreates the checkout from the session branch, or names the \
             archive and its restore steps when one holds leftovers"
        ),
    }
}

/// The note left for a session the broker just closed automatically.
pub(crate) fn close_note(session_id: i64, worktree_path: &str, payload: &str) -> String {
    let payload = serde_json::from_str::<Value>(payload).unwrap_or(Value::Null);
    format!(
        "The broker closed session {session_id} automatically: {}. Closing deleted nothing. \
         {}.",
        close_reason_text(&payload),
        continuation(worktree_path)
    )
}

/// What a session command refused on a closed session adds, when the
/// broker closed that session itself; `None` for a session closed by
/// `finish` or by an operator.
pub(crate) fn explain_automatic_close(
    store: &crate::BrokerStore,
    session_id: i64,
) -> Option<String> {
    let event = store.latest_session_abandoned_event(session_id).ok()??;
    let worktree_path = store.session(session_id).ok()?.worktree_path;
    let payload = event
        .payload_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or(Value::Null);
    Some(format!(
        "the broker closed session {session_id} automatically because {}; closing deleted \
         nothing. {}",
        close_reason_text(&payload),
        continuation(&worktree_path)
    ))
}

/// The refusal a session command prints for a closed session.
pub(crate) fn closed_session_refusal(store: &crate::BrokerStore, session_id: i64) -> String {
    let refusal = match explain_automatic_close(store, session_id) {
        Some(explanation) => format!(
            "session {session_id} is already closed; it cannot start new session work: \
             {explanation}"
        ),
        None => format!("session {session_id} is already closed; it cannot start new session work"),
    };
    match restore_required(store, session_id) {
        Some(action) => {
            // `--json` refusals carry the action as data too.
            REFUSAL_ACTION.with(|cell| *cell.borrow_mut() = Some(action.json()));
            crate::phase_telemetry::add(
                "closed_session_refusal_restore_required",
                std::time::Duration::ZERO,
            );
            format!("{refusal}; {}", action.text())
        }
        None => refusal,
    }
}

/// The `restore_required` action of a session whose checkout was retired
/// and left a tombstone: where, and the one command that restores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoreRequired {
    pub session_id: i64,
    pub path: std::path::PathBuf,
    pub command: String,
}

impl RestoreRequired {
    pub(crate) fn text(&self) -> String {
        format!(
            "restore_required: its checkout was retired with all its work saved; restore it at the same path with `{}`",
            self.command
        )
    }

    /// The structured form `--json` errors carry under `error.action`.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": "restore_required",
            "session_id": self.session_id,
            "path": self.path,
            "command": self.command,
        })
    }
}

thread_local! {
    /// The structured action of the last refusal built on this thread, for
    /// the `--json` error envelope.
    static REFUSAL_ACTION: std::cell::RefCell<Option<serde_json::Value>> =
        const { std::cell::RefCell::new(None) };
}

/// Leave `action` as this thread's structured refusal action.
#[cfg(test)]
pub(crate) fn set_refusal_action_for_test(action: serde_json::Value) {
    REFUSAL_ACTION.with(|cell| *cell.borrow_mut() = Some(action));
}

/// Forget any structured refusal action left on this thread.
pub(crate) fn clear_refusal_action() {
    REFUSAL_ACTION.with(|cell| *cell.borrow_mut() = None);
}

/// Take the structured action of the last refusal built on this thread.
pub(crate) fn take_refusal_action() -> Option<serde_json::Value> {
    REFUSAL_ACTION.with(|cell| cell.borrow_mut().take())
}

/// For a session whose checkout was retired and left a tombstone: the
/// `restore_required` action and its one command.
pub(crate) fn restore_required(
    store: &crate::BrokerStore,
    session_id: i64,
) -> Option<RestoreRequired> {
    let session = store.session(session_id).ok()?;
    let path = std::path::Path::new(&session.worktree_path);
    crate::broker::is_checkout_tombstone(path).then(|| RestoreRequired {
        session_id,
        path: path.to_path_buf(),
        command: crate::broker::restore_command(path),
    })
}

/// For a command run from a checkout without `--session`: when the latest
/// session of this checkout is closed and the broker closed it itself, the
/// explanation; the command itself is not refused here.
pub(crate) fn closed_checkout_notice(
    store: &crate::BrokerStore,
    worktree_path: &str,
) -> Option<String> {
    let session = store
        .latest_session_for_worktree_any_state(worktree_path)
        .ok()??;
    if session.cleanup_state == crate::SessionCleanupState::Open {
        return None;
    }
    explain_automatic_close(store, session.id).map(|explanation| {
        format!(
            "this checkout's session {} is closed: {explanation}",
            session.id
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merges(links: &[(i64, Option<i64>)]) -> PullRequestMerges {
        PullRequestMerges {
            links: links
                .iter()
                .map(|(number, merged)| ("fixture".to_string(), *number, *merged))
                .collect(),
        }
    }

    #[test]
    fn only_a_non_empty_set_of_merged_pull_requests_counts_as_all_merged() {
        assert!(!merges(&[]).all_merged(), "no pull request is not a merge");
        assert!(merges(&[(1, Some(10)), (2, Some(20))]).all_merged());
        assert!(
            !merges(&[(1, Some(10)), (2, None)]).all_merged(),
            "a second open or closed-unmerged pull request keeps the session"
        );
        assert_eq!(
            merges(&[(1, Some(10)), (2, Some(20))]).latest_merged_at(),
            Some(20)
        );
        assert_eq!(merges(&[(1, Some(10)), (2, None)]).latest_merged_at(), None);
    }

    #[test]
    fn an_abandonment_close_names_a_session_without_any_pull_request() {
        let base = crate::events::session_abandoned_payload(3 * 86_400_000, 259_200_000, None);
        let none = abandonment_close_payload(
            &base,
            Some(&merges(&[])),
            Some("abc"),
            Some(true),
            "moved_on",
            Some(77),
        );
        let value: Value = serde_json::from_str(&none).unwrap();
        assert_eq!(value["close_reason"], CLOSE_REASON_INACTIVE_NO_PULL_REQUEST);
        assert_eq!(value["unlanded_commits"], true);
        assert_eq!(value["checkout_deleted"], false);
        assert_eq!(value["idle_ms"], 3 * 86_400_000, "existing fields are kept");
        let with_pr = abandonment_close_payload(
            &base,
            Some(&merges(&[(4, None)])),
            Some("abc"),
            None,
            "gone",
            None,
        );
        let value: Value = serde_json::from_str(&with_pr).unwrap();
        assert_eq!(value["close_reason"], CLOSE_REASON_INACTIVE);
        let note = close_note(9, "/nonexistent/auto-close-fixture", &none);
        assert!(note.contains("no pull request was recorded"), "{note}");
        assert!(note.contains("3 days"), "{note}");
        assert!(note.contains("its branch is kept"), "{note}");
        assert!(note.contains("moved on to session 77"), "{note}");
        assert!(note.contains("has been retired"), "{note}");
        assert!(note.contains("start --reuse"), "{note}");
    }

    #[test]
    fn a_merged_pull_request_close_names_the_pull_requests_and_the_head() {
        let payload = json!({
            "idle_ms": 3 * 3_600_000,
            "close_reason": CLOSE_REASON_MERGED_PULL_REQUEST,
            "head": "0123456789abcdef",
            "pull_requests": [{"repository": "fixture", "number": 12, "merged_at": 1}],
        })
        .to_string();
        let note = close_note(5, "/nonexistent/auto-close-fixture", &payload);
        assert!(note.contains("#12"), "{note}");
        assert!(note.contains("0123456789ab"), "{note}");
        assert!(note.contains("3 hours"), "{note}");
        assert!(note.contains("Closing deleted nothing"), "{note}");
    }
}

/// The two automatic closes end to end, on a real repository with a local
/// `origin`: the merged-pull-request window and the abandonment window.
#[cfg(all(test, unix))]
mod lane_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use serde_json::Value;

    use crate::types::SessionCleanupState;

    const HOUR_MS: i64 = 3_600_000;

    /// `[cleanup]` per-class windows no longer than the abandonment window.
    const CLASS_WINDOWS_OFF: &str = "[cleanup]\nstale_landed_close_after_hours = 1\n\
        stale_pushed_close_after_hours = 1\nstale_unpushed_close_after_hours = 1\n\
        stale_closing_notice_hours = 0\n";

    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn now_ms() -> i64 {
        crate::clock::epoch_ms()
    }

    /// A repository with a local `origin` whose default branch is known, the
    /// shipped windows (merged 2 h, landed 24 h, abandoned 72 h) unless
    /// `retention` overrides them, and the closed-checkout grace given.
    fn fixture(
        retention: &str,
    ) -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto close test"]);
        git(
            &root,
            &["config", "user.email", "auto-close@example.invalid"],
        );
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&root, &["add", ".gitignore", "base.txt"]);
        git(&root, &["commit", "-qm", "fixture"]);
        aethyme_testkit::ensure_origin(&root);
        git(&root, &["remote", "set-head", "origin", "main"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            format!("[retention]\nsession_abandoned_after_hours = 72\n{retention}"),
        )
        .unwrap();
        // Every test here judges the abandonment window alone, except the
        // per-class tests, which remove this file to get the shipped windows.
        std::fs::write(root.join(".aethyme/config.toml"), CLASS_WINDOWS_OFF).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        // No process on this test thread uses any checkout: the open-file
        // observation is fixed instead of running `lsof`.
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        (owner, storage, broker)
    }

    /// Start a session, commit one file and, with `merge`, land it on the
    /// default branch by a merge-commit pull request.
    fn session(broker: &mut crate::Broker, label: &str, merge: bool) -> (crate::Session, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let file = format!("work-{}.txt", session.id);
        std::fs::write(worktree.join(&file), format!("{label}\n")).unwrap();
        git(&worktree, &["add", &file]);
        git(&worktree, &["commit", "-qm", label]);
        if merge {
            let root = broker.main_root().to_path_buf();
            aethyme_testkit::land_as_pull_request(
                &root,
                &session.branch,
                aethyme_testkit::MergeStyle::MergeCommit,
            );
        }
        (session, worktree)
    }

    fn link_pull_request(
        broker: &mut crate::Broker,
        session_id: i64,
        number: i64,
        merged_at: Option<i64>,
    ) {
        let connection = broker.store().connection();
        connection
            .execute(
                "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', ?1, ?2, 1, 'watch')",
                rusqlite::params![number, session_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO pull_request_milestones (repository, pr_number, merged_at, first_seen_at, updated_at) VALUES ('fixture', ?1, ?2, 1, 1)",
                rusqlite::params![number, merged_at],
            )
            .unwrap();
    }

    /// Bind a holder process that no longer exists, and push the session's
    /// recorded and Git-visible activity `hours` into the past.
    fn quiet(broker: &mut crate::Broker, session: &crate::Session, hours: u64) {
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "fixture-agent".into(),
        };
        bind_holder(broker, session, &holder);
        age(broker, session, hours);
    }

    fn bind_holder(
        broker: &mut crate::Broker,
        session: &crate::Session,
        holder: &crate::session_holder::AgentProcess,
    ) -> i64 {
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    holder, "fixture", None,
                )),
            )
            .unwrap();
        broker
            .store_ref()
            .latest_session_holder_event(session.id)
            .unwrap()
            .unwrap()
            .id
    }

    /// Push the session's recorded and Git-visible activity `hours` back.
    fn age(broker: &mut crate::Broker, session: &crate::Session, hours: u64) {
        let old_time = std::time::SystemTime::now() - Duration::from_secs(hours * 60 * 60);
        let old_ms = old_time
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let checkout = PathBuf::from(&session.worktree_path);
        let git_dir = crate::git::checked_worktree_git_dir(&checkout)
            .unwrap()
            .unwrap();
        for name in ["index", "HEAD"] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(git_dir.join(name))
                .unwrap()
                .set_modified(old_time)
                .unwrap();
        }
        crate::worktree_status_activity::age_tree_for_test(&checkout, old_time);
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET created_at = ?1, updated_at = ?1, last_activity_at = ?1 WHERE id = ?2",
                rusqlite::params![old_ms, session.id],
            )
            .unwrap();
    }

    fn close_payload(broker: &crate::Broker, session_id: i64) -> Value {
        let event = broker
            .store_ref()
            .latest_session_abandoned_event(session_id)
            .unwrap()
            .expect("an automatic close writes session.abandoned");
        serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap()
    }

    fn is_open(broker: &crate::Broker, session_id: i64) -> bool {
        broker
            .store_ref()
            .session(session_id)
            .unwrap()
            .cleanup_state
            == SessionCleanupState::Open
    }

    /// The merged-pull-request window: landed, clean, quiet for three hours,
    /// every pull request merged. Closed as `finish` closes, with the proof
    /// recorded, a note left, a named refusal for the returning agent, and
    /// the checkout still on disk.
    #[test]
    fn a_landed_session_with_every_pull_request_merged_closes_after_two_hours() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, worktree) = session(&mut broker, "merged and landed", true);
        link_pull_request(&mut broker, session.id, 11, Some(1));
        quiet(&mut broker, &session, 3);

        let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

        assert_eq!(
            report.merged_pull_request_sessions_examined, 1,
            "{report:#?}"
        );
        assert_eq!(report.merged_pull_request_sessions_closed, 1, "{report:#?}");
        assert_eq!(report.landed_sessions_closed, 1, "{report:#?}");
        let closed = broker.store_ref().session(session.id).unwrap();
        assert_eq!(closed.cleanup_state, SessionCleanupState::Closed);
        assert!(closed.status.is_closed());
        assert!(worktree.exists(), "closing never deletes the checkout");

        let payload = close_payload(&broker, session.id);
        assert_eq!(
            payload["close_reason"],
            super::CLOSE_REASON_MERGED_PULL_REQUEST
        );
        assert_eq!(payload["pull_requests"][0]["number"], 11);
        assert_eq!(payload["landing_evidence"], "ancestry");
        let head = crate::git::GitRepo::discover(&worktree)
            .unwrap()
            .head_commit()
            .unwrap();
        assert_eq!(payload["head"], head.as_str());
        assert_eq!(payload["checkout_deleted"], false);
        // The landing proof is remembered for this head, so the next lap
        // answers with one ancestry check instead of searching again.
        let remembered = broker
            .store_ref()
            .meta_get(&format!("landed_lane.landed.{}", session.id))
            .unwrap()
            .expect("a proved landing is remembered");
        assert!(remembered.starts_with(head.as_str()), "{remembered}");

        let notes = broker
            .store_ref()
            .unread_automation_notes(session.id)
            .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].sender, super::NOTE_SENDER);
        assert!(notes[0].message.contains("#11"), "{}", notes[0].message);

        let refusal = super::closed_session_refusal(broker.store_ref(), session.id);
        assert!(refusal.contains("closed session"), "{refusal}");
        assert!(refusal.contains("automatically"), "{refusal}");
        assert!(refusal.contains("broker start --reuse"), "{refusal}");
    }

    /// Ignored content no longer keeps a landed session open: closing
    /// deletes nothing, and the checkout-removal proof still lists and
    /// classifies the ignored directory before anything is removed.
    #[test]
    fn ignored_content_keeps_the_checkout_but_not_the_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let root = broker.main_root().to_path_buf();
        std::fs::write(root.join(".git/info/exclude"), "project-cache/\n").unwrap();
        let (session, worktree) = session(&mut broker, "merged with an ignored cache", true);
        std::fs::create_dir_all(worktree.join("project-cache/quality")).unwrap();
        std::fs::write(worktree.join("project-cache/quality/index.json"), "{}\n").unwrap();
        link_pull_request(&mut broker, session.id, 12, Some(1));
        quiet(&mut broker, &session, 3);

        let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

        assert_eq!(report.merged_pull_request_sessions_closed, 1, "{report:#?}");
        assert!(!is_open(&broker, session.id));
        assert!(worktree.join("project-cache/quality/index.json").exists());
        let policy = crate::auto_cleanup::AutoCleanupPolicy::load(&root).unwrap();
        let removal = crate::auto_cleanup::unclean_reason(
            &worktree,
            Some(session.branch.as_str()),
            &root,
            &policy,
        )
        .unwrap()
        .expect("the removal proof still keeps the checkout");
        assert_eq!(
            crate::auto_cleanup::kept_reason_code(&removal),
            "ignored_content",
            "{removal}"
        );
    }

    /// A script proof-3 listings run instead of Git: `status` runs `status`
    /// (a shell fragment), anything else goes to the real Git.
    fn status_script(dir: &Path, name: &str, status: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncase \" $* \" in\n  *\" status \"*) {status} ;;\n  *) exec git \"$@\" ;;\nesac\n"
            ),
        )
        .unwrap();
        path
    }

    fn merged_quiet_session(broker: &mut crate::Broker, label: &str) -> crate::Session {
        let (session, _) = session(broker, label, true);
        link_pull_request(broker, session.id, 31, Some(1));
        quiet(broker, &session, 3);
        session
    }

    fn close_merged(
        broker: &mut crate::Broker,
        session: &crate::Session,
        table: &mut crate::session_holder::RecordedHolderProcessTable,
        deadline: Option<std::time::Instant>,
    ) -> crate::broker::AbandonedSessionCloseOutcome {
        broker
            .try_close_merged_pull_request_session(
                session.id,
                now_ms(),
                2 * HOUR_MS,
                table,
                true,
                deadline,
            )
            .unwrap()
    }

    fn declined_as(
        reason: crate::broker::LandedSessionDecline,
    ) -> crate::broker::AbandonedSessionCloseOutcome {
        crate::broker::AbandonedSessionCloseOutcome::LandedDeclined(reason)
    }

    /// A cleanliness listing that never finishes is stopped at the session's
    /// budget: the session is kept as `proof_timed_out` and its streak
    /// starts. The listing blocks until released, so the outcome does not
    /// depend on how fast the runner is.
    #[test]
    fn a_stopped_proof_listing_keeps_the_session_as_timed_out() {
        let (owner, _storage, mut broker) = fixture("");
        let session = merged_quiet_session(&mut broker, "listing never finishes");
        let release = owner.path().join("release");
        let script = status_script(
            owner.path(),
            "blocking.sh",
            &format!(
                "while [ ! -e '{}' ]; do sleep 0.05; done",
                release.display()
            ),
        );
        let _seam = crate::auto_cleanup::proof_git_seam_for_test(Some(script), None);
        let _budget = crate::broker::landed_proof_session_budget_for_test(
            Some(session.id),
            Duration::from_millis(500),
        );
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let outcome = close_merged(
            &mut broker,
            &session,
            &mut table,
            Some(std::time::Instant::now() + Duration::from_secs(3_600)),
        );
        std::fs::write(&release, "").unwrap();

        assert_eq!(
            outcome,
            declined_as(crate::broker::LandedSessionDecline::ProofTimedOut)
        );
        assert!(is_open(&broker, session.id));
        assert_eq!(
            broker
                .landed_proof_timeout_streaks()
                .get(&session.id)
                .map(|(streak, _)| *streak),
            Some(1)
        );
    }

    /// A listing past its output limit is no verdict on the checkout: the
    /// session is kept (`worktree_unavailable`), not judged clean.
    #[test]
    fn an_overlong_proof_listing_keeps_the_session() {
        let (owner, _storage, mut broker) = fixture("");
        let session = merged_quiet_session(&mut broker, "listing too long");
        let script = status_script(
            owner.path(),
            "overlong.sh",
            "i=0; while [ $i -lt 64 ]; do printf '!! build-%04d/\\0' $i; i=$((i+1)); done",
        );
        let _seam = crate::auto_cleanup::proof_git_seam_for_test(Some(script), Some(64));
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let outcome = close_merged(&mut broker, &session, &mut table, None);

        assert_eq!(
            outcome,
            declined_as(crate::broker::LandedSessionDecline::WorktreeUnavailable)
        );
        assert!(is_open(&broker, session.id));
    }

    /// The last look under the claims hitting a failed listing keeps the
    /// session under its own name, not as work the agent did.
    #[test]
    fn a_failed_recheck_under_the_claims_keeps_the_session() {
        let (owner, _storage, mut broker) = fixture("");
        let session = merged_quiet_session(&mut broker, "re-check fails");
        let failing = status_script(owner.path(), "failing.sh", "echo refused >&2; exit 1");
        // Restores "no seam" at the end; the hook replaces it mid-close.
        let _restore = crate::auto_cleanup::proof_git_seam_for_test(None, None);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_before_landed_close_for_test(Box::new(move || {
            std::mem::forget(crate::auto_cleanup::proof_git_seam_for_test(
                Some(failing.clone()),
                None,
            ));
        }));
        let outcome = close_merged(&mut broker, &session, &mut table, None);

        assert_eq!(
            outcome,
            declined_as(crate::broker::LandedSessionDecline::WorktreeUnavailable)
        );
        assert!(is_open(&broker, session.id));
    }

    /// Each consecutive `proof_timed_out` doubles the session's budget, a
    /// conclusive answer ends the streak, an inconclusive one leaves it, and
    /// the session timing out longest is the one a pass grants its share to.
    #[test]
    fn timeout_streaks_double_the_budget_and_name_the_oldest() {
        use crate::broker::LandedSessionDecline as Decline;
        let base = Duration::from_secs(10);
        assert_eq!(crate::broker::doubled_proof_budget(base, 0), base);
        assert_eq!(
            crate::broker::doubled_proof_budget(base, 3),
            Duration::from_secs(80)
        );
        assert_eq!(
            crate::broker::doubled_proof_budget(base, 100),
            crate::broker::doubled_proof_budget(base, 16),
            "the doubling is bounded; the pass deadline bounds it first"
        );

        let (_owner, _storage, mut broker) = fixture("");
        let (first, _) = session(&mut broker, "first to time out", false);
        let (second, _) = session(&mut broker, "second to time out", false);
        let base_budget = broker.landed_proof_session_budget(first.id);
        broker.record_landed_proof_outcome(first.id, Some(Decline::ProofTimedOut));
        broker.record_landed_proof_outcome(second.id, Some(Decline::ProofTimedOut));
        broker.record_landed_proof_outcome(first.id, Some(Decline::ProofTimedOut));
        assert_eq!(
            broker.landed_proof_session_budget(first.id),
            base_budget * 4
        );
        assert_eq!(
            broker.landed_proof_session_budget(second.id),
            base_budget * 2
        );
        assert_eq!(
            broker.oldest_timed_out_session(&[first.id, second.id]),
            Some(first.id)
        );
        assert_eq!(
            broker.oldest_timed_out_session(&[second.id]),
            Some(second.id),
            "only sessions the pass may judge are granted"
        );

        for cheap in [
            Decline::Busy,
            Decline::RecentActivity,
            Decline::ProcessAlive,
            Decline::Changed,
            Decline::UpstreamMoved,
            Decline::LeaseHeld,
            Decline::BudgetExhausted,
        ] {
            broker.record_landed_proof_outcome(first.id, Some(cheap));
            assert_eq!(
                broker.landed_proof_session_budget(first.id),
                base_budget * 4,
                "{cheap:?}, decided before the proof ran, leaves the streak"
            );
        }
        broker.record_landed_proof_outcome(first.id, Some(Decline::NotLanded));
        assert_eq!(broker.landed_proof_session_budget(first.id), base_budget);
        broker.record_landed_proof_outcome(second.id, None);
        assert!(broker.landed_proof_timeout_streaks().is_empty());
        {
            let _grant = crate::broker::long_proof_grant(second.id);
            assert_eq!(broker.landed_proof_session_budget(second.id), Duration::MAX);
            assert_eq!(broker.landed_proof_session_budget(first.id), base_budget);
        }
        assert_eq!(broker.landed_proof_session_budget(second.id), base_budget);
    }

    /// The streaks stay bounded and current: ids of sessions no longer open
    /// are pruned, the oldest entry goes at the cap, the grant runs first
    /// every fourth pass, and a doubled budget never takes more than half of
    /// what is left of the pass.
    #[test]
    fn timeout_streaks_are_pruned_capped_and_paced() {
        use crate::broker::LandedSessionDecline as Decline;
        let (_owner, _storage, mut broker) = fixture("");
        let (open, _) = session(&mut broker, "still open", false);

        // At the cap, the oldest first-timeout is evicted, not the newest.
        let full = (0..4_096_i64)
            .map(|index| ((1_000_000 + index).to_string(), (1_u32, 1_000 + index)))
            .collect::<BTreeMap<_, _>>();
        broker
            .store_ref()
            .meta_set(
                crate::broker::LANDED_PROOF_TIMEOUTS_KEY,
                &serde_json::to_string(&full).unwrap(),
            )
            .unwrap();
        broker.record_landed_proof_outcome(open.id, Some(Decline::ProofTimedOut));
        let streaks = broker.landed_proof_timeout_streaks();
        assert_eq!(streaks.len(), 4_096);
        assert!(!streaks.contains_key(&1_000_000), "the oldest went");
        assert!(streaks.contains_key(&1_004_095));
        assert!(streaks.contains_key(&open.id), "the newest stayed");

        // Sessions no longer open are pruned with their unprovable marks.
        broker.mark_landed_proof_unprovable(1_000_001);
        broker.mark_landed_proof_unprovable(open.id);
        broker.prune_landed_proof_timeouts(&std::collections::BTreeSet::from([open.id]));
        assert_eq!(
            broker
                .landed_proof_timeout_streaks()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![open.id]
        );
        assert_eq!(
            broker.landed_proof_unprovable_sessions(),
            std::collections::BTreeSet::from([open.id])
        );
        broker.record_landed_proof_outcome(open.id, None);
        assert!(
            broker.landed_proof_unprovable_sessions().is_empty(),
            "a close forgets the mark"
        );

        // The grant runs first on every fourth pass that has one.
        let cadence = (0..8)
            .map(|_| broker.landed_proof_grant_goes_first())
            .collect::<Vec<_>>();
        assert_eq!(
            cadence,
            vec![false, false, false, true, false, false, false, true]
        );

        // A doubled budget is capped at half of what is left of the pass.
        for _ in 0..3 {
            broker.record_landed_proof_outcome(open.id, Some(Decline::ProofTimedOut));
        }
        let pass = std::time::Instant::now() + Duration::from_secs(20);
        let deadline = broker
            .landed_proof_session_deadline(open.id, Some(pass))
            .unwrap();
        assert!(
            deadline <= std::time::Instant::now() + Duration::from_secs(10),
            "80 s doubled, capped at half of the 20 s left"
        );
        assert_eq!(broker.landed_proof_session_deadline(open.id, None), None);
    }

    /// The unprovable advice names at most 20 sessions and then the total.
    #[test]
    fn unprovable_advice_names_twenty_sessions_and_the_total() {
        let few = (1..=3).collect::<std::collections::BTreeSet<i64>>();
        assert_eq!(
            crate::broker::unprovable_advice_evidence(&few),
            vec!["session 1", "session 2", "session 3"]
        );
        let many = (1..=25).collect::<std::collections::BTreeSet<i64>>();
        let evidence = crate::broker::unprovable_advice_evidence(&many);
        assert_eq!(evidence.len(), 21);
        assert_eq!(evidence[19], "session 20");
        assert_eq!(evidence[20], "5 more; 25 in total");
    }

    /// A session that runs out of time even when the grant runs first, with
    /// the whole share, is counted and shown in `broker status` by its
    /// session id only. Its listing blocks until released, so the outcome
    /// does not depend on how fast the runner is.
    #[test]
    fn a_session_unprovable_under_the_grant_is_reported() {
        let (owner, _storage, mut broker) = fixture("");
        let session = merged_quiet_session(&mut broker, "never provable");
        let worktree = session.worktree_path.clone();
        broker.record_landed_proof_outcome(
            session.id,
            Some(crate::broker::LandedSessionDecline::ProofTimedOut),
        );
        // The next pass with a grant is the fourth: the grant goes first.
        broker
            .store_ref()
            .meta_set("landed_lane.grant_passes", "3")
            .unwrap();
        let release = owner.path().join("release");
        let script = status_script(
            owner.path(),
            "blocking.sh",
            &format!(
                "while [ ! -e '{}' ]; do sleep 0.05; done",
                release.display()
            ),
        );
        let seam = crate::auto_cleanup::proof_git_seam_for_test(Some(script), None);
        let recorder = crate::phase_telemetry::Recorder::default();
        let report = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker
                .retire_abandoned_sessions_for_recovery(Some(
                    // The likely-landed queue gets half: well past the
                    // process-table margin, so the holder is checked and the
                    // proof itself is what runs out.
                    std::time::Instant::now() + Duration::from_secs(10),
                ))
                .unwrap()
        };
        std::fs::write(&release, "").unwrap();
        drop(seam);

        assert!(is_open(&broker, session.id), "{report:#?}");
        assert_eq!(phase_count(&recorder, "landed_proof_long_budget_first"), 1);
        assert_eq!(phase_count(&recorder, "landed_proof_unprovable"), 1);
        assert!(
            broker
                .landed_proof_unprovable_sessions()
                .contains(&session.id)
        );
        let view = broker.status(now_ms()).unwrap();
        let advice = view
            .advice
            .iter()
            .find(|advice| advice.id == "cleanup.landed-proof-unprovable")
            .expect("the unprovable session is shown");
        assert_eq!(advice.session_id, Some(session.id));
        assert_eq!(advice.evidence, vec![format!("session {}", session.id)]);
        let shown = serde_json::to_string(advice).unwrap();
        assert!(!shown.contains(&worktree), "no checkout path: {shown}");
    }

    /// A session whose proof always outlives its own budget is still closed:
    /// the next pass grants the session timing out longest the rest of the
    /// lane's share. The base budget is zero, so only the grant can prove it.
    #[test]
    fn the_oldest_timed_out_session_is_granted_the_share_and_closes() {
        let (_owner, _storage, mut broker) = fixture("");
        let session = merged_quiet_session(&mut broker, "always slow");
        let _budget = crate::broker::landed_proof_session_budget_for_test(None, Duration::ZERO);
        let pass = || Some(std::time::Instant::now() + Duration::from_secs(3_600));

        let first = broker
            .retire_abandoned_sessions_for_recovery(pass())
            .unwrap();
        assert_eq!(
            first.landed_sessions_declined.get("proof_timed_out"),
            Some(&1),
            "{first:#?}"
        );
        assert!(is_open(&broker, session.id));

        let recorder = crate::phase_telemetry::Recorder::default();
        let second = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker
                .retire_abandoned_sessions_for_recovery(pass())
                .unwrap()
        };
        assert_eq!(second.merged_pull_request_sessions_closed, 1, "{second:#?}");
        assert!(!is_open(&broker, session.id));
        assert_eq!(
            phase_count(&recorder, "landed_proof_long_budget_granted"),
            1
        );
        assert!(
            broker.landed_proof_timeout_streaks().is_empty(),
            "the close ends the streak"
        );
    }

    /// A session whose proof runs past its own budget is kept as
    /// `proof_timed_out`, and the likely-landed queue goes on to the next
    /// session in the same pass instead of stopping there: one slow checkout
    /// no longer spends the whole retirement share. The slow session's
    /// budget is zero, so nothing depends on how fast the runner is.
    #[test]
    fn a_proof_that_times_out_keeps_its_session_and_the_queue_moves_on() {
        let (_owner, _storage, mut broker) = fixture("");
        let (slow, slow_worktree) = session(&mut broker, "slow proof", true);
        link_pull_request(&mut broker, slow.id, 21, Some(1));
        quiet(&mut broker, &slow, 3);
        let (next, _) = session(&mut broker, "next in the queue", true);
        link_pull_request(&mut broker, next.id, 22, Some(1));
        quiet(&mut broker, &next, 3);
        assert!(slow.id < next.id, "the queue takes the slow session first");

        let _budget =
            crate::broker::landed_proof_session_budget_for_test(Some(slow.id), Duration::ZERO);
        let recorder = crate::phase_telemetry::Recorder::default();
        let report = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker
                .retire_abandoned_sessions_for_recovery(Some(
                    std::time::Instant::now() + Duration::from_secs(3_600),
                ))
                .unwrap()
        };

        assert_eq!(
            report.merged_pull_request_sessions_examined, 2,
            "{report:#?}"
        );
        assert_eq!(report.merged_pull_request_sessions_closed, 1, "{report:#?}");
        assert_eq!(
            report.landed_sessions_declined.get("proof_timed_out"),
            Some(&1),
            "{report:#?}"
        );
        assert!(
            !report
                .landed_sessions_declined
                .contains_key("budget_exhausted"),
            "{report:#?}"
        );
        assert!(is_open(&broker, slow.id), "an inconclusive proof keeps");
        assert!(slow_worktree.exists());
        assert!(!is_open(&broker, next.id), "the next session still closed");
        assert_eq!(
            phase_count(&recorder, "landed_sessions_declined_proof_timed_out"),
            1
        );
        assert_eq!(
            recorder.attachments()["phase_distinct_counts"]["landed_sessions_examined"],
            2
        );
    }

    /// Every condition the merged-pull-request window cannot prove keeps the
    /// session open with its checkout.
    #[test]
    fn the_merged_pull_request_window_keeps_what_it_cannot_prove() {
        type Setup = fn(&mut crate::Broker, &crate::Session, &Path);
        fn none(_: &mut crate::Broker, _: &crate::Session, _: &Path) {}
        fn unmerged_only(broker: &mut crate::Broker, session: &crate::Session, _: &Path) {
            broker
                .store()
                .connection()
                .execute("DELETE FROM pull_request_milestones", [])
                .unwrap();
            link_pull_request(broker, session.id, 12, None);
        }
        fn second_open(broker: &mut crate::Broker, session: &crate::Session, _: &Path) {
            link_pull_request(broker, session.id, 12, None);
        }
        fn commit_after_merge(_: &mut crate::Broker, _: &crate::Session, worktree: &Path) {
            std::fs::write(worktree.join("follow-up.txt"), "after the merge\n").unwrap();
            git(worktree, &["add", "follow-up.txt"]);
            git(worktree, &["commit", "-qm", "follow-up after the merge"]);
        }
        fn uncommitted(_: &mut crate::Broker, _: &crate::Session, worktree: &Path) {
            std::fs::write(worktree.join("base.txt"), "edited, not committed\n").unwrap();
        }
        fn live_pid(broker: &mut crate::Broker, session: &crate::Session, _: &Path) {
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET pid = ?1 WHERE id = ?2",
                    rusqlite::params![i64::from(std::process::id()), session.id],
                )
                .unwrap();
        }
        fn running_gate(broker: &mut crate::Broker, session: &crate::Session, _: &Path) {
            let run = crate::gates::running_dir(broker.main_root());
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(
                run.join(format!("{}-fixture.pid", session.id)),
                "unreadable",
            )
            .unwrap();
        }
        fn adopted(broker: &mut crate::Broker, session: &crate::Session, _: &Path) {
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET origin = 'adopted' WHERE id = ?1",
                    [session.id],
                )
                .unwrap();
        }
        fn merged_recently(broker: &mut crate::Broker, _: &crate::Session, _: &Path) {
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE pull_request_milestones SET merged_at = ?1 WHERE pr_number = 11",
                    [now_ms() - HOUR_MS / 2],
                )
                .unwrap();
        }
        // (case, retention override, quiet hours, setup)
        let cases: [(&str, &str, u64, Setup); 11] = [
            ("unmerged pull request", "", 3, unmerged_only),
            ("a second open pull request", "", 3, second_open),
            ("a commit after the merge", "", 3, commit_after_merge),
            ("an uncommitted change", "", 3, uncommitted),
            ("a live process", "", 3, live_pid),
            ("a running gate", "", 3, running_gate),
            ("recent activity", "", 1, none),
            ("a merge half an hour ago", "", 3, merged_recently),
            ("an adopted session", "", 3, adopted),
            (
                "the window disabled",
                "session_merged_after_hours = 0\n",
                3,
                none,
            ),
            ("retirement disabled", "", 3, none),
        ];
        for (case, retention, hours, setup) in cases {
            let (_owner, _storage, mut broker) = fixture(retention);
            // `session_abandoned_after_hours = 0` disables every automatic
            // close; the fixture's table already sets it, so it is rewritten.
            if case == "retirement disabled" {
                std::fs::write(
                    broker.main_root().join(".aethyme/broker.toml"),
                    "[retention]\nsession_abandoned_after_hours = 0\n",
                )
                .unwrap();
            }
            let (session, worktree) = session(&mut broker, case, true);
            link_pull_request(&mut broker, session.id, 11, Some(1));
            setup(&mut broker, &session, &worktree);
            quiet(&mut broker, &session, hours);

            let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

            assert!(is_open(&broker, session.id), "{case}: {report:#?}");
            assert_eq!(report.merged_pull_request_sessions_closed, 0, "{case}");
            assert!(worktree.exists(), "{case}: the checkout is kept");
            assert!(
                broker
                    .store_ref()
                    .latest_session_abandoned_event(session.id)
                    .unwrap()
                    .is_none(),
                "{case}"
            );
        }
    }

    /// A commit or a new file between the proof and the close keeps the
    /// session: both are read again immediately before the transition.
    #[test]
    fn work_between_the_proof_and_the_close_keeps_the_session() {
        for new_file_only in [false, true] {
            let (_owner, _storage, mut broker) = fixture("");
            let (session, worktree) = session(&mut broker, "late work", true);
            link_pull_request(&mut broker, session.id, 11, Some(1));
            quiet(&mut broker, &session, 3);
            let mut table = crate::session_holder::RecordedHolderProcessTable::default();
            let late = worktree.clone();
            table.install_before_landed_close_for_test(Box::new(move || {
                std::fs::write(late.join("late.txt"), "late work\n").unwrap();
                if !new_file_only {
                    git(&late, &["add", "late.txt"]);
                    git(&late, &["commit", "-qm", "late work"]);
                }
            }));

            let outcome = broker
                .try_close_merged_pull_request_session(
                    session.id,
                    now_ms(),
                    2 * HOUR_MS,
                    &mut table,
                    true,
                    None,
                )
                .unwrap();

            assert_eq!(
                outcome,
                crate::broker::AbandonedSessionCloseOutcome::LandedDeclined(
                    crate::broker::LandedSessionDecline::MovedBeforeClose
                ),
                "new_file_only={new_file_only}"
            );
            assert!(is_open(&broker, session.id));
        }
    }

    /// The abandonment window closes a session with no pull request and
    /// commits the default branch lacks. Closing deletes nothing: the event
    /// says the commits are unlanded, and every later removal pass keeps the
    /// checkout and the branch.
    #[test]
    fn an_inactive_session_without_a_pull_request_closes_and_keeps_unlanded_work() {
        let (_owner, _storage, mut broker) = fixture("closed_worktree_grace_hours = 0\n");
        let (session, worktree) = session(&mut broker, "unlanded, no pull request", false);
        let head = crate::git::GitRepo::discover(&worktree)
            .unwrap()
            .head_commit()
            .unwrap();
        quiet(&mut broker, &session, 73);

        let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

        assert_eq!(report.sessions_retired, 1, "{report:#?}");
        assert!(!is_open(&broker, session.id));
        let payload = close_payload(&broker, session.id);
        assert_eq!(
            payload["close_reason"],
            super::CLOSE_REASON_INACTIVE_NO_PULL_REQUEST
        );
        assert_eq!(payload["unlanded_commits"], true);
        assert_eq!(payload["head"], head.as_str());
        let notes = broker
            .store_ref()
            .unread_automation_notes(session.id)
            .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        let refusal = super::closed_session_refusal(broker.store_ref(), session.id);
        assert!(
            refusal.contains("no pull request was recorded"),
            "{refusal}"
        );

        for _ in 0..3 {
            broker.auto_cleanup_now(None).unwrap();
            assert!(worktree.exists(), "unlanded work keeps its checkout");
            assert_eq!(
                broker
                    .repo_handle()
                    .resolve_ref(&format!("refs/heads/{}", session.branch))
                    .as_deref(),
                Some(head.as_str()),
                "the session branch keeps the unlanded commit"
            );
        }
        let closed = broker.store_ref().session(session.id).unwrap();
        assert_eq!(
            closed.cleanup_state,
            SessionCleanupState::Closed,
            "never recorded as cleaned while its checkout is kept"
        );
    }

    /// A session already past the abandonment window is closed by that
    /// window first, without the landed proof: the proof used to run first
    /// for every such session and starve the lane.
    #[test]
    fn a_session_past_the_abandonment_window_closes_without_the_landed_proof() {
        let (_owner, _storage, mut broker) = fixture("");
        // Unlanded and without a pull request, so only the oldest-first lap
        // reaches it, where the landed proof used to be asked first.
        let (session, worktree) = session(&mut broker, "quiet for days", false);
        quiet(&mut broker, &session, 80);

        let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

        assert!(!is_open(&broker, session.id), "{report:#?}");
        assert_eq!(report.landed_sessions_examined, 0, "{report:#?}");
        assert_eq!(
            report.sessions_closed_by_reason.get("abandoned"),
            Some(&1),
            "{report:#?}"
        );
        assert!(worktree.exists());
    }

    /// A pass whose retirement lane judged nothing (a broker open's sliver,
    /// `gc plan`'s zero-budget pass) keeps the last counts `status` shows.
    #[test]
    fn a_report_without_retirement_keeps_the_last_retirement_counts() {
        let (_owner, _storage, mut broker) = fixture("");
        let mut worked = crate::AutoCleanupReport {
            evaluated_at_ms: 1_000,
            landed_sessions_examined: 4,
            landed_sessions_closed: 2,
            ..crate::AutoCleanupReport::default()
        };
        worked
            .sessions_closed_by_reason
            .insert(super::CLOSE_REASON_MERGED_PULL_REQUEST.into(), 2);
        broker.record_auto_cleanup_report(&worked).unwrap();
        broker
            .record_auto_cleanup_report(&crate::AutoCleanupReport {
                evaluated_at_ms: 2_000,
                ..crate::AutoCleanupReport::default()
            })
            .unwrap();

        let last = broker.last_auto_cleanup_report().unwrap();
        assert_eq!(last.evaluated_at_ms, 2_000);
        let view = last.landed_session_retirement_view().expect("counts kept");
        assert_eq!(view.examined, 4);
        assert_eq!(view.closed, 2);
        assert_eq!(
            view.evaluated_at_ms, 1_000,
            "the counts keep their own time"
        );
        assert_eq!(
            view.closed_by_reason
                .get(super::CLOSE_REASON_MERGED_PULL_REQUEST),
            Some(&2)
        );
    }

    /// The memo key reads a checkout's HEAD from its files, and agrees with
    /// Git, on a branch and detached.
    #[test]
    fn a_checkout_head_read_from_its_files_matches_git() {
        let (_owner, _storage, mut broker) = fixture("");
        let (_, worktree) = session(&mut broker, "head from files", false);
        let tips = broker.repo_handle().local_branch_tips().unwrap();
        let git_head = crate::git::GitRepo::discover(&worktree)
            .unwrap()
            .head_commit()
            .unwrap();
        assert_eq!(
            crate::auto_cleanup::head_from_checkout_files(&worktree, &tips).as_deref(),
            Some(git_head.as_str())
        );
        git(&worktree, &["checkout", "-q", "--detach"]);
        assert_eq!(
            crate::auto_cleanup::head_from_checkout_files(
                &worktree,
                &std::collections::BTreeMap::new()
            )
            .as_deref(),
            Some(git_head.as_str())
        );
    }

    /// One merged, landed, clean session quiet for three hours, closed or
    /// kept by a direct call with `table` as its process observation.
    fn merged_candidate(broker: &mut crate::Broker, label: &str) -> (crate::Session, PathBuf) {
        let (session, worktree) = session(broker, label, true);
        link_pull_request(broker, session.id, 1_000 + session.id, Some(1));
        (session, worktree)
    }

    fn close_directly(
        broker: &mut crate::Broker,
        session: &crate::Session,
        table: &mut crate::session_holder::RecordedHolderProcessTable,
    ) -> crate::broker::AbandonedSessionCloseOutcome {
        broker
            .try_close_merged_pull_request_session(
                session.id,
                now_ms(),
                2 * HOUR_MS,
                table,
                true,
                None,
            )
            .unwrap()
    }

    fn declined(
        reason: crate::broker::LandedSessionDecline,
    ) -> crate::broker::AbandonedSessionCloseOutcome {
        crate::broker::AbandonedSessionCloseOutcome::LandedDeclined(reason)
    }

    /// Where the session's preparation record lives, its directory created.
    fn preparation_record(broker: &crate::Broker, session: &crate::Session) -> PathBuf {
        let directory = broker.main_root().join(".aethyme/run/preparation");
        std::fs::create_dir_all(&directory).unwrap();
        directory.join(format!("session-{}.json", session.id))
    }

    fn write_preparation_record(broker: &crate::Broker, session: &crate::Session, state: &str) {
        write_preparation_record_started(broker, session, state, 1);
    }

    fn write_preparation_record_started(
        broker: &crate::Broker,
        session: &crate::Session,
        state: &str,
        started_at: i64,
    ) {
        std::fs::write(
            preparation_record(broker, session),
            serde_json::json!({
                "schema_version": 1,
                "session_id": session.id,
                "state": state,
                "digest": "fixture",
                "source_digest": "fixture",
                "started_at": started_at,
                "completed_at": null,
                "failed_step": null,
                "exit_code": null,
                "host_lease_id": null,
                "host_lease_generation": null,
            })
            .to_string(),
        )
        .unwrap();
    }

    /// A declaration whose input does not exist and whose output was never
    /// made: the full status reads `invalid`.
    fn declare_preparation(worktree: &Path) {
        std::fs::create_dir_all(worktree.join(".aethyme")).unwrap();
        std::fs::write(
            worktree.join(".aethyme/prepare.toml"),
            "schema_version = 1\n[[steps]]\nname = \"deps\"\ncommand = [\"true\"]\n\
             inputs = [\"absent.lock\"]\noutputs = [\"vendor/\"]\n",
        )
        .unwrap();
    }

    fn declare_text(worktree: &Path, text: &[u8]) {
        std::fs::create_dir_all(worktree.join(".aethyme")).unwrap();
        std::fs::write(worktree.join(".aethyme/prepare.toml"), text).unwrap();
    }

    /// The landed close asks the preparation record, not the prepared state:
    /// it closes with a finished, failed or absent record without hashing
    /// one declared input, even when the full status would be `invalid`.
    #[test]
    fn a_landed_close_hashes_no_preparation_input() {
        let (_owner, _storage, mut broker) = fixture("");
        for state in [None, Some("current"), Some("failed")] {
            let (session, worktree) = merged_candidate(&mut broker, &format!("prepared {state:?}"));
            declare_preparation(&worktree);
            if let Some(state) = state {
                write_preparation_record(&broker, &session, state);
            }
            quiet(&mut broker, &session, 3);
            let computed =
                || crate::preparation::SOURCE_DIGESTS_COMPUTED.with(std::cell::Cell::get);
            let before_status = computed();
            assert_eq!(
                broker.preparation_status(session.id).unwrap().state,
                crate::PreparationState::Invalid
            );
            assert!(computed() > before_status, "the counter sees a full status");

            let before = computed();
            let recorder = crate::phase_telemetry::Recorder::default();
            let outcome = {
                let _installed = crate::phase_telemetry::install(recorder.clone());
                let mut table = crate::session_holder::RecordedHolderProcessTable::default();
                close_directly(&mut broker, &session, &mut table)
            };

            assert_eq!(
                outcome,
                crate::broker::AbandonedSessionCloseOutcome::Closed,
                "{state:?}"
            );
            assert_eq!(computed(), before, "{state:?}");
            assert_eq!(phase_count(&recorder, "preparation_record_check"), 1);
            assert_eq!(phase_count(&recorder, "preparation_status"), 0);
        }
    }

    /// Bind a gone holder that entered the task-lease protocol, and age the
    /// session.
    fn quiet_with_task_lease(broker: &mut crate::Broker, session: &crate::Session, hours: u64) {
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "fixture-agent".into(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_task_lease_bound_payload(
                    &holder, "fixture", None,
                )),
            )
            .unwrap();
        age(broker, session, hours);
    }

    /// The Landed lane asks the record check when the holder has a task
    /// lease binding, and not at all for a legacy binding.
    #[test]
    fn the_landed_lane_asks_the_record_for_a_task_lease_holder_only() {
        use crate::broker::AbandonedSessionCloseOutcome as Outcome;
        for (label, task_lease, state, expected, asked) in [
            ("leased, ended", true, "current", Outcome::Closed, 1),
            (
                "leased, running",
                true,
                "in_progress",
                Outcome::OperationActive,
                1,
            ),
            ("legacy holder", false, "in_progress", Outcome::Closed, 0),
        ] {
            let (_owner, _storage, mut broker) = fixture("");
            let (session, worktree) = session(&mut broker, label, true);
            declare_preparation(&worktree);
            write_preparation_record(&broker, &session, state);
            if task_lease {
                quiet_with_task_lease(&mut broker, &session, 30);
            } else {
                quiet(&mut broker, &session, 30);
            }
            let computed =
                || crate::preparation::SOURCE_DIGESTS_COMPUTED.with(std::cell::Cell::get);
            let before = computed();
            let recorder = crate::phase_telemetry::Recorder::default();
            let outcome = {
                let _installed = crate::phase_telemetry::install(recorder.clone());
                let mut table = crate::session_holder::RecordedHolderProcessTable::default();
                broker
                    .try_close_landed_session(
                        session.id,
                        now_ms(),
                        24 * HOUR_MS,
                        &mut table,
                        true,
                        None,
                    )
                    .unwrap()
            };

            assert_eq!(outcome, expected, "{label}");
            assert_eq!(computed(), before, "{label}");
            assert_eq!(
                phase_count(&recorder, "preparation_record_check"),
                asked,
                "{label}"
            );
            assert_eq!(phase_count(&recorder, "preparation_status"), 0, "{label}");
        }
    }

    /// The abandonment and no-work lanes keep the full status: a missing
    /// declared input still keeps their session.
    #[test]
    fn the_abandonment_and_no_work_lanes_still_read_the_full_status() {
        use crate::broker::{AbandonedSessionCloseOutcome as Outcome, AbandonmentKeepReason};
        let (_owner, _storage, mut broker) = fixture("");
        crate::host_boot::set_boot_time_ms_for_tests(Some(now_ms() - HOUR_MS));
        let (abandoned, abandoned_tree) = session(&mut broker, "abandoned", false);
        declare_preparation(&abandoned_tree);
        quiet_with_task_lease(&mut broker, &abandoned, 80);
        let (idle, idle_tree) = no_work_session(&mut broker, "no work");
        declare_preparation(&idle_tree);
        quiet(&mut broker, &idle, 5);

        let mut judge = |no_work: bool, session: &crate::Session| {
            let recorder = crate::phase_telemetry::Recorder::default();
            let _installed = crate::phase_telemetry::install(recorder.clone());
            let mut table = crate::session_holder::RecordedHolderProcessTable::default();
            let outcome = if no_work {
                close_no_work_directly(&mut broker, session, &mut table)
            } else {
                close_abandoned_directly(&mut broker, session, &mut table)
            };
            (
                outcome,
                table.keep_reason(),
                phase_count(&recorder, "preparation_status"),
                phase_count(&recorder, "preparation_record_check"),
            )
        };
        let judged = [judge(false, &abandoned), judge(true, &idle)];
        crate::host_boot::set_boot_time_ms_for_tests(None);

        for judged in judged {
            assert_eq!(
                judged,
                (
                    Outcome::OperationEvidenceUnavailable,
                    Some(AbandonmentKeepReason::PreparationInvalid),
                    1,
                    0
                )
            );
        }
        assert!(is_open(&broker, abandoned.id));
        assert!(is_open(&broker, idle.id));
    }

    /// What the full status cannot use (a symlinked or unreadable declared
    /// input or output) says nothing about a running preparation: the
    /// landed lanes close over it.
    #[test]
    fn a_landed_close_ignores_unusable_inputs_and_outputs() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for case in [
            "symlinked input",
            "unreadable input",
            "symlinked output",
            "unreadable output",
        ] {
            let (_owner, _storage, mut broker) = fixture("");
            let (session, worktree) = merged_candidate(&mut broker, case);
            // Everything is under the ignored `.aethyme/`: the checkout stays
            // clean. `file/vendor` cannot be looked up: `file` is no
            // directory.
            let state = worktree.join(".aethyme");
            let (input, output) = match case {
                "symlinked input" | "unreadable input" => (".aethyme/lock", ".aethyme/vendor"),
                "symlinked output" => (".aethyme/file", ".aethyme/vendor"),
                _ => (".aethyme/file", ".aethyme/file/vendor"),
            };
            declare_text(
                &worktree,
                format!(
                    "schema_version = 1\n[[steps]]\nname = \"deps\"\ncommand = [\"true\"]\n\
                     inputs = [\"{input}\"]\noutputs = [\"{output}\"]\n"
                )
                .as_bytes(),
            );
            std::fs::write(state.join("file"), "file\n").unwrap();
            match case {
                "symlinked input" => symlink("file", state.join("lock")).unwrap(),
                "unreadable input" => std::fs::write(state.join("lock"), "lock\n").unwrap(),
                "symlinked output" => symlink("file", state.join("vendor")).unwrap(),
                _ => {}
            }
            quiet(&mut broker, &session, 3);
            if case == "unreadable input" {
                let lock = state.join("lock");
                std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0)).unwrap();
            }
            let status = broker.preparation_status(session.id);
            match case {
                // A root user reads a mode-0 file: the input is usable.
                "unreadable input" if std::fs::File::open(state.join("lock")).is_ok() => {}
                "symlinked input" | "unreadable input" => assert_eq!(
                    status.unwrap().state,
                    crate::PreparationState::Invalid,
                    "{case}"
                ),
                _ => assert!(status.is_err(), "{case}: {status:?}"),
            }
            let mut table = crate::session_holder::RecordedHolderProcessTable::default();

            assert_eq!(
                close_directly(&mut broker, &session, &mut table),
                crate::broker::AbandonedSessionCloseOutcome::Closed,
                "{case}"
            );
        }
    }

    /// An in-progress record without a host lease whose declaration was
    /// removed ends with the boot that wrote it: nothing else can prove its
    /// end. With a declaration, or started since the boot, or with the boot
    /// unknown, it keeps the session.
    #[test]
    fn a_lease_less_record_without_a_declaration_ends_with_its_boot() {
        let boot = now_ms() - HOUR_MS;
        for (label, boot, declared, started_at, closes) in [
            ("before the boot", Some(boot), false, 1, true),
            ("no start time", Some(boot), false, 0, false),
            ("negative start time", Some(boot), false, -1, false),
            ("since the boot", Some(boot), false, boot + 1, false),
            ("inside the margin", Some(boot), false, boot - 1, false),
            ("boot unknown", None, false, 1, false),
            ("declared", Some(boot), true, 1, false),
        ] {
            let (_owner, _storage, mut broker) = fixture("");
            let (session, worktree) = merged_candidate(&mut broker, label);
            if declared {
                declare_preparation(&worktree);
            }
            write_preparation_record_started(&broker, &session, "in_progress", started_at);
            quiet(&mut broker, &session, 3);
            crate::host_boot::set_boot_time_ms_for_tests(boot);
            let recorder = crate::phase_telemetry::Recorder::default();
            let installed = crate::phase_telemetry::install(recorder.clone());
            let mut table = crate::session_holder::RecordedHolderProcessTable::default();
            let outcome = close_directly(&mut broker, &session, &mut table);
            drop(installed);
            crate::host_boot::set_boot_time_ms_for_tests(None);

            assert_eq!(
                phase_count(&recorder, "preparation_pre_boot_without_lease"),
                u64::from(closes),
                "{label}"
            );

            assert_eq!(
                outcome == crate::broker::AbandonedSessionCloseOutcome::Closed,
                closes,
                "{label}: {outcome:?}"
            );
            assert_eq!(is_open(&broker, session.id), !closes, "{label}");
        }
    }

    /// A preparation that runs, or that cannot be ruled out, keeps a landed
    /// session: an in-progress record (with or without a declaration), and
    /// an unreadable or invalid declaration or record.
    #[test]
    fn a_landed_close_keeps_a_session_whose_preparation_may_run() {
        use crate::broker::{AbandonedSessionCloseOutcome as Outcome, AbandonmentKeepReason};
        type Setup = fn(&crate::Broker, &crate::Session, &Path);
        let cases: [(&str, Setup, Outcome, AbandonmentKeepReason); 6] = [
            (
                "in progress",
                |broker, session, worktree| {
                    declare_preparation(worktree);
                    write_preparation_record(broker, session, "in_progress");
                },
                Outcome::OperationActive,
                AbandonmentKeepReason::PreparationInProgress,
            ),
            (
                "in progress, declaration removed",
                |broker, session, _| write_preparation_record(broker, session, "in_progress"),
                Outcome::OperationActive,
                AbandonmentKeepReason::PreparationInProgress,
            ),
            (
                "invalid declaration",
                |_, _, worktree| declare_text(worktree, b"schema_version = 99\nsteps = []\n"),
                Outcome::OperationEvidenceUnavailable,
                AbandonmentKeepReason::PreparationInvalid,
            ),
            (
                "unreadable declaration",
                |_, _, worktree| declare_text(worktree, b"\xff\xfe"),
                Outcome::OperationEvidenceUnavailable,
                AbandonmentKeepReason::PreparationInvalid,
            ),
            (
                "invalid record",
                |broker, session, worktree| {
                    declare_preparation(worktree);
                    std::fs::write(preparation_record(broker, session), "{").unwrap();
                },
                Outcome::OperationEvidenceUnavailable,
                AbandonmentKeepReason::PreparationUnknown,
            ),
            (
                "unreadable record",
                |broker, session, worktree| {
                    declare_preparation(worktree);
                    std::fs::create_dir(preparation_record(broker, session)).unwrap();
                },
                Outcome::OperationEvidenceUnavailable,
                AbandonmentKeepReason::PreparationUnknown,
            ),
        ];
        for (label, setup, expected, reason) in cases {
            let (_owner, _storage, mut broker) = fixture("");
            let (session, worktree) = merged_candidate(&mut broker, label);
            setup(&broker, &session, &worktree);
            quiet(&mut broker, &session, 3);
            let mut table = crate::session_holder::RecordedHolderProcessTable::default();

            assert_eq!(
                close_directly(&mut broker, &session, &mut table),
                expected,
                "{label}"
            );
            assert_eq!(table.keep_reason(), Some(reason), "{label}");
            assert!(is_open(&broker, session.id), "{label}");
        }
    }

    /// Running work the merged lane checks for every holder: a submission
    /// in flight, an unresolved guarded command, a process working in the
    /// checkout (with a legacy holder binding, which has no task lease).
    #[test]
    fn the_merged_lane_keeps_sessions_with_running_work() {
        let (_owner, _storage, mut broker) = fixture("");
        let (queued, queued_tree) = merged_candidate(&mut broker, "submission in flight");
        let head = crate::git::GitRepo::discover(&queued_tree)
            .unwrap()
            .head_commit()
            .unwrap();
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO merge_queue (session_id, head_commit, base_commit, status, created_at, updated_at) VALUES (?1, ?2, ?2, 'submitted', 1, 1)",
                rusqlite::params![queued.id, head],
            )
            .unwrap();
        quiet(&mut broker, &queued, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &queued, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );

        let (guarded, _) = merged_candidate(&mut broker, "guarded command running");
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO guarded_exec_receipts (receipt_id, session_id, phase, command_outcome, ownership_audit, process_group_cleanup, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'running', 'unknown', 'not_run', 'unknown', 1, 1)",
                rusqlite::params!["0123456789abcdef0123456789abcdef", guarded.id],
            )
            .unwrap();
        quiet(&mut broker, &guarded, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &guarded, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );

        let (occupied, occupied_tree) = merged_candidate(&mut broker, "someone works here");
        quiet(&mut broker, &occupied, 3);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "editor".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&occupied_tree).unwrap(),
            }]),
        ));
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &occupied, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );
        for session in [&queued, &guarded, &occupied] {
            assert!(is_open(&broker, session.id), "{:?}", session.task);
        }
    }

    /// A live holder, and a session no holder was ever bound to, keep the
    /// session in the merged lane, decided before any landing proof.
    #[test]
    fn the_merged_lane_keeps_live_and_unbound_holders() {
        let (_owner, _storage, mut broker) = fixture("");
        let (alive, _) = merged_candidate(&mut broker, "holder still running");
        let holder = crate::session_holder::AgentProcess {
            pid: i64::from(std::process::id()),
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "fixture-agent".into(),
        };
        let event_id = bind_holder(&mut broker, &alive, &holder);
        age(&mut broker, &alive, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_holder_process_table_for_test(
            alive.id,
            event_id,
            crate::session_holder::ProcessTable::parse(&format!(
                "{} 1 {} {}\n",
                holder.pid, holder.started, holder.command
            )),
        );
        assert_eq!(
            close_directly(&mut broker, &alive, &mut table),
            declined(crate::broker::LandedSessionDecline::HolderAlive)
        );

        let (unbound, _) = merged_candidate(&mut broker, "no holder ever bound");
        age(&mut broker, &unbound, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &unbound, &mut table),
            declined(crate::broker::LandedSessionDecline::HolderUnbound)
        );
        assert!(
            broker
                .store_ref()
                .meta_get(&format!("landed_lane.landed.{}", unbound.id))
                .unwrap()
                .is_none(),
            "the holder is asked before the landing proof"
        );
        assert!(is_open(&broker, alive.id));
        assert!(is_open(&broker, unbound.id));
    }

    /// A pull request recorded for the session after the proof, before the
    /// close, keeps it: the pull requests are read again last.
    #[test]
    fn a_pull_request_recorded_during_the_close_keeps_the_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = merged_candidate(&mut broker, "new pull request meanwhile");
        quiet(&mut broker, &session, 3);
        let database = broker.store_ref().db_path().to_path_buf();
        let session_id = session.id;
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_before_landed_close_for_test(Box::new(move || {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute(
                    "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', 77, ?1, 1, 'watch')",
                    [session_id],
                )
                .unwrap();
        }));
        let outcome = close_directly(&mut broker, &session, &mut table);
        assert_ne!(outcome, crate::broker::AbandonedSessionCloseOutcome::Closed);
        assert!(is_open(&broker, session.id));
    }

    /// A follow-up task protects its session for the longer of the landed and
    /// abandonment windows (here 72 h) whatever the holder evidence, both for
    /// a reused open session and for a new session adopted on the broker's
    /// checkout after an automatic close; past that the adopted follow-up is
    /// closed by the merged lane like a spawned session.
    #[test]
    fn a_follow_up_task_is_protected_for_the_landed_window() {
        let (_owner, _storage, mut broker) = fixture("");
        let (reused, _) = merged_candidate(&mut broker, "reused for a follow-up");
        quiet(&mut broker, &reused, 3);
        broker
            .store()
            .append_event(crate::events::SESSION_REUSED, Some(reused.id), None)
            .unwrap();
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &reused, &mut table),
            declined(crate::broker::LandedSessionDecline::RecentActivity)
        );

        for (hours, closes) in [(3_u64, false), (30, false), (80, true)] {
            let (first, worktree) = merged_candidate(&mut broker, "first task");
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET status = 'cleaned', cleanup_state = 'closed', closed_at = 1 WHERE id = ?1",
                    [first.id],
                )
                .unwrap();
            let follow_up = broker.adopt(&worktree, Some("follow-up task")).unwrap();
            assert_eq!(follow_up.origin, crate::SessionOrigin::Adopted);
            link_pull_request(&mut broker, follow_up.id, 900 + hours as i64, Some(1));
            quiet(&mut broker, &follow_up, hours);

            let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

            assert_eq!(
                !is_open(&broker, follow_up.id),
                closes,
                "{hours} h: {report:#?}"
            );
            assert_eq!(
                report.merged_pull_request_sessions_closed,
                usize::from(closes),
                "{hours} h: {report:#?}"
            );
        }
    }

    fn opened_in(worktree: &Path) -> crate::reclaim::OpenFileSnapshot {
        crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
            pid: std::process::id(),
            command: "build".into(),
            descriptor: "cwd".into(),
            file_type: Some("DIR".into()),
            path: std::fs::canonicalize(worktree).unwrap(),
        }])
    }

    /// A process that starts in the checkout between the first open-file
    /// look and the final one keeps the session: the final look's
    /// observation, re-taken here, is checked like the first.
    #[test]
    fn work_started_between_the_two_open_file_looks_keeps_the_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, worktree) = merged_candidate(&mut broker, "build started late");
        quiet(&mut broker, &session, 3);
        crate::session_holder::queue_process_use_snapshots_for_tests(
            vec![
                crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
                opened_in(&worktree),
            ],
            true,
        );
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let outcome = close_directly(&mut broker, &session, &mut table);
        crate::session_holder::queue_process_use_snapshots_for_tests(Vec::new(), false);
        assert_eq!(
            outcome,
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );
        assert!(is_open(&broker, session.id));
    }

    /// Plant a remembered landing for an unlanded session with a merged pull
    /// request: `tip` and `fingerprint` as given.
    fn plant_landing_memo(
        broker: &mut crate::Broker,
        session: &crate::Session,
        tip: &str,
        fingerprint: &str,
    ) -> String {
        let head = crate::git::GitRepo::discover(Path::new(&session.worktree_path))
            .unwrap()
            .head_commit()
            .unwrap();
        broker
            .store_ref()
            .meta_set(
                &format!("landed_lane.landed.{}", session.id),
                &format!("{head} {tip} content {fingerprint}"),
            )
            .unwrap();
        head
    }

    /// The memo is a judging shortcut bound to its evidence: a changed
    /// pull-request fingerprint or a tip that is not an ancestor of the
    /// default branch voids it, and a close resting on a valid one searches
    /// again without it (here finding the work unlanded).
    #[test]
    fn a_remembered_landing_is_bound_to_its_evidence_and_never_closes_alone() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = session(&mut broker, "not on the default branch", false);
        link_pull_request(&mut broker, session.id, 41, Some(1));
        let (_, tip) = broker.repo_handle().upstream_default().unwrap();
        let fingerprint = super::PullRequestMerges::read(broker.store_ref(), session.id)
            .unwrap()
            .fingerprint();

        let head = plant_landing_memo(&mut broker, &session, &tip, "0000000000000000");
        assert_eq!(
            broker.landed_session_decline(&session, None).err(),
            Some(crate::broker::LandedSessionDecline::NotLanded),
            "a different pull-request fingerprint voids the memo"
        );
        broker
            .store_ref()
            .meta_set(&format!("landed_lane.not_landed.{}", session.id), "")
            .unwrap();
        plant_landing_memo(&mut broker, &session, &head, &fingerprint);
        assert_eq!(
            broker.landed_session_decline(&session, None).err(),
            Some(crate::broker::LandedSessionDecline::NotLanded),
            "a tip that is not an ancestor of the default branch voids the memo"
        );
        broker
            .store_ref()
            .meta_set(&format!("landed_lane.not_landed.{}", session.id), "")
            .unwrap();
        plant_landing_memo(&mut broker, &session, &tip, &fingerprint);
        let judged = broker.landed_session_decline(&session, None).unwrap();
        assert!(judged.memoized, "a valid memo answers the judgement");

        quiet(&mut broker, &session, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &session, &mut table),
            declined(crate::broker::LandedSessionDecline::NotLanded),
            "the close searched again without the memo"
        );
        assert!(is_open(&broker, session.id));
    }

    /// An adopted checkout the broker never spawned stays out of the merged
    /// lane.
    #[test]
    fn an_adopted_checkout_without_a_spawned_owner_is_refused() {
        let (owner, _storage, mut broker) = fixture("");
        let root = broker.main_root().to_path_buf();
        let worktree = owner.path().join("own-checkout");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/own",
                worktree.to_str().unwrap(),
                "main",
            ],
        );
        let session = broker
            .adopt(&worktree, Some("someone's own checkout"))
            .unwrap();
        link_pull_request(&mut broker, session.id, 51, Some(1));
        quiet(&mut broker, &session, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_directly(&mut broker, &session, &mut table),
            declined(crate::broker::LandedSessionDecline::Adopted)
        );
        assert!(is_open(&broker, session.id));
    }

    /// The likely-landed queue judges sessions with a merged pull request
    /// before branch-ancestry-only ones, whatever the rotation.
    #[test]
    fn the_likely_landed_queue_orders_merged_pull_requests_first() {
        let merged = std::collections::BTreeSet::from([30_i64]);
        let mut ids = vec![10_i64, 20, 30, 40];
        ids.sort_by_key(|id| crate::auto_cleanup::likely_landed_order(*id, &merged, 15));
        assert_eq!(ids, vec![30, 20, 40, 10]);
    }

    /// A failed process-table snapshot is retried at once; a second retry
    /// waits for the retry interval, so the holder stays unknown meanwhile.
    #[test]
    fn a_failed_process_table_is_retried_once_then_stays_unknown() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = session(&mut broker, "process table failed", false);
        quiet(&mut broker, &session, 3);
        let event_id = broker
            .store_ref()
            .latest_session_holder_event(session.id)
            .unwrap()
            .unwrap()
            .id;
        let mut retried = crate::session_holder::RecordedHolderProcessTable::default();
        retried.fail_process_table_for_test(session.id, event_id, true);
        assert_eq!(
            crate::session_holder::recorded_holder_liveness(
                broker.store_ref(),
                session.id,
                &mut retried,
                true
            )
            .liveness,
            crate::session_holder::RecordedHolderLiveness::Unknown
        );
        assert_eq!(retried.snapshots_taken_for_test(), 0, "no second retry");

        let mut first_failure = crate::session_holder::RecordedHolderProcessTable::default();
        first_failure.fail_process_table_for_test(session.id, event_id, false);
        crate::session_holder::recorded_holder_liveness(
            broker.store_ref(),
            session.id,
            &mut first_failure,
            true,
        );
        assert_eq!(first_failure.snapshots_taken_for_test(), 1, "one retry");
    }

    /// One failed snapshot does not leave the rest of the pass unknown: after
    /// the immediate retry, the process table is taken again once the retry
    /// interval has passed, up to the per-pass limit.
    #[test]
    fn a_failed_process_table_is_retried_again_after_the_interval_within_a_pass() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = session(&mut broker, "process table failed twice", false);
        quiet(&mut broker, &session, 80);
        let liveness = |table: &mut crate::session_holder::RecordedHolderProcessTable| {
            crate::session_holder::recorded_holder_liveness(
                broker.store_ref(),
                session.id,
                table,
                true,
            )
            .liveness
        };
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.queue_process_table_for_test(None);
        assert_eq!(
            liveness(&mut table),
            crate::session_holder::RecordedHolderLiveness::Unknown
        );
        table.queue_process_table_for_test(None);
        assert_eq!(
            liveness(&mut table),
            crate::session_holder::RecordedHolderLiveness::Unknown,
            "the immediate retry failed too"
        );
        assert_eq!(table.snapshots_taken_for_test(), 2);
        assert_eq!(
            liveness(&mut table),
            crate::session_holder::RecordedHolderLiveness::Unknown,
            "the next retry waits for the interval"
        );
        assert_eq!(table.snapshots_taken_for_test(), 2);

        table.age_process_table_failure_for_test(
            crate::session_holder::PROCESS_TABLE_RETRY_INTERVAL,
        );
        assert_eq!(
            liveness(&mut table),
            crate::session_holder::RecordedHolderLiveness::Gone,
            "a later retry within the same pass answers"
        );
        assert_eq!(table.snapshots_taken_for_test(), 3);

        // The per-pass limit still bounds the retries.
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        for _ in 0..=crate::session_holder::PROCESS_TABLE_MAX_RETRIES_PER_PASS {
            table.queue_process_table_for_test(None);
            liveness(&mut table);
            table.age_process_table_failure_for_test(
                crate::session_holder::PROCESS_TABLE_RETRY_INTERVAL,
            );
        }
        let taken = table.snapshots_taken_for_test();
        assert_eq!(
            taken,
            1 + crate::session_holder::PROCESS_TABLE_MAX_RETRIES_PER_PASS
        );
        assert_eq!(
            liveness(&mut table),
            crate::session_holder::RecordedHolderLiveness::Unknown
        );
        assert_eq!(
            table.snapshots_taken_for_test(),
            taken,
            "no retry past the limit"
        );
    }

    /// The agent process this test runs in, as a holder the synthetic process
    /// table below reports alive.
    fn live_holder() -> crate::session_holder::AgentProcess {
        crate::session_holder::AgentProcess {
            pid: i64::from(std::process::id()),
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "orchestrating-agent".into(),
        }
    }

    /// Bind `holder` to `session`, push its activity `hours` back, and give
    /// back a process table in which `holder` is alive.
    fn held_by_live_holder(
        broker: &mut crate::Broker,
        session: &crate::Session,
        holder: &crate::session_holder::AgentProcess,
        hours: u64,
    ) -> crate::session_holder::RecordedHolderProcessTable {
        let event_id = bind_holder(broker, session, holder);
        age(broker, session, hours);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_holder_process_table_for_test(
            session.id,
            event_id,
            crate::session_holder::ProcessTable::parse(&format!(
                "{} 1 {} {}\n",
                holder.pid, holder.started, holder.command
            )),
        );
        table
    }

    /// One orchestrating agent holds many sessions: once it was bound to a
    /// newer session after this one's last activity, a live holder no longer
    /// keeps this merged, landed, clean session.
    #[test]
    fn a_live_holder_that_moved_on_no_longer_keeps_a_merged_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let holder = live_holder();
        let (old, worktree) = merged_candidate(&mut broker, "finished long ago");
        let mut table = held_by_live_holder(&mut broker, &old, &holder, 3);
        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);

        assert_eq!(
            close_directly(&mut broker, &old, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        let payload = close_payload(&broker, old.id);
        assert_eq!(payload["holder_evidence"], "moved_on");
        assert_eq!(payload["holder_moved_on_to_session"], newer.id);
        assert!(worktree.exists(), "closing deletes nothing");
        let refusal = super::closed_session_refusal(broker.store_ref(), old.id);
        assert!(
            refusal.contains(&format!("moved on to session {}", newer.id)),
            "{refusal}"
        );
    }

    /// The holder's latest session is kept whatever its idle time, and a
    /// moved-on holder still keeps a session something is working in.
    #[test]
    fn a_live_holder_keeps_its_latest_session_and_any_session_in_use() {
        let (_owner, _storage, mut broker) = fixture("");
        let holder = live_holder();
        let (latest, _) = merged_candidate(&mut broker, "the agent's latest session");
        let mut table = held_by_live_holder(&mut broker, &latest, &holder, 30);
        assert_eq!(
            close_directly(&mut broker, &latest, &mut table),
            declined(crate::broker::LandedSessionDecline::HolderAlive)
        );
        assert_eq!(
            crate::broker::LandedSessionDecline::HolderAlive.code(),
            "holder_alive_current"
        );

        let (in_use, in_use_tree) = merged_candidate(&mut broker, "a process still works here");
        let mut table = held_by_live_holder(&mut broker, &in_use, &holder, 3);
        let (newer, _) = session(&mut broker, "moved on", false);
        bind_holder(&mut broker, &newer, &holder);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(opened_in(&in_use_tree)));
        assert_eq!(
            close_directly(&mut broker, &in_use, &mut table),
            declined(crate::broker::LandedSessionDecline::HolderAliveInUse)
        );
        assert!(is_open(&broker, latest.id));
        assert!(is_open(&broker, in_use.id));
    }

    fn close_abandoned_directly(
        broker: &mut crate::Broker,
        session: &crate::Session,
        table: &mut crate::session_holder::RecordedHolderProcessTable,
    ) -> crate::broker::AbandonedSessionCloseOutcome {
        broker
            .try_close_abandoned_session(session.id, now_ms(), 72 * HOUR_MS, table, true, None)
            .unwrap()
    }

    /// The abandonment window treats a moved-on live holder as no holder of
    /// the older session: unlanded commits do not keep a clean checkout, and
    /// they survive every later removal pass with their branch. (Uncommitted
    /// work waits for its own window: see the per-class tests.)
    #[test]
    fn the_abandonment_window_closes_a_moved_on_session_by_its_work() {
        let (_owner, _storage, mut broker) = fixture("closed_worktree_grace_hours = 0\n");
        let holder = live_holder();

        let (clean, clean_tree) = session(&mut broker, "clean with unlanded commits", false);
        let clean_head = crate::git::GitRepo::discover(&clean_tree)
            .unwrap()
            .head_commit()
            .unwrap();
        let mut clean_table = held_by_live_holder(&mut broker, &clean, &holder, 80);

        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);

        assert_eq!(
            close_abandoned_directly(&mut broker, &clean, &mut clean_table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        let payload = close_payload(&broker, clean.id);
        assert_eq!(
            payload["close_reason"],
            super::CLOSE_REASON_INACTIVE_NO_PULL_REQUEST
        );
        assert_eq!(payload["holder_evidence"], "moved_on");
        assert_eq!(payload["unlanded_commits"], true);
        for _ in 0..3 {
            broker.auto_cleanup_now(None).unwrap();
            assert!(clean_tree.exists(), "unlanded work keeps its checkout");
            assert_eq!(
                broker
                    .repo_handle()
                    .resolve_ref(&format!("refs/heads/{}", clean.branch))
                    .as_deref(),
                Some(clean_head.as_str())
            );
        }
    }

    /// Age every `session.holder_gone` event of `session` by `minutes`, as if
    /// the broker had first seen its holder gone that long ago.
    fn age_gone_event(broker: &mut crate::Broker, session: &crate::Session, minutes: i64) {
        broker
            .store()
            .connection()
            .execute(
                "UPDATE events SET ts = ts - ?1 WHERE session_id = ?2 AND kind = ?3",
                rusqlite::params![
                    minutes * 60_000,
                    session.id,
                    crate::events::SESSION_HOLDER_GONE
                ],
            )
            .unwrap();
    }

    /// The abandonment-window counters a command recorded, by name.
    fn kept_counters(recorder: &crate::phase_telemetry::Recorder) -> BTreeMap<String, u64> {
        recorder
            .totals()
            .1
            .into_iter()
            .filter(|(name, _)| name.starts_with("session_abandonment_kept_"))
            .collect()
    }

    /// A session quiet for days by its row but whose checkout Git metadata
    /// moved recently is kept for recent activity once; until that activity
    /// plus the window, later passes skip it instead of judging it again on
    /// every command, and it stays open.
    #[test]
    fn a_session_kept_for_recent_activity_is_not_judged_again_until_due() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, worktree) = session(&mut broker, "row quiet, checkout touched", false);
        quiet(&mut broker, &session, 80);
        let git_dir = crate::git::checked_worktree_git_dir(&worktree)
            .unwrap()
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(git_dir.join("index"))
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
        let counters = |recorder: &crate::phase_telemetry::Recorder| {
            let totals = recorder.totals().1;
            let get = |name: &str| totals.get(name).copied().unwrap_or(0);
            (
                get("session_abandonment_kept_recent_activity"),
                get("retirement_not_due_memo_recorded"),
                get("retirement_not_due_memo_skips"),
            )
        };

        let recorder = crate::phase_telemetry::Recorder::default();
        let installed = crate::phase_telemetry::install(recorder.clone());
        broker.retire_abandoned_sessions_for_recovery(None).unwrap();
        drop(installed);
        assert_eq!(counters(&recorder), (1, 1, 0));
        assert!(is_open(&broker, session.id));

        let recorder = crate::phase_telemetry::Recorder::default();
        let installed = crate::phase_telemetry::install(recorder.clone());
        broker.retire_abandoned_sessions_for_recovery(None).unwrap();
        drop(installed);
        assert_eq!(counters(&recorder), (0, 0, 1));
        assert!(is_open(&broker, session.id));
    }

    /// A gone holder's explicit leases hold the session only through the
    /// stale grace, which starts when the broker first sees the holder gone;
    /// past it the session closes and its leases are released with it.
    #[test]
    fn a_gone_holders_explicit_leases_stop_holding_after_the_stale_grace() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, worktree) = session(&mut broker, "explicit claims, holder gone", false);
        let lease = broker
            .store()
            .claim_lease(session.id, "base.txt", None)
            .unwrap();
        quiet(&mut broker, &session, 80);

        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::LeaseHeld,
            "the first sighting starts the grace"
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::LeaseHeld)
        );
        assert_eq!(
            kept_counters(&recorder),
            BTreeMap::from([("session_abandonment_kept_lease_held".to_string(), 1)])
        );
        assert!(is_open(&broker, session.id));

        age_gone_event(&mut broker, &session, 20);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        assert_eq!(table.keep_reason(), None);
        assert_eq!(
            close_payload(&broker, session.id)["holder_evidence"],
            "gone"
        );
        let unreleased: i64 = broker
            .store()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM leases WHERE id = ?1 AND released_at IS NULL",
                [lease.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unreleased, 0, "the close releases the explicit lease");
        assert!(worktree.exists(), "closing deletes nothing");
    }

    /// How often the command's metrics counted `name`.
    fn phase_count(recorder: &crate::phase_telemetry::Recorder, name: &str) -> u64 {
        recorder.totals().1.get(name).copied().unwrap_or(0)
    }

    /// Insert a guarded-exec receipt left in `phase`, last written at `at_ms`.
    fn left_receipt(
        broker: &mut crate::Broker,
        session_id: i64,
        receipt_id: &str,
        phase: &str,
        at_ms: i64,
    ) {
        let cleanup = if phase == "outcome_unknown" {
            "unverified"
        } else {
            "unknown"
        };
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO guarded_exec_receipts (receipt_id, session_id, phase, command_outcome, ownership_audit, process_group_cleanup, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, 'unknown', 'not_run', ?4, ?5, ?5)",
                rusqlite::params![receipt_id, session_id, phase, cleanup, at_ms],
            )
            .unwrap();
    }

    /// A guarded command recorded running before the host booted died with
    /// that boot: its receipt is settled as abandoned, with an event, and no
    /// longer keeps the session. One written after the boot, and an
    /// `outcome_unknown` one of any age, still keep theirs.
    #[test]
    fn only_a_pre_boot_running_receipt_stops_keeping_the_session() {
        let (_owner, _storage, mut broker) = fixture("");
        crate::host_boot::set_boot_time_ms_for_tests(Some(now_ms() - HOUR_MS));
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());

        let (died, _) = merged_candidate(&mut broker, "command died with the boot");
        left_receipt(
            &mut broker,
            died.id,
            "0123456789abcdef0123456789abcdef",
            "running",
            1,
        );
        quiet(&mut broker, &died, 3);
        let (running, _) = merged_candidate(&mut broker, "command running since the boot");
        left_receipt(
            &mut broker,
            running.id,
            "1123456789abcdef0123456789abcdef",
            "running",
            now_ms(),
        );
        quiet(&mut broker, &running, 3);
        let (ambiguous, _) = merged_candidate(&mut broker, "outcome unknown before the boot");
        left_receipt(
            &mut broker,
            ambiguous.id,
            "2123456789abcdef0123456789abcdef",
            "outcome_unknown",
            1,
        );
        quiet(&mut broker, &ambiguous, 3);
        // A receipt naming another boot is dead whatever its timestamp.
        crate::host_boot::set_boot_id_for_tests(Some("boot-now"));
        let (other_boot, _) = merged_candidate(&mut broker, "receipt from another boot");
        left_receipt(
            &mut broker,
            other_boot.id,
            "5123456789abcdef0123456789abcdef",
            "running",
            now_ms(),
        );
        quiet(&mut broker, &other_boot, 3);
        // A session kept for another reason keeps its dead receipt as it is.
        let (queued, queued_tree) = merged_candidate(&mut broker, "dead receipt, queued");
        left_receipt(
            &mut broker,
            queued.id,
            "6123456789abcdef0123456789abcdef",
            "running",
            1,
        );
        let head = crate::git::GitRepo::discover(&queued_tree)
            .unwrap()
            .head_commit()
            .unwrap();
        broker
            .store()
            .connection()
            .execute_batch(&format!(
                "UPDATE guarded_exec_receipts SET boot_id = 'boot-before'
                 WHERE receipt_id = '5123456789abcdef0123456789abcdef';
                 INSERT INTO merge_queue (session_id, head_commit, base_commit, status, created_at, updated_at)
                 VALUES ({}, '{head}', '{head}', 'submitted', 1, 1);",
                queued.id
            ))
            .unwrap();
        quiet(&mut broker, &queued, 3);

        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let other_boot_outcome = close_directly(&mut broker, &other_boot, &mut table);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let queued_outcome = close_directly(&mut broker, &queued, &mut table);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let died_outcome = close_directly(&mut broker, &died, &mut table);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let running_outcome = close_directly(&mut broker, &running, &mut table);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let ambiguous_outcome = close_directly(&mut broker, &ambiguous, &mut table);
        crate::host_boot::set_boot_time_ms_for_tests(None);
        crate::host_boot::set_boot_id_for_tests(None);

        assert_eq!(
            other_boot_outcome,
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        assert_eq!(
            queued_outcome,
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );
        let queued_phase: String = broker
            .store_ref()
            .connection()
            .query_row(
                "SELECT phase FROM guarded_exec_receipts WHERE receipt_id = ?1",
                ["6123456789abcdef0123456789abcdef"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            queued_phase, "running",
            "a session that stays open keeps its receipts untouched"
        );

        assert_eq!(
            died_outcome,
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        assert!(!is_open(&broker, died.id));
        let (phase, outcome): (String, String) = broker
            .store_ref()
            .connection()
            .query_row(
                "SELECT phase, command_outcome FROM guarded_exec_receipts WHERE receipt_id = ?1",
                ["0123456789abcdef0123456789abcdef"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((phase.as_str(), outcome.as_str()), ("completed", "unknown"));
        let abandoned: i64 = broker
            .store_ref()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = ?1 AND session_id = ?2",
                rusqlite::params![crate::events::GUARDED_EXEC_RECEIPT_ABANDONED, died.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(abandoned, 1);
        assert_eq!(
            phase_count(&recorder, "guarded_exec_receipts_abandoned_pre_boot"),
            2
        );

        for (session, outcome) in [(&running, running_outcome), (&ambiguous, ambiguous_outcome)] {
            assert_eq!(
                outcome,
                crate::broker::AbandonedSessionCloseOutcome::OperationActive,
                "{:?}",
                session.task
            );
            assert!(is_open(&broker, session.id));
        }
    }

    /// After a restart the broker first sees every holder gone only when a
    /// command runs. A holder whose recorded start predates the boot has
    /// been gone since the boot, so its stale grace does not start again.
    #[test]
    fn a_holder_started_before_the_boot_gets_no_fresh_grace() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = session(&mut broker, "explicit claims, holder died at reboot", false);
        broker
            .store()
            .claim_lease(session.id, "base.txt", None)
            .unwrap();
        // `quiet` binds a holder started on 1 January 2024.
        quiet(&mut broker, &session, 80);
        crate::host_boot::set_boot_time_ms_for_tests(Some(now_ms() - HOUR_MS));
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());

        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let outcome = close_abandoned_directly(&mut broker, &session, &mut table);
        crate::host_boot::set_boot_time_ms_for_tests(None);

        assert_eq!(
            outcome,
            crate::broker::AbandonedSessionCloseOutcome::Closed,
            "the grace ran from the boot an hour ago, not from this first sighting"
        );
        assert!(phase_count(&recorder, "holder_grace_anchored_to_boot") >= 1);
    }

    /// A legacy session (created before holders were recorded, with no
    /// holder event) releases its explicit claims only past the abandonment
    /// window and with no unresolved operation; inside the window, or with a
    /// command still unresolved, its claims keep it. A session created since
    /// then that merely has no holder record (an unidentified caller) keeps
    /// its claims whatever its age.
    #[test]
    fn a_legacy_session_without_a_holder_releases_claims_only_when_old_and_idle() {
        let (_owner, _storage, mut broker) = fixture("");
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        // A day before holder recording shipped (2026-10-05T00:00:00Z).
        let before_holder_recording = 1_791_158_400_000 - 24 * HOUR_MS;
        let claimed = |broker: &mut crate::Broker, label: &str, hours: u64, legacy: bool| {
            let (session, _) = session(broker, label, false);
            broker
                .store()
                .claim_lease(session.id, "base.txt", None)
                .unwrap();
            age(broker, &session, hours);
            if legacy {
                broker
                    .store()
                    .connection()
                    .execute(
                        "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                        rusqlite::params![before_holder_recording, session.id],
                    )
                    .unwrap();
            }
            session
        };

        let recent = claimed(&mut broker, "legacy, inside the window", 1, true);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &recent, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::NotDue
        );
        assert!(is_open(&broker, recent.id));

        let unidentified = claimed(&mut broker, "no holder, created recently", 80, false);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &unidentified, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::LeaseHeld,
            "an unidentified caller's session is not legacy"
        );
        assert!(is_open(&broker, unidentified.id));

        let busy = claimed(&mut broker, "legacy, command still running", 80, true);
        left_receipt(
            &mut broker,
            busy.id,
            "3123456789abcdef0123456789abcdef",
            "running",
            now_ms(),
        );
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &busy, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::LeaseHeld
        );
        assert!(is_open(&broker, busy.id));
        assert_eq!(phase_count(&recorder, "legacy_unbound_claims_released"), 0);

        let old = claimed(&mut broker, "legacy, quiet for days", 80, true);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &old, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed,
            "age is the only evidence a legacy session can have"
        );
        assert!(!is_open(&broker, old.id));
        assert_eq!(phase_count(&recorder, "legacy_unbound_claims_released"), 1);
    }

    /// A legacy session whose checkout a process still uses keeps its claim.
    #[test]
    fn a_legacy_sessions_claim_is_kept_while_a_process_uses_its_checkout() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, worktree) = session(&mut broker, "legacy, editor open", false);
        broker
            .store()
            .claim_lease(session.id, "base.txt", None)
            .unwrap();
        age(&mut broker, &session, 80);
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![1_791_158_400_000_i64 - 24 * HOUR_MS, session.id],
            )
            .unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "editor".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&worktree).unwrap(),
            }]),
        ));
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let outcome = close_abandoned_directly(&mut broker, &session, &mut table);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        assert_ne!(outcome, crate::broker::AbandonedSessionCloseOutcome::Closed);
        assert!(is_open(&broker, session.id));
    }

    /// A command refused the process listing (sandboxed) judges no holder:
    /// the session is kept under its own reason, not as an unknown holder,
    /// and the pass does not retry the listing.
    #[test]
    fn a_refused_process_listing_is_its_own_keep_reason() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = session(&mut broker, "holder hidden by a sandbox", false);
        quiet(&mut broker, &session, 80);
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.queue_denied_process_table_for_test();

        assert_eq!(
            close_abandoned_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::HolderUnavailable
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::HolderProcessTableDenied)
        );
        assert!(table.process_listing_denied());
        assert_eq!(phase_count(&recorder, "holder_process_table_denied"), 1);
        assert_eq!(
            phase_count(&recorder, "session_abandonment_kept_holder_unknown"),
            0
        );
        assert!(is_open(&broker, session.id));
    }

    /// A session started but never committed to: HEAD is its diff base.
    fn no_work_session(broker: &mut crate::Broker, label: &str) -> (crate::Session, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        (session, worktree)
    }

    fn close_no_work_directly(
        broker: &mut crate::Broker,
        session: &crate::Session,
        table: &mut crate::session_holder::RecordedHolderProcessTable,
    ) -> crate::broker::AbandonedSessionCloseOutcome {
        broker
            .try_close_no_work_session(session.id, now_ms(), 4 * HOUR_MS, table, true, None)
            .unwrap()
    }

    /// A no-work session whose holder died with the previous boot (an hour
    /// ago, so past the stale grace), quiet for five hours, closes by the
    /// no-work window with its own close reason and counter; quiet for three
    /// hours it is not due yet. Without the lane both stay open until 72 h.
    #[test]
    fn a_session_that_never_worked_closes_after_four_quiet_hours() {
        let (_owner, _storage, mut broker) = fixture("");
        crate::host_boot::set_boot_time_ms_for_tests(Some(now_ms() - HOUR_MS));
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());

        let (recent, _) = no_work_session(&mut broker, "no work, three quiet hours");
        quiet(&mut broker, &recent, 3);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let recent_outcome = close_no_work_directly(&mut broker, &recent, &mut table);

        let (idle, worktree) = no_work_session(&mut broker, "no work, five quiet hours");
        quiet(&mut broker, &idle, 5);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let idle_outcome = close_no_work_directly(&mut broker, &idle, &mut table);

        let (by_status, _) = no_work_session(&mut broker, "no work, closed by a status read");
        quiet(&mut broker, &by_status, 5);
        broker.agents(now_ms()).unwrap();
        crate::host_boot::set_boot_time_ms_for_tests(None);

        assert_eq!(
            recent_outcome,
            crate::broker::AbandonedSessionCloseOutcome::NotDue
        );
        assert!(is_open(&broker, recent.id));
        assert_eq!(
            idle_outcome,
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        assert!(!is_open(&broker, idle.id));
        assert_eq!(
            close_payload(&broker, idle.id)["close_reason"],
            super::CLOSE_REASON_INACTIVE_NO_WORK
        );
        assert!(worktree.exists(), "closing deletes nothing");
        assert!(
            !is_open(&broker, by_status.id),
            "the foreground pass judges a no-work session at four hours"
        );
        assert_eq!(phase_count(&recorder, "session_closed_no_work"), 2);
        assert_eq!(
            phase_count(&recorder, "session_no_work_kept_recent_activity"),
            1
        );
    }

    /// At five quiet hours, each single failing condition keeps the session
    /// and is counted under its own reason.
    #[test]
    fn a_session_failing_one_no_work_condition_is_kept() {
        use crate::broker::AbandonmentKeepReason as Reason;
        let (_owner, _storage, mut broker) = fixture("");
        crate::host_boot::set_boot_time_ms_for_tests(Some(now_ms() - HOUR_MS));
        let mut kept = Vec::new();
        let mut judge = |broker: &mut crate::Broker,
                         session: &crate::Session,
                         mut table: crate::session_holder::RecordedHolderProcessTable,
                         expected: Reason| {
            let outcome = close_no_work_directly(broker, session, &mut table);
            kept.push((
                session.task.clone(),
                outcome,
                table.keep_reason(),
                expected,
                is_open(broker, session.id),
            ));
        };

        let (committed, _) = session(&mut broker, "one commit ahead", false);
        quiet(&mut broker, &committed, 5);
        judge(
            &mut broker,
            &committed,
            Default::default(),
            Reason::NoWorkSessionCommits,
        );

        let (dirty, dirty_tree) = no_work_session(&mut broker, "untracked file");
        std::fs::write(dirty_tree.join("draft.txt"), "draft\n").unwrap();
        quiet(&mut broker, &dirty, 5);
        judge(
            &mut broker,
            &dirty,
            Default::default(),
            Reason::NoWorkWorktreeDirty,
        );

        let (alive, _) = no_work_session(&mut broker, "holder still running");
        let table = held_by_live_holder(&mut broker, &alive, &live_holder(), 5);
        judge(&mut broker, &alive, table, Reason::NoWorkHolderAlive);

        let (unbound, _) = no_work_session(&mut broker, "no holder ever recorded");
        age(&mut broker, &unbound, 5);
        judge(
            &mut broker,
            &unbound,
            Default::default(),
            Reason::NoWorkHolderUnbound,
        );

        let (open_pr, _) = no_work_session(&mut broker, "pull request open");
        link_pull_request(&mut broker, open_pr.id, 21, None);
        quiet(&mut broker, &open_pr, 5);
        judge(
            &mut broker,
            &open_pr,
            Default::default(),
            Reason::NoWorkPullRequestOpen,
        );

        let (claimed, _) = no_work_session(&mut broker, "explicit claim");
        broker
            .store()
            .claim_lease(claimed.id, "base.txt", None)
            .unwrap();
        quiet(&mut broker, &claimed, 5);
        judge(
            &mut broker,
            &claimed,
            Default::default(),
            Reason::NoWorkExplicitClaim,
        );

        let (running, _) = no_work_session(&mut broker, "guarded command running");
        left_receipt(
            &mut broker,
            running.id,
            "4123456789abcdef0123456789abcdef",
            "running",
            now_ms(),
        );
        quiet(&mut broker, &running, 5);
        judge(
            &mut broker,
            &running,
            Default::default(),
            Reason::OperationUnresolved,
        );

        let (preparing, preparing_tree) = no_work_session(&mut broker, "preparation running");
        std::fs::create_dir_all(preparing_tree.join(".aethyme")).unwrap();
        std::fs::write(
            preparing_tree.join(".aethyme/prepare.toml"),
            "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\ninputs = []\noutputs = [\"out/\"]\nrequired_for_hooks = false\n",
        )
        .unwrap();
        let preparation_dir = broker.main_root().join(".aethyme/run/preparation");
        std::fs::create_dir_all(&preparation_dir).unwrap();
        std::fs::write(
            preparation_dir.join(format!("session-{}.json", preparing.id)),
            serde_json::json!({
                "schema_version": 1,
                "session_id": preparing.id,
                "state": "in_progress",
                "digest": "fixture-digest",
                "source_digest": "fixture-source",
                "started_at": now_ms(),
                "completed_at": null,
                "failed_step": null,
                "exit_code": null,
                "host_lease_id": null,
                "host_lease_generation": null,
            })
            .to_string(),
        )
        .unwrap();
        quiet(&mut broker, &preparing, 5);
        judge(
            &mut broker,
            &preparing,
            Default::default(),
            Reason::PreparationInProgress,
        );

        let (adopted, _) = no_work_session(&mut broker, "adopted session");
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET origin = 'adopted' WHERE id = ?1",
                [adopted.id],
            )
            .unwrap();
        quiet(&mut broker, &adopted, 5);
        judge(
            &mut broker,
            &adopted,
            Default::default(),
            Reason::NoWorkAdopted,
        );

        let (detached, detached_tree) = no_work_session(&mut broker, "detached HEAD");
        git(&detached_tree, &["checkout", "-q", "--detach"]);
        quiet(&mut broker, &detached, 5);
        judge(
            &mut broker,
            &detached,
            Default::default(),
            Reason::NoWorkSessionCommits,
        );

        // The agent committed on the session branch, then switched to a new
        // branch at the base: HEAD equals the base, the session branch does not.
        let (ahead, ahead_tree) = session(&mut broker, "branch ahead, HEAD on base", false);
        let base = ahead
            .diff_base
            .clone()
            .expect("a spawned session records its base");
        git(&ahead_tree, &["checkout", "-q", "-b", "scratch", &base]);
        quiet(&mut broker, &ahead, 5);
        judge(
            &mut broker,
            &ahead,
            Default::default(),
            Reason::NoWorkSessionCommits,
        );

        let (queued, queued_tree) = no_work_session(&mut broker, "merge-queue entry");
        let head = crate::git::GitRepo::discover(&queued_tree)
            .unwrap()
            .head_commit()
            .unwrap();
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO merge_queue (session_id, head_commit, base_commit, status, created_at, updated_at) VALUES (?1, ?2, ?2, 'rejected', 1, 1)",
                rusqlite::params![queued.id, head],
            )
            .unwrap();
        quiet(&mut broker, &queued, 5);
        judge(
            &mut broker,
            &queued,
            Default::default(),
            Reason::NoWorkMergeQueue,
        );

        // Unreadable pull-request state does not qualify: the table the
        // links are read from is gone.
        let (unknown_pr, _) = no_work_session(&mut broker, "pull request state unknown");
        quiet(&mut broker, &unknown_pr, 5);
        broker
            .store()
            .connection()
            .execute_batch("DROP TABLE pull_request_milestones")
            .unwrap();
        judge(
            &mut broker,
            &unknown_pr,
            Default::default(),
            Reason::NoWorkPullRequestUnknown,
        );
        crate::host_boot::set_boot_time_ms_for_tests(None);

        for (task, outcome, reason, expected, open) in kept {
            assert_ne!(
                outcome,
                crate::broker::AbandonedSessionCloseOutcome::Closed,
                "{task:?}"
            );
            assert_eq!(reason, Some(expected), "{task:?}");
            assert!(open, "{task:?}");
        }
    }

    /// A holder seen gone for the first time starts the stale grace: the
    /// no-work lane waits for it, then closes.
    #[test]
    fn the_no_work_lane_waits_for_the_holders_stale_grace() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = no_work_session(&mut broker, "holder just went away");
        quiet(&mut broker, &session, 5);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_no_work_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::NotDue
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::NoWorkHolderGoneRecently)
        );
        age_gone_event(&mut broker, &session, 20);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_no_work_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
    }

    /// `0` disables the no-work lane, and it never exceeds the main window.
    #[test]
    fn the_no_work_window_is_disabled_by_zero_and_clamped_to_the_main_window() {
        let (_owner, _storage, broker) = fixture("");
        assert_eq!(broker.no_work_window_ms(), 4 * HOUR_MS);
        let (_owner, _storage, broker) = fixture("session_no_work_abandoned_after_hours = 0\n");
        assert_eq!(broker.no_work_window_ms(), 0);
        let (_owner, _storage, broker) = fixture("session_no_work_abandoned_after_hours = 100\n");
        assert_eq!(broker.no_work_window_ms(), 72 * HOUR_MS);

        let (_owner, _storage, mut broker) = fixture("session_no_work_abandoned_after_hours = 0\n");
        crate::host_boot::set_boot_time_ms_for_tests(Some(now_ms() - HOUR_MS));
        let (session, _) = no_work_session(&mut broker, "lane disabled");
        quiet(&mut broker, &session, 5);
        broker.agents(now_ms()).unwrap();
        crate::host_boot::set_boot_time_ms_for_tests(None);
        assert!(
            is_open(&broker, session.id),
            "a disabled lane closes nothing"
        );
    }

    /// An explicit lease whose holder runs keeps the session, even when the
    /// holder moved on and the checkout is clean, which alone would close it.
    #[test]
    fn a_live_holders_explicit_lease_still_keeps_the_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let holder = live_holder();
        let (claimed, _) = session(&mut broker, "explicit claim, holder alive", false);
        broker
            .store()
            .claim_lease(claimed.id, "base.txt", None)
            .unwrap();
        let mut table = held_by_live_holder(&mut broker, &claimed, &holder, 80);
        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);

        assert_eq!(
            close_abandoned_directly(&mut broker, &claimed, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::LeaseHeld
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::LeaseHeld)
        );
        assert!(is_open(&broker, claimed.id));
    }

    /// Each abandonment-window keep is counted under its own reason, and the
    /// live-process keep is told apart by cause.
    #[test]
    fn every_abandonment_keep_is_counted_under_its_reason() {
        use crate::broker::AbandonmentKeepReason as Reason;
        let (_owner, _storage, mut broker) = fixture("");
        let holder = live_holder();
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        // Counted as a delta: session setup may judge other sessions too.
        let count = |reason: Reason| {
            kept_counters(&recorder)
                .get(reason.telemetry_name())
                .copied()
                .unwrap_or(0)
        };
        let kept = |broker: &mut crate::Broker,
                    session: &crate::Session,
                    table: &mut crate::session_holder::RecordedHolderProcessTable,
                    reason: Reason| {
            let before = count(reason);
            assert_ne!(
                close_abandoned_directly(broker, session, table),
                crate::broker::AbandonedSessionCloseOutcome::Closed,
                "{reason:?}"
            );
            assert_eq!(table.keep_reason(), Some(reason));
            assert_eq!(count(reason), before + 1, "{reason:?}");
        };

        // Recent activity.
        let (recent, _) = session(&mut broker, "worked an hour ago", false);
        quiet(&mut broker, &recent, 1);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        kept(&mut broker, &recent, &mut table, Reason::RecentActivity);

        // The holder runs and this is its latest session.
        let (current, _) = session(&mut broker, "the holder's latest session", false);
        let mut table = held_by_live_holder(&mut broker, &current, &holder, 80);
        kept(
            &mut broker,
            &current,
            &mut table,
            Reason::HolderAliveCurrent,
        );

        // The holder could not be checked.
        let (unknown, _) = session(&mut broker, "process table failed", false);
        quiet(&mut broker, &unknown, 80);
        let event_id = broker
            .store_ref()
            .latest_session_holder_event(unknown.id)
            .unwrap()
            .unwrap()
            .id;
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.fail_process_table_for_test(unknown.id, event_id, true);
        kept(&mut broker, &unknown, &mut table, Reason::HolderUnknown);

        // The holder moved on, but a process works in the checkout.
        let (in_use, in_use_tree) = session(&mut broker, "moved on, in use", false);
        let mut in_use_table = held_by_live_holder(&mut broker, &in_use, &holder, 80);
        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(opened_in(&in_use_tree)));
        kept(
            &mut broker,
            &in_use,
            &mut in_use_table,
            Reason::OpenFilesInCheckout,
        );
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));

        // The window is disabled.
        let before = count(Reason::WindowDisabled);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            broker
                .try_close_abandoned_session(recent.id, now_ms(), 0, &mut table, true, None)
                .unwrap(),
            crate::broker::AbandonedSessionCloseOutcome::NotDue
        );
        assert_eq!(table.keep_reason(), Some(Reason::WindowDisabled));
        assert_eq!(count(Reason::WindowDisabled), before + 1);
    }

    /// Every keep reason has its own counter, named after its code.
    #[test]
    fn every_abandonment_keep_reason_names_its_counter_after_its_code() {
        use crate::broker::AbandonmentKeepReason as Reason;
        let every = [
            Reason::WindowDisabled,
            Reason::Changed,
            Reason::SessionPidAlive,
            Reason::FollowUpProtected,
            Reason::WorktreeActivityUnavailable,
            Reason::RecentActivity,
            Reason::WorkClassNotDue,
            Reason::HolderUnknown,
            Reason::HolderProcessTableDenied,
            Reason::HolderAliveCurrent,
            Reason::HolderRecordUnavailable,
            Reason::HolderSharedService,
            Reason::TaskLeaseCurrent,
            Reason::OperationGuardBusy,
            Reason::OperationGuardUnavailable,
            Reason::StartReserved,
            Reason::LifecycleBusy,
            Reason::DeadlineReached,
            Reason::CloseClaimBudget,
            Reason::WorkingTreeNotExamined,
            Reason::GateRunning,
            Reason::GatesUnknown,
            Reason::OperationUnresolved,
            Reason::OperationsUnknown,
            Reason::MergeQueueActive,
            Reason::MergeQueueUnknown,
            Reason::PreparationInProgress,
            Reason::PreparationInvalid,
            Reason::PreparationUnknown,
            Reason::CheckoutNotDirectory,
            Reason::ArtifactProducerActive,
            Reason::ArtifactProtocolUnavailable,
            Reason::LegacyArtifactMarkers,
            Reason::OpenFilesInCheckout,
            Reason::OpenFilesUnavailable,
            Reason::OpenFilesThrottled,
            Reason::LeaseHeld,
            Reason::LeaseUnavailable,
            Reason::DatabaseBusy,
            Reason::PolicyUnavailable,
            Reason::LandedDeclined,
            Reason::NoWorkAdopted,
            Reason::NoWorkSessionCommits,
            Reason::NoWorkWorktreeDirty,
            Reason::NoWorkHolderUnbound,
            Reason::NoWorkHolderAlive,
            Reason::NoWorkHolderGoneRecently,
            Reason::NoWorkExplicitClaim,
            Reason::NoWorkMergeQueue,
            Reason::NoWorkPullRequestOpen,
            Reason::NoWorkPullRequestUnknown,
            Reason::Unrecorded,
        ];
        // A new reason must be added to `every` above.
        for reason in every {
            match reason {
                Reason::WindowDisabled
                | Reason::Changed
                | Reason::SessionPidAlive
                | Reason::FollowUpProtected
                | Reason::WorktreeActivityUnavailable
                | Reason::RecentActivity
                | Reason::WorkClassNotDue
                | Reason::HolderUnknown
                | Reason::HolderProcessTableDenied
                | Reason::HolderAliveCurrent
                | Reason::HolderRecordUnavailable
                | Reason::HolderSharedService
                | Reason::TaskLeaseCurrent
                | Reason::OperationGuardBusy
                | Reason::OperationGuardUnavailable
                | Reason::StartReserved
                | Reason::LifecycleBusy
                | Reason::DeadlineReached
                | Reason::CloseClaimBudget
                | Reason::WorkingTreeNotExamined
                | Reason::GateRunning
                | Reason::GatesUnknown
                | Reason::OperationUnresolved
                | Reason::OperationsUnknown
                | Reason::MergeQueueActive
                | Reason::MergeQueueUnknown
                | Reason::PreparationInProgress
                | Reason::PreparationInvalid
                | Reason::PreparationUnknown
                | Reason::CheckoutNotDirectory
                | Reason::ArtifactProducerActive
                | Reason::ArtifactProtocolUnavailable
                | Reason::LegacyArtifactMarkers
                | Reason::OpenFilesInCheckout
                | Reason::OpenFilesUnavailable
                | Reason::OpenFilesThrottled
                | Reason::LeaseHeld
                | Reason::LeaseUnavailable
                | Reason::DatabaseBusy
                | Reason::PolicyUnavailable
                | Reason::LandedDeclined
                | Reason::NoWorkAdopted
                | Reason::NoWorkSessionCommits
                | Reason::NoWorkWorktreeDirty
                | Reason::NoWorkHolderUnbound
                | Reason::NoWorkHolderAlive
                | Reason::NoWorkHolderGoneRecently
                | Reason::NoWorkExplicitClaim
                | Reason::NoWorkMergeQueue
                | Reason::NoWorkPullRequestOpen
                | Reason::NoWorkPullRequestUnknown
                | Reason::Unrecorded => {}
            }
            assert_eq!(
                reason.telemetry_name(),
                format!("session_abandonment_kept_{}", reason.code())
            );
        }
        let codes = every
            .iter()
            .map(|reason| reason.code())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(codes.len(), every.len(), "codes are distinct");
    }

    /// The retirement lap reports what the abandonment window did, kept
    /// sessions per reason, and where the cursor started and ended, and
    /// `broker status` shows the same breakdown.
    #[test]
    fn the_retirement_report_carries_the_abandonment_breakdown() {
        let (_owner, _storage, mut broker) = fixture("");
        let (claimed, _) = session(&mut broker, "explicit claims, holder gone", false);
        broker
            .store()
            .claim_lease(claimed.id, "base.txt", None)
            .unwrap();
        quiet(&mut broker, &claimed, 80);
        let (plain, _) = session(&mut broker, "no claims, holder gone", false);
        quiet(&mut broker, &plain, 80);

        let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();

        assert!(is_open(&broker, claimed.id), "{report:#?}");
        assert!(!is_open(&broker, plain.id), "{report:#?}");
        assert_eq!(report.abandonment_sessions_examined, 2, "{report:#?}");
        assert_eq!(report.abandonment_sessions_closed, 1, "{report:#?}");
        assert_eq!(
            report.abandonment_sessions_kept,
            BTreeMap::from([("lease_held".to_string(), 1)]),
            "{report:#?}"
        );
        assert_eq!(
            report
                .retirement_cursor_start
                .map(|cursor| cursor.after_session_id),
            Some(0),
            "{report:#?}"
        );
        let view = report
            .landed_session_retirement_view()
            .expect("the abandonment window judged sessions");
        let abandonment = view.abandonment.as_ref().expect("abandonment breakdown");
        assert_eq!(abandonment.examined, 2);
        assert_eq!(abandonment.closed, 1);
        assert_eq!(abandonment.kept.get("lease_held"), Some(&1));
        assert_eq!(view.cursor_start, report.retirement_cursor_start);
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["abandonment"]["kept"]["lease_held"], 1);

        // Past the grace the next lap closes it.
        age_gone_event(&mut broker, &claimed, 20);
        let report = broker.retire_abandoned_sessions_for_recovery(None).unwrap();
        assert!(!is_open(&broker, claimed.id), "{report:#?}");
        assert_eq!(report.abandonment_sessions_closed, 1, "{report:#?}");
    }

    /// A session whose landed proof runs out of budget leaves the retirement
    /// cursor on itself for one pass only. It used to stay there for as long
    /// as no pass gave that one proof ten seconds, so every later session
    /// waited behind it, pass after pass.
    #[test]
    fn a_budget_exhausted_session_holds_the_retirement_cursor_for_one_pass_only() {
        let (_owner, _storage, mut broker) = fixture("");
        // Past the landed window (24 h), short of the abandonment one (72 h),
        // unlanded: only the oldest-first lap's landed proof judges them.
        let (first, _) = session(&mut broker, "proof runs out of budget", false);
        quiet(&mut broker, &first, 30);
        let (second, _) = session(&mut broker, "waits behind it", false);
        quiet(&mut broker, &second, 30);
        crate::auto_cleanup::LAP_LANDED_PROOF_EXHAUSTS_BUDGET.with(|flag| flag.set(true));
        let passes = (0..3)
            .map(|_| broker.retire_abandoned_sessions_for_recovery(None))
            .collect::<Vec<_>>();
        crate::auto_cleanup::LAP_LANDED_PROOF_EXHAUSTS_BUDGET.with(|flag| flag.set(false));
        let cursor_after = |report: &crate::AutoCleanupReport| {
            report
                .retirement_cursor
                .map(|cursor| cursor.after_session_id)
        };
        let passes = passes.into_iter().collect::<Result<Vec<_>, _>>().unwrap();

        assert_eq!(
            cursor_after(&passes[0]),
            Some(0),
            "held on the first session"
        );
        assert_eq!(
            cursor_after(&passes[1]),
            Some(first.id),
            "the next pass judges it once more and moves on: {:#?}",
            passes[1]
        );
        assert_eq!(
            passes[1]
                .retirement_cursor_start
                .map(|cursor| cursor.after_session_id),
            Some(0)
        );
        assert!(
            cursor_after(&passes[2]).is_some_and(|after| after >= second.id),
            "and the session behind it does not wait either: {:#?}",
            passes[2]
        );
        assert!(is_open(&broker, first.id));
        assert!(is_open(&broker, second.id));
    }

    /// A landed proof that spends the pass's landed budget no longer ends
    /// the pass: the sessions after it are still judged by the abandonment
    /// window (stale classification), while the cursor stays on the spent
    /// session for the next pass.
    #[test]
    fn a_spent_landed_budget_does_not_stop_the_abandonment_judgements() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        // Past the landed window only: its landed proof runs out of budget.
        let (spent, _) = session(&mut broker, "proof runs out of budget", false);
        quiet(&mut broker, &spent, 30);
        // Past the abandonment window: judged by its work class.
        let (stale, _) = session(&mut broker, "stale, unpushed", false);
        quiet(&mut broker, &stale, 400);
        crate::auto_cleanup::LAP_LANDED_PROOF_EXHAUSTS_BUDGET.with(|flag| flag.set(true));
        let report = broker.retire_abandoned_sessions_for_recovery(None);
        crate::auto_cleanup::LAP_LANDED_PROOF_EXHAUSTS_BUDGET.with(|flag| flag.set(false));
        let report = report.unwrap();

        assert_eq!(report.abandonment_sessions_examined, 1, "{report:#?}");
        assert_eq!(
            report
                .retirement_cursor
                .map(|cursor| cursor.after_session_id),
            Some(0),
            "the cursor stays on the spent session: {report:#?}"
        );
        assert_eq!(closing_soon_notes(&broker, &stale), 1, "{report:#?}");
        assert!(is_open(&broker, spent.id));
        assert!(is_open(&broker, stale.id));
    }

    /// Re-judging a noted session in later passes neither sends another
    /// closing-soon note nor moves the one that stands: its time, and so the
    /// close it announces, stay as first written.
    #[test]
    fn a_closing_soon_note_is_neither_resent_nor_reset_by_later_passes() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        let (noted, _) = session(&mut broker, "unpushed, in its notice period", false);
        quiet(&mut broker, &noted, 150);
        let note_times = |broker: &crate::Broker| {
            broker
                .store_ref()
                .automation_notes(noted.id)
                .unwrap()
                .into_iter()
                .filter(|note| {
                    note.fingerprint.as_deref()
                        == Some(crate::stale_session_policy::CLOSING_SOON_NOTE_FINGERPRINT)
                })
                .map(|note| (note.id, note.created_at))
                .collect::<Vec<_>>()
        };

        broker.retire_abandoned_sessions_for_recovery(None).unwrap();
        let first = note_times(&broker);
        std::thread::sleep(std::time::Duration::from_millis(20));
        broker.retire_abandoned_sessions_for_recovery(None).unwrap();
        let second = note_times(&broker);

        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(second, first, "the note is not re-sent or rewritten");
        assert!(is_open(&broker, noted.id));
    }

    fn close_session_row(broker: &mut crate::Broker, session: &crate::Session) {
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET status = 'cleaned', cleanup_state = 'closed', closed_at = 1 WHERE id = ?1",
                [session.id],
            )
            .unwrap();
    }

    /// A closed session whose checkout is gone from a mounted pool root,
    /// unregistered and unused, is recorded as cleaned without deleting
    /// anything; every unproven case is kept with its reason.
    #[test]
    fn a_closed_session_whose_checkout_is_gone_is_finished_only_when_proven() {
        let (_owner, _storage, mut broker) = fixture("");
        let root = broker.main_root().to_path_buf();

        let (gone, gone_tree) = session(&mut broker, "checkout removed elsewhere", false);
        close_session_row(&mut broker, &gone);
        git(
            &root,
            &["worktree", "remove", "--force", gone_tree.to_str().unwrap()],
        );

        let (present, _) = session(&mut broker, "checkout still here", false);
        close_session_row(&mut broker, &present);

        let (registered, registered_tree) = session(&mut broker, "still registered", false);
        close_session_row(&mut broker, &registered);
        std::fs::remove_dir_all(&registered_tree).unwrap();

        let (shared, shared_tree) = session(&mut broker, "path reused by an open session", false);
        close_session_row(&mut broker, &shared);
        git(
            &root,
            &[
                "worktree",
                "remove",
                "--force",
                shared_tree.to_str().unwrap(),
            ],
        );
        let (reuser, _) = session(&mut broker, "open at the same path", false);
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET worktree_path = ?1 WHERE id = ?2",
                rusqlite::params![shared.worktree_path, reuser.id],
            )
            .unwrap();

        let (unmounted, _) = session(&mut broker, "pool root not on disk", false);
        close_session_row(&mut broker, &unmounted);
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET worktree_path = '/nonexistent-auto-close-pool/checkout' WHERE id = ?1",
                [unmounted.id],
            )
            .unwrap();

        let summary = broker
            .finish_closed_sessions_with_missing_checkout(None)
            .unwrap();

        assert_eq!(summary.finished, 1, "{summary:?}");
        let finished = broker.store_ref().session(gone.id).unwrap();
        assert_eq!(finished.cleanup_state, SessionCleanupState::Cleaned);
        assert!(
            broker
                .repo_handle()
                .resolve_ref(&format!("refs/heads/{}", gone.branch))
                .is_some(),
            "the session branch is kept"
        );
        assert_eq!(
            summary.kept.get("checkout_present"),
            Some(&1),
            "{summary:?}"
        );
        assert_eq!(summary.kept.get("git_registered"), Some(&1), "{summary:?}");
        assert_eq!(
            summary.kept.get("open_session_at_path"),
            Some(&1),
            "{summary:?}"
        );
        assert_eq!(
            summary.kept.get("pool_root_missing"),
            Some(&1),
            "{summary:?}"
        );
        for kept in [&present, &registered, &shared, &unmounted] {
            assert_eq!(
                broker.store_ref().session(kept.id).unwrap().cleanup_state,
                SessionCleanupState::Closed,
                "{:?}",
                kept.task
            );
        }
    }

    /// The shipped per-class windows instead of the fixture's.
    /// The class windows these lane tests were written against (24 h /
    /// 72 h / 168 h, a 24 h notice), set explicitly: what the lanes do with
    /// a window does not depend on which numbers are shipped.
    fn shipped_class_windows(broker: &crate::Broker) {
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[cleanup]\nstale_landed_close_after_hours = 24\nstale_pushed_close_after_hours = 72\n\
             stale_unpushed_close_after_hours = 168\nstale_closing_notice_hours = 24\n",
        )
        .unwrap();
    }

    /// Push the session branch so its remote-tracking ref holds HEAD.
    fn push_branch(broker: &crate::Broker, session: &crate::Session) {
        let root = broker.main_root().to_path_buf();
        git(&root, &["push", "-q", "origin", session.branch.as_str()]);
    }

    fn table_with_live(
        session: &crate::Session,
        event_id: i64,
        holder: &crate::session_holder::AgentProcess,
    ) -> crate::session_holder::RecordedHolderProcessTable {
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_holder_process_table_for_test(
            session.id,
            event_id,
            crate::session_holder::ProcessTable::parse(&format!(
                "{} 1 {} {}\n",
                holder.pid, holder.started, holder.command
            )),
        );
        table
    }

    /// Push every closing-soon note of `session` `hours` into the past, as if
    /// it had been left that long ago.
    fn age_closing_soon_notes(broker: &mut crate::Broker, session: &crate::Session, hours: i64) {
        broker
            .store()
            .connection()
            .execute(
                "UPDATE automation_notes SET created_at = created_at - ?1
                 WHERE recipient_session_id = ?2 AND fingerprint = ?3",
                rusqlite::params![
                    hours * HOUR_MS,
                    session.id,
                    crate::stale_session_policy::CLOSING_SOON_NOTE_FINGERPRINT
                ],
            )
            .unwrap();
    }

    fn closing_soon_notes(broker: &crate::Broker, session: &crate::Session) -> usize {
        broker
            .store_ref()
            .automation_notes(session.id)
            .unwrap()
            .iter()
            .filter(|note| {
                note.fingerprint.as_deref()
                    == Some(crate::stale_session_policy::CLOSING_SOON_NOTE_FINGERPRINT)
            })
            .count()
    }

    fn kept_not_due(broker: &mut crate::Broker, session: &crate::Session) {
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(broker, session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::NotDue,
            "{:?}",
            session.task
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::WorkClassNotDue)
        );
        assert!(is_open(broker, session.id));
    }

    fn closes_as(broker: &mut crate::Broker, session: &crate::Session, class: &str) {
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(broker, session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed,
            "{class}"
        );
        assert_eq!(close_payload(broker, session.id)["work_class"], class);
    }

    /// Past the abandonment window with its holder gone, a session waits for
    /// the window of what closing would put at risk: unpushed work a week,
    /// pushed but unmerged work three days (its branch left on the remote),
    /// work with nothing to lose closes at once. Work closes only after a
    /// closing-soon note has stood for a day, even when the window passed
    /// long ago (the notice period is never skipped).
    #[test]
    fn each_work_class_closes_after_its_own_window_and_a_standing_note() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        let (early, _) = session(&mut broker, "unpushed, three days idle", false);
        let (soon, _) = session(&mut broker, "unpushed, six days idle", false);
        let (due, _) = session(&mut broker, "unpushed, a week idle", false);
        let (pushed, _) = session(&mut broker, "pushed, unmerged", false);
        let nothing = broker.start_worktree("never committed", None).unwrap();
        push_branch(&broker, &pushed);
        quiet(&mut broker, &early, 80);
        quiet(&mut broker, &soon, 150);
        quiet(&mut broker, &due, 400);
        quiet(&mut broker, &pushed, 80);
        quiet(&mut broker, &nothing, 80);

        for kept in [&early, &soon, &due] {
            kept_not_due(&mut broker, kept);
        }
        // Pushed and nothing else: nothing on it can be lost, so it closes
        // at the abandonment window instead of waiting for its class window
        // and a note (the early-close rule), leaving the branch alone.
        closes_as(&mut broker, &pushed, "pushed_unmerged");
        assert_eq!(
            closing_soon_notes(&broker, &early),
            0,
            "days from its close"
        );
        for noted in [&soon, &due] {
            assert_eq!(closing_soon_notes(&broker, noted), 1, "{:?}", noted.task);
        }
        let notes = broker.store_ref().unread_automation_notes(soon.id).unwrap();
        assert!(notes[0].message.contains("closes it automatically"));
        // A second look before the note has stood a day still keeps them.
        kept_not_due(&mut broker, &due);
        assert_eq!(
            closing_soon_notes(&broker, &due),
            1,
            "one note, not one per look"
        );
        closes_as(&mut broker, &nothing, "landed");

        age_closing_soon_notes(&mut broker, &due, 25);
        closes_as(&mut broker, &due, "unpushed");
        assert!(
            broker
                .repo_handle()
                .resolve_ref(&format!("refs/remotes/origin/{}", pushed.branch))
                .is_some(),
            "closing leaves the pushed branch alone"
        );

        let view = broker.status(now_ms()).unwrap();
        let backlog = view
            .advice
            .iter()
            .find(|advice| advice.id == "sessions.stale-backlog")
            .expect("open sessions past the window are reported by class");
        assert!(
            backlog.summary.contains("2 with unpushed"),
            "{}",
            backlog.summary
        );
        let closing = view
            .advice
            .iter()
            .find(|advice| advice.id == "sessions.closing-soon")
            .expect("the announced session is advised");
        assert_eq!(closing.session_id, Some(soon.id));
    }

    /// A closing-soon note that cannot be written keeps the session and does
    /// not start the notice period; the next look writes it.
    #[test]
    fn a_failed_closing_soon_note_keeps_the_session_without_advancing() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        let (stale, _) = session(&mut broker, "unpushed, note write fails", false);
        quiet(&mut broker, &stale, 400);
        crate::stale_session_policy::fail_next_closing_soon_note_for_test();
        kept_not_due(&mut broker, &stale);
        assert_eq!(closing_soon_notes(&broker, &stale), 0);
        kept_not_due(&mut broker, &stale);
        assert_eq!(closing_soon_notes(&broker, &stale), 1);
    }

    /// The class reads the session branch as well as HEAD: a checkout
    /// detached at its base, or every pull request merged with commits on
    /// the branch after what merged, still holds unpushed work.
    #[test]
    fn the_branch_tip_is_classed_as_well_as_head() {
        let (_owner, _storage, mut broker) = fixture("");
        let (detached, detached_tree) = session(&mut broker, "detached at the base", false);
        let base = detached.diff_base.clone().unwrap();
        git(&detached_tree, &["checkout", "-q", "--detach", &base]);
        assert_eq!(
            broker.stale_work_class(&detached),
            crate::stale_session_policy::StaleWorkClass::Unpushed
        );

        let (merged, merged_tree) = session(&mut broker, "merged, then more work", true);
        link_pull_request(&mut broker, merged.id, 77, Some(1));
        assert_eq!(
            broker.stale_work_class(&merged),
            crate::stale_session_policy::StaleWorkClass::Landed,
            "what merged has landed"
        );
        std::fs::write(merged_tree.join("later.txt"), "after the merge\n").unwrap();
        git(&merged_tree, &["add", "later.txt"]);
        git(&merged_tree, &["commit", "-qm", "after the merge"]);
        assert_eq!(
            broker.stale_work_class(&merged),
            crate::stale_session_policy::StaleWorkClass::Unpushed,
            "a merged pull request does not cover later commits"
        );
    }

    /// A working-tree status that cannot be taken to the end (here a corrupt
    /// index; in practice the lane's deadline) is "not examined": the class
    /// is the protective one for this look only, and nothing is remembered,
    /// so a later look is not answered "dirty" from it.
    #[test]
    fn a_status_that_cannot_finish_is_not_examined_and_never_remembered() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        let nothing = broker.start_worktree("status cannot finish", None).unwrap();
        quiet(&mut broker, &nothing, 80);
        let worktree = PathBuf::from(&nothing.worktree_path);
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
            .current_dir(&worktree)
            .output()
            .unwrap();
        let index = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        std::fs::write(&index, b"not an index").unwrap();
        let (class, examined) = broker
            .stale_work_class_examined(&nothing, crate::stale_session_policy::TreeCheck::Fresh);
        assert_eq!(class, crate::stale_session_policy::StaleWorkClass::Unpushed);
        assert!(!examined, "a status that failed was not an examination");
        let remembered = broker
            .store_ref()
            .meta_get(crate::stale_session_policy::CLEAN_CACHE_META_KEY)
            .unwrap()
            .and_then(|raw| {
                serde_json::from_str::<std::collections::BTreeMap<i64, serde_json::Value>>(&raw)
                    .ok()
            })
            .unwrap_or_default();
        assert!(
            !remembered.contains_key(&nothing.id),
            "a failed status must not be remembered as dirty: {remembered:?}"
        );
    }

    /// Each open session's last keep reason is shown in `broker status`, as is
    /// a lane that judged nothing for an hour while sessions are due, and a
    /// retention policy that cannot be read.
    #[test]
    fn status_shows_why_abandoned_sessions_stay_open() {
        let (_owner, _storage, mut broker) = fixture("");
        let (held, _) = session(&mut broker, "held by a lease", false);
        quiet(&mut broker, &held, 80);
        crate::stale_session_policy::remember_keep(
            broker.store(),
            held.id,
            Some(crate::stale_session_policy::KeepRecord {
                reason: "lease_held".into(),
                at_ms: now_ms(),
            }),
        );
        let view = broker.status(now_ms()).unwrap();
        let held_advice = view
            .advice
            .iter()
            .find(|advice| advice.id == "sessions.abandonment-held")
            .expect("held sessions are reported with their reason");
        assert!(
            held_advice.summary.contains("1 by lease_held"),
            "{}",
            held_advice.summary
        );
        // Nothing judged yet while the session is 80 h idle (window 72 h).
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "sessions.abandonment-lane-idle")
        );
        crate::stale_session_policy::record_abandonment_judged(broker.store(), now_ms());
        let view = broker.status(now_ms()).unwrap();
        assert!(
            !view
                .advice
                .iter()
                .any(|advice| advice.id == "sessions.abandonment-lane-idle")
        );

        // A fractional window cannot be read: loud, never silent.
        std::fs::write(
            broker.main_root().join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 0.5\n",
        )
        .unwrap();
        let view = broker.status(now_ms()).unwrap();
        let invalid = view
            .advice
            .iter()
            .find(|advice| advice.id == "retention.invalid")
            .expect("an unreadable retention policy is reported");
        assert!(
            invalid.summary.contains("never closed"),
            "{}",
            invalid.summary
        );
    }

    /// A remembered clean answer never closes a session: an untracked file
    /// added after it (the index, which the memory is keyed by, does not
    /// change) is seen by the close decision, which asks the tree afresh,
    /// so the session waits for the unpushed window instead of closing as
    /// landed.
    #[test]
    fn a_remembered_clean_answer_never_closes_a_session() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        let nothing = broker.start_worktree("never committed", None).unwrap();
        quiet(&mut broker, &nothing, 80);
        assert_eq!(
            broker.stale_work_class(&nothing),
            crate::stale_session_policy::StaleWorkClass::Landed,
            "clean, remembered"
        );
        std::fs::write(
            PathBuf::from(&nothing.worktree_path).join("notes.txt"),
            "untracked work\n",
        )
        .unwrap();
        // Written long ago (a fresh file would itself be recent activity):
        // this test is about the remembered class, not file times.
        crate::worktree_status_activity::age_tree_for_test(
            &PathBuf::from(&nothing.worktree_path),
            std::time::SystemTime::now() - Duration::from_secs(80 * 60 * 60),
        );
        // Remembered, the class still reads clean (nothing it is keyed by
        // changed); the close decision asks afresh.
        assert_eq!(
            broker.stale_work_class(&nothing),
            crate::stale_session_policy::StaleWorkClass::Landed
        );

        kept_not_due(&mut broker, &nothing);

        assert!(
            PathBuf::from(&nothing.worktree_path)
                .join("notes.txt")
                .exists()
        );
        assert_eq!(
            broker.stale_work_class_fresh(&nothing),
            crate::stale_session_policy::StaleWorkClass::Unpushed
        );
    }

    /// Classification asks the working tree only when refs alone would
    /// class the session below unpushed, and remembers that answer while
    /// HEAD, the branch tip and the index are unchanged; staging a change
    /// asks again.
    #[test]
    fn the_working_tree_is_asked_only_when_refs_cannot_decide() {
        let (_owner, _storage, mut broker) = fixture("");
        let recorder = crate::phase_telemetry::Recorder::default();
        let installed = crate::phase_telemetry::install(recorder.clone());
        let (unpushed, _) = session(&mut broker, "refs decide", false);
        assert_eq!(
            broker.stale_work_class(&unpushed),
            crate::stale_session_policy::StaleWorkClass::Unpushed
        );
        let (merged, merged_tree) = session(&mut broker, "tree decides", true);
        link_pull_request(&mut broker, merged.id, 78, Some(1));
        assert_eq!(
            broker.stale_work_class(&merged),
            crate::stale_session_policy::StaleWorkClass::Landed
        );
        assert_eq!(
            broker.stale_work_class(&merged),
            crate::stale_session_policy::StaleWorkClass::Landed
        );
        std::fs::write(merged_tree.join("staged.txt"), "staged\n").unwrap();
        git(&merged_tree, &["add", "staged.txt"]);
        assert_eq!(
            broker.stale_work_class(&merged),
            crate::stale_session_policy::StaleWorkClass::Unpushed,
            "a changed index is asked again"
        );
        drop(installed);
        let (_millis, counts) = recorder.totals();
        assert_eq!(
            counts.get("stale_work_classification_refs_only").copied(),
            Some(1),
            "{counts:?}"
        );
        assert_eq!(
            counts.get("stale_work_clean_remembered").copied(),
            Some(1),
            "{counts:?}"
        );
    }

    /// A live holder bound to a newer session of this repository is no live
    /// holder of the older one: its uncommitted work gets the closing-soon
    /// note and then closes after the unpushed window, through the archive
    /// path.
    #[test]
    fn a_holder_that_moved_on_counts_as_no_live_holder_for_the_work_class() {
        let (_owner, _storage, mut broker) = fixture("");
        shipped_class_windows(&broker);
        let holder = live_holder();
        let (old, tree) = session(&mut broker, "uncommitted, holder moved on", false);
        std::fs::write(tree.join("base.txt"), "edited, not committed\n").unwrap();
        let mut table = held_by_live_holder(&mut broker, &old, &holder, 400);
        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);

        assert_eq!(
            close_abandoned_directly(&mut broker, &old, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::NotDue
        );
        assert_eq!(closing_soon_notes(&broker, &old), 1);
        age_closing_soon_notes(&mut broker, &old, 25);
        assert_eq!(
            close_abandoned_directly(&mut broker, &old, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        let payload = close_payload(&broker, old.id);
        assert_eq!(payload["holder_evidence"], "moved_on");
        assert_eq!(payload["holder_moved_on_to_session"], newer.id);
        assert_eq!(payload["work_class"], "unpushed");
        assert!(
            is_open(&broker, newer.id),
            "the holder's latest session stays"
        );
    }

    /// A bind back to the older session makes the process its holder again.
    #[test]
    fn a_holder_bound_back_keeps_the_older_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let holder = live_holder();
        let (old, _) = session(&mut broker, "holder came back", false);
        held_by_live_holder(&mut broker, &old, &holder, 80);
        let (newer, _) = session(&mut broker, "a task in between", false);
        bind_holder(&mut broker, &newer, &holder);
        let back = bind_holder(&mut broker, &old, &holder);
        let mut table = table_with_live(&old, back, &holder);

        assert_eq!(
            close_abandoned_directly(&mut broker, &old, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::ActiveProcess
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::HolderAliveCurrent)
        );
        assert!(is_open(&broker, old.id));
    }

    /// One process may legitimately work two sessions at once: an explicit
    /// claim it still holds on the older one keeps that session, however
    /// the newer binding reads.
    #[test]
    fn an_explicit_claim_of_a_moved_on_holder_keeps_the_older_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let holder = live_holder();
        let (old, _) = session(&mut broker, "two sessions at once", false);
        let mut table = held_by_live_holder(&mut broker, &old, &holder, 80);
        broker
            .store()
            .claim_lease(old.id, "base.txt", None)
            .unwrap();
        let (newer, _) = session(&mut broker, "the other session", false);
        bind_holder(&mut broker, &newer, &holder);

        assert_ne!(
            close_abandoned_directly(&mut broker, &old, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::LeaseHeld)
        );
        assert!(is_open(&broker, old.id));
    }

    /// `session_abandoned_after_hours = 0` turns the no-work window off too
    /// (`RetentionPolicy::no_work_abandonment_window_ms`), so a foreground
    /// view finds nothing due and queues nothing; `stale_session_auto_close
    /// = false` keeps what the windows would close.
    #[test]
    fn disabled_windows_close_nothing_and_count_nothing_due() {
        let (_owner, _storage, mut broker) = fixture("");
        std::fs::write(
            broker.main_root().join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 0\n\
             session_no_work_abandoned_after_hours = 4\n",
        )
        .unwrap();
        let idle = broker
            .start_worktree("never committed, idle", None)
            .unwrap();
        quiet(&mut broker, &idle, 80);
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        {
            let _foreground = crate::file_lock::ForegroundCommand::enter();
            broker.agents(now_ms()).unwrap();
        }
        broker.agents(now_ms()).unwrap();
        assert!(is_open(&broker, idle.id));
        assert_eq!(
            phase_count(&recorder, "agents_view_abandonment_due_deferred"),
            0
        );

        std::fs::write(
            broker.main_root().join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\n",
        )
        .unwrap();
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[cleanup]\nstale_session_auto_close = false\n",
        )
        .unwrap();
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &idle, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::NotDue
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::WindowDisabled)
        );
        assert!(is_open(&broker, idle.id));
    }

    fn opened_file(path: PathBuf) -> crate::reclaim::OpenFileSnapshot {
        crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
            pid: std::process::id(),
            command: "reviewer".into(),
            descriptor: "4r".into(),
            file_type: Some("REG".into()),
            path,
        }])
    }

    /// The broker's own database open under the primary checkout's
    /// `.aethyme/` does not keep a session adopted on the primary checkout,
    /// but a file open in a review checkout under `.aethyme/reviews/` keeps
    /// the session bound to that checkout.
    #[test]
    fn only_the_brokers_own_state_files_are_ignored_by_the_open_file_check() {
        let (_owner, _storage, mut broker) = fixture("");
        let root = std::fs::canonicalize(broker.main_root()).unwrap();
        let review = root.join(".aethyme/reviews/pr-7/code");
        std::fs::create_dir_all(review.parent().unwrap()).unwrap();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "review-pr-7",
                review.to_str().unwrap(),
            ],
        );
        let reviewed = broker.adopt(&review, Some("review checkout")).unwrap();
        age(&mut broker, &reviewed, 80);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(opened_file(
            review.join("base.txt"),
        )));
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &reviewed, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::OpenFilesInCheckout)
        );
        assert!(is_open(&broker, reviewed.id));

        let primary = broker.adopt(&root, Some("primary checkout")).unwrap();
        age(&mut broker, &primary, 80);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![
                crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "aethyme".into(),
                    descriptor: "5u".into(),
                    file_type: Some("REG".into()),
                    path: root.join(".aethyme/broker.db"),
                },
                crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "aethyme".into(),
                    descriptor: "6u".into(),
                    file_type: Some("REG".into()),
                    path: root.join(".aethyme/broker.db-wal"),
                },
            ]),
        ));
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &primary, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed,
            "{:?}",
            table.keep_reason()
        );
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
    }

    /// A path a running `broker start` has reserved is in use: the close
    /// keeps the session until the reservation is released.
    #[test]
    fn a_start_reservation_on_the_checkout_keeps_the_session() {
        let (_owner, _storage, mut broker) = fixture("");
        let session = broker.start_worktree("reserved by a start", None).unwrap();
        quiet(&mut broker, &session, 80);
        let root = broker.main_root().to_path_buf();
        std::fs::create_dir_all(root.join(".aethyme/run")).unwrap();
        // `reserve` creates the destination, so the checkout steps aside for
        // the moment it takes and comes back under the held reservation.
        let checkout = PathBuf::from(&session.worktree_path);
        let aside = checkout.with_file_name("reserved-checkout-aside");
        std::fs::rename(&checkout, &aside).unwrap();
        let crate::start_reservation::Reserve::Reserved(reservation) =
            crate::start_reservation::reserve(
                &root,
                checkout.file_name().unwrap().to_str().unwrap(),
                &checkout,
                &session.branch,
                session.diff_base.as_deref().unwrap_or_default(),
            )
            .unwrap()
        else {
            panic!("the reservation is taken");
        };
        std::fs::remove_dir(&checkout).unwrap();
        std::fs::rename(&aside, &checkout).unwrap();
        assert!(crate::start_reservation::path_is_reserved(&root, &checkout));

        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::OperationActive
        );
        assert_eq!(
            table.keep_reason(),
            Some(crate::broker::AbandonmentKeepReason::StartReserved)
        );
        assert!(is_open(&broker, session.id));

        reservation.release();
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert_eq!(
            close_abandoned_directly(&mut broker, &session, &mut table),
            crate::broker::AbandonedSessionCloseOutcome::Closed
        );
    }

    /// A foreground command never judges: the `agents` view leaves a due
    /// session open (the shared worker closes it); a library caller still
    /// judges it inline.
    #[test]
    fn a_foreground_agents_view_leaves_due_sessions_to_the_worker() {
        let (_owner, _storage, mut broker) = fixture("");
        let (session, _) = session(&mut broker, "due, holder gone", false);
        quiet(&mut broker, &session, 80);
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        {
            let _foreground = crate::file_lock::ForegroundCommand::enter();
            broker.agents(now_ms()).unwrap();
        }
        assert!(is_open(&broker, session.id));
        assert_eq!(
            phase_count(&recorder, "agents_view_abandonment_due_deferred"),
            1
        );
        broker.agents(now_ms()).unwrap();
        assert!(!is_open(&broker, session.id));
    }

    // ---- The early-close rule: nothing on the session can be lost. ----

    use crate::abandonment_safety::{SafetyCheck, UnsafeReason};

    /// A broker on a fixture repository whose open-file observation is empty
    /// (no process uses any checkout), so the rule alone decides.
    fn early_fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let fixture = fixture("");
        // The shipped class windows (a day and longer): only the early rule
        // can close anything at one hour.
        shipped_class_windows(&fixture.2);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        fixture
    }

    /// Close with a one-hour abandonment window, as
    /// `session_abandoned_after_hours = 1` does; the class windows stay at
    /// their defaults (hours longer).
    fn close_after_one_hour(
        broker: &mut crate::Broker,
        session: &crate::Session,
    ) -> (
        crate::broker::AbandonedSessionCloseOutcome,
        Option<crate::broker::AbandonmentKeepReason>,
    ) {
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let outcome = broker
            .try_close_abandoned_session(session.id, now_ms(), HOUR_MS, &mut table, true, None)
            .unwrap();
        (outcome, table.keep_reason())
    }

    fn reload(broker: &crate::Broker, session: &crate::Session) -> crate::Session {
        broker.store_ref().session(session.id).unwrap()
    }

    fn origin_path(broker: &crate::Broker) -> PathBuf {
        let output = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(broker.main_root())
            .output()
            .unwrap();
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
    }

    fn head_of(tree: &Path) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(tree)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit_in(tree: &Path, name: &str) -> String {
        std::fs::write(tree.join(name), format!("{name}\n")).unwrap();
        git(tree, &["add", name]);
        git(tree, &["commit", "-qm", name]);
        head_of(tree)
    }

    /// Every hazard, as a session that must stay open, with the reason the
    /// rule names; the last two are safe and must close.
    fn hazard_fixtures(
        broker: &mut crate::Broker,
    ) -> Vec<(&'static str, crate::Session, Option<UnsafeReason>)> {
        let mut fixtures = Vec::new();
        let start = |broker: &mut crate::Broker, label: &'static str| {
            let session = broker.start_worktree(label, None).unwrap();
            let tree = PathBuf::from(&session.worktree_path);
            (session, tree)
        };

        let (staged, tree) = start(broker, "staged");
        std::fs::write(tree.join("base.txt"), "staged change\n").unwrap();
        git(&tree, &["add", "base.txt"]);
        fixtures.push(("staged", staged, Some(UnsafeReason::Staged)));

        let (unstaged, tree) = start(broker, "unstaged");
        std::fs::write(tree.join("base.txt"), "unstaged change\n").unwrap();
        fixtures.push(("unstaged", unstaged, Some(UnsafeReason::Unstaged)));

        let (untracked, tree) = start(broker, "untracked");
        std::fs::write(tree.join("notes.txt"), "untracked\n").unwrap();
        fixtures.push(("untracked", untracked, Some(UnsafeReason::Untracked)));

        let (stash, tree) = start(broker, "stash");
        std::fs::write(tree.join("base.txt"), "stashed change\n").unwrap();
        git(&tree, &["stash", "push", "-q"]);
        fixtures.push(("stash", stash, Some(UnsafeReason::Stash)));

        // A real interactive rebase, stopped at a `break` before replaying
        // anything: the checkout is clean and HEAD contained, only the
        // in-progress rebase shows.
        let (rebase, tree) = start(broker, "rebase in progress");
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .args(["rebase", "-i", "HEAD"])
            .current_dir(&tree)
            .env("GIT_SEQUENCE_EDITOR", "sh -c 'echo break > \"$1\"' sh")
            .output()
            .unwrap();
        let git_dir = crate::git::checked_worktree_git_dir(&tree)
            .unwrap()
            .unwrap();
        assert!(
            git_dir.join("rebase-merge").is_dir(),
            "the rebase must be stopped mid-way"
        );
        fixtures.push(("rebase", rebase, Some(UnsafeReason::InProgress)));

        // A change status cannot see: the file is flagged assume-unchanged.
        let (hidden, tree) = start(broker, "assume-unchanged edit");
        std::fs::write(tree.join("base.txt"), "hidden change\n").unwrap();
        git(&tree, &["update-index", "--assume-unchanged", "base.txt"]);
        fixtures.push(("assume_unchanged", hidden, Some(UnsafeReason::NotExamined)));

        // The branch holds an unpushed commit while HEAD sits detached on
        // the (contained) base: only the branch shows it.
        let (branch, tree) = start(broker, "unpushed commit on the branch");
        let base = branch.diff_base.clone().unwrap();
        commit_in(&tree, "branch-only.txt");
        git(&tree, &["checkout", "-q", "--detach", &base]);
        fixtures.push((
            "unpushed_branch",
            branch,
            Some(UnsafeReason::BranchNotContained),
        ));

        // A commit on a detached HEAD only; the branch stays at the base.
        let (detached, tree) = start(broker, "commit on detached HEAD only");
        git(&tree, &["checkout", "-q", "--detach"]);
        commit_in(&tree, "detached-only.txt");
        fixtures.push((
            "detached_head",
            detached,
            Some(UnsafeReason::HeadNotContained),
        ));

        // Adopted at a HEAD holding commits nobody pushed: the diff base is
        // that HEAD, and gets no exemption.
        let (adopted, tree) = start(broker, "adopted with unpushed commits");
        let adopted_head = commit_in(&tree, "pre-adoption.txt");
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET diff_base = ?1 WHERE id = ?2",
                rusqlite::params![adopted_head, adopted.id],
            )
            .unwrap();
        let adopted = reload(broker, &adopted);
        fixtures.push(("adopted", adopted, Some(UnsafeReason::HeadNotContained)));

        // Pushed, then the remote branch deleted: the local tracking ref
        // still names the commit, the remote no longer has it.
        let (stale_ref, tree) = start(broker, "stale tracking ref");
        commit_in(&tree, "pushed-then-deleted.txt");
        git(&tree, &["push", "-q", "origin", &stale_ref.branch]);
        let origin = origin_path(broker);
        git(
            &origin,
            &[
                "update-ref",
                "-d",
                &format!("refs/heads/{}", stale_ref.branch),
            ],
        );
        fixtures.push((
            "stale_tracking_ref",
            stale_ref,
            Some(UnsafeReason::HeadNotContained),
        ));

        // Safe: nothing committed, nothing changed.
        let (clean, _) = start(broker, "clean, nothing committed");
        fixtures.push(("clean", clean, None));

        // Safe: a commit the remote holds.
        let (pushed, tree) = start(broker, "pushed commit");
        commit_in(&tree, "pushed.txt");
        git(&tree, &["push", "-q", "origin", &pushed.branch]);
        fixtures.push(("pushed", pushed, None));

        fixtures
    }

    /// Each hazard keeps its session; the two safe sessions pass the rule.
    #[test]
    fn the_early_close_rule_names_every_hazard() {
        let (_owner, _storage, mut broker) = early_fixture();
        for (label, session, expected) in hazard_fixtures(&mut broker) {
            let verdict = broker.abandonment_safety_check(&session);
            match expected {
                Some(reason) => assert_eq!(verdict, Err(reason), "{label}"),
                None => assert_eq!(verdict, Ok(()), "{label}"),
            }
        }
    }

    /// Neutering any one check of the rule lets exactly one hazard through:
    /// every check is needed, and each has the fixture that proves it.
    #[test]
    fn each_neutered_check_lets_exactly_one_hazard_through() {
        let (_owner, _storage, mut broker) = early_fixture();
        let fixtures = hazard_fixtures(&mut broker);
        let expected = [
            (SafetyCheck::Staged, "staged"),
            (SafetyCheck::Unstaged, "unstaged"),
            (SafetyCheck::Untracked, "untracked"),
            (SafetyCheck::Stash, "stash"),
            (SafetyCheck::InProgress, "rebase"),
            (SafetyCheck::BranchContained, "unpushed_branch"),
            (SafetyCheck::HeadContained, "detached_head"),
            (SafetyCheck::DiffBaseNotExempt, "adopted"),
            (SafetyCheck::RemoteAsked, "stale_tracking_ref"),
            (SafetyCheck::IndexFlags, "assume_unchanged"),
        ];
        for (check, hazard) in expected {
            crate::abandonment_safety::neuter_for_test(Some(check));
            let let_through = fixtures
                .iter()
                .filter(|(_, _, expected)| expected.is_some())
                .filter(|(_, session, _)| broker.abandonment_safety_check(session).is_ok())
                .map(|(label, _, _)| *label)
                .collect::<Vec<_>>();
            crate::abandonment_safety::neuter_for_test(None);
            assert_eq!(let_through, [hazard], "neutering {check:?}");
        }
    }

    /// Through the whole close path with a one-hour window: every hazard is
    /// kept (by its class window, as today), and only safe sessions close.
    #[test]
    fn with_a_one_hour_window_only_safe_sessions_close_early() {
        let (_owner, _storage, mut broker) = early_fixture();
        for (label, session, expected) in hazard_fixtures(&mut broker) {
            quiet(&mut broker, &session, 2);
            let (outcome, reason) = close_after_one_hour(&mut broker, &session);
            if expected.is_some() {
                assert_ne!(
                    outcome,
                    crate::broker::AbandonedSessionCloseOutcome::Closed,
                    "{label} closed: {reason:?}"
                );
                assert!(is_open(&broker, session.id), "{label}");
            } else {
                assert_eq!(
                    outcome,
                    crate::broker::AbandonedSessionCloseOutcome::Closed,
                    "{label} kept: {reason:?}"
                );
            }
        }
    }

    /// A safe session closed at one hour resumes exactly through the path
    /// `start --reuse` takes: its retired checkout is recreated from the
    /// branch (`recreate_retired_checkout`, as the CLI does), then the session
    /// is reused, with the same HEAD, a clean status and every commit it
    /// could reach. The close is recorded with its reason trail, and the
    /// resume within a day is counted and advised.
    #[test]
    fn a_session_closed_at_one_hour_resumes_exactly() {
        let (_owner, _storage, mut broker) = fixture("closed_worktree_grace_hours = 0\n");
        shipped_class_windows(&broker);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        let session = broker
            .start_worktree("closed early, then reused", None)
            .unwrap();
        let tree = PathBuf::from(&session.worktree_path);
        let head = commit_in(&tree, "resumable.txt");
        git(&tree, &["push", "-q", "origin", &session.branch]);
        let reachable = |root: &Path| {
            let output = Command::new("git")
                .args(["rev-list", "--all"])
                .current_dir(root)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>()
        };
        let before = reachable(broker.main_root());
        quiet(&mut broker, &session, 2);
        let (outcome, reason) = close_after_one_hour(&mut broker, &session);
        assert_eq!(
            outcome,
            crate::broker::AbandonedSessionCloseOutcome::Closed,
            "{reason:?}"
        );
        assert!(!is_open(&broker, session.id));
        assert_eq!(
            close_payload(&broker, session.id)["early_close"]["rule"],
            "nothing_on_the_session_can_be_lost"
        );

        // Retire the closed checkout as the retirement lane does, so reuse
        // takes the CLI's restore path.
        let report = broker
            .run_checkout_retirement(std::time::Instant::now() + Duration::from_secs(600))
            .unwrap();
        assert!(
            !tree.exists() || crate::broker::is_checkout_tombstone(&tree),
            "the checkout is retired, so reuse takes the restore path: {report:#?}"
        );
        assert!(broker.recreate_retired_checkout(&tree).unwrap());
        let reused = broker
            .adopt_with(&tree, Some("follow-up"), crate::AdoptMode::Reuse, None)
            .unwrap()
            .session;
        let reused_tree = PathBuf::from(&reused.worktree_path);
        assert_eq!(head_of(&reused_tree), head);
        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(&reused_tree)
            .output()
            .unwrap();
        assert!(
            status.stdout.is_empty(),
            "{:?}",
            String::from_utf8_lossy(&status.stdout)
        );
        let after = reachable(broker.main_root());
        assert!(
            before.is_subset(&after),
            "commits became unreachable: {:?}",
            before.difference(&after).collect::<Vec<_>>()
        );
        assert!(
            crate::stale_session_policy::read_checkout_times(
                broker.store_ref(),
                crate::stale_session_policy::EARLY_RESUMED_META_KEY
            )
            .contains_key(&reused.worktree_path),
            "a resume within a day of an early close is recorded"
        );
        let view = broker.status(now_ms()).unwrap();
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "sessions.early-close-resumed")
        );
    }

    /// The rule fails on the first look (a transient untracked file) and
    /// holds only under the claims, after the process checks ran for a
    /// class-window close: those checks skipped the open-file observation,
    /// so the session is kept even though a process sits in its checkout.
    #[test]
    fn a_rule_that_first_holds_under_the_claims_keeps_the_session() {
        let (_owner, _storage, mut broker) = early_fixture();
        let session = broker
            .start_worktree("pushed, a process inside", None)
            .unwrap();
        let tree = PathBuf::from(&session.worktree_path);
        commit_in(&tree, "pushed.txt");
        git(&tree, &["push", "-q", "origin", &session.branch]);
        // Inside the pushed class's notice period: the first look passes.
        quiet(&mut broker, &session, 50);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "agent".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&tree).unwrap(),
            }]),
        ));
        crate::abandonment_safety::fail_next_looks_for_test(1);
        let (outcome, _) = close_after_one_hour(&mut broker, &session);
        crate::abandonment_safety::fail_next_looks_for_test(0);
        assert_eq!(outcome, crate::broker::AbandonedSessionCloseOutcome::NotDue);
        assert!(is_open(&broker, session.id));
    }

    /// An agent that only reads still calls tools: its PreToolUse signal is
    /// activity, so the one-hour window does not close it under the agent.
    #[test]
    fn a_reading_agent_is_not_idle() {
        let (_owner, _storage, mut broker) = early_fixture();
        let session = broker.start_worktree("reading only", None).unwrap();
        quiet(&mut broker, &session, 2);
        crate::agent_hook::record_hook_activity(broker.main_root(), session.id);
        let (outcome, reason) = close_after_one_hour(&mut broker, &session);
        assert_eq!(outcome, crate::broker::AbandonedSessionCloseOutcome::NotDue);
        assert_eq!(
            reason,
            Some(crate::broker::AbandonmentKeepReason::RecentActivity)
        );
    }

    /// A process using the checkout keeps a session that would close early,
    /// whatever its holder evidence says.
    #[test]
    fn a_process_in_the_checkout_keeps_an_early_close() {
        let (_owner, _storage, mut broker) = early_fixture();
        let session = broker.start_worktree("someone is in here", None).unwrap();
        quiet(&mut broker, &session, 2);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "reader".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&session.worktree_path).unwrap(),
            }]),
        ));
        let (outcome, _) = close_after_one_hour(&mut broker, &session);
        assert_ne!(outcome, crate::broker::AbandonedSessionCloseOutcome::Closed);
        assert!(is_open(&broker, session.id));
    }
}
