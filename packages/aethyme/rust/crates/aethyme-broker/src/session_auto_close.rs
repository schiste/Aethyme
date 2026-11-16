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
                "; its commits are not on the default branch yet, so its branch and checkout \
                 are kept"
            } else {
                ""
            }
        ),
        _ => format!("it was inactive for {idle} with no live agent"),
    }
}

/// How to go on after an automatic close, depending on whether the checkout
/// is still on disk (automatic removal takes it once its grace has passed
/// and its work is contained).
fn continuation(worktree_path: &str) -> String {
    match std::fs::symlink_metadata(worktree_path) {
        Ok(_) => format!(
            "Its checkout {worktree_path} is still on disk. To continue there, run \
             `aethyme broker start --reuse --task \"<task>\" --short-name \"<name>\"` from \
             inside that checkout"
        ),
        Err(_) => "Its checkout has since been removed by automatic cleanup; start a new \
                   session with `aethyme broker start --task \"<task>\" --short-name \
                   \"<name>\"`"
            .to_string(),
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
    match explain_automatic_close(store, session_id) {
        Some(explanation) => format!(
            "session {session_id} is already closed; it cannot start new session work: \
             {explanation}"
        ),
        None => format!("session {session_id} is already closed; it cannot start new session work"),
    }
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
        assert!(note.contains("branch and checkout are kept"), "{note}");
        assert!(note.contains("moved on to session 77"), "{note}");
        assert!(note.contains("has since been removed"), "{note}");
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

    /// The abandonment window treats a moved-on live holder as gone only for
    /// a clean checkout; unlanded commits do not keep a clean one, and they
    /// survive every later removal pass with their branch.
    #[test]
    fn the_abandonment_window_closes_a_moved_on_session_only_when_clean() {
        let (_owner, _storage, mut broker) = fixture("closed_worktree_grace_hours = 0\n");
        let holder = live_holder();

        let (dirty, dirty_tree) = session(&mut broker, "dirty, no pull request", false);
        std::fs::write(dirty_tree.join("base.txt"), "edited, not committed\n").unwrap();
        let mut dirty_table = held_by_live_holder(&mut broker, &dirty, &holder, 80);

        let (clean, clean_tree) = session(&mut broker, "clean with unlanded commits", false);
        let clean_head = crate::git::GitRepo::discover(&clean_tree)
            .unwrap()
            .head_commit()
            .unwrap();
        let mut clean_table = held_by_live_holder(&mut broker, &clean, &holder, 80);

        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);

        assert_eq!(
            close_abandoned_directly(&mut broker, &dirty, &mut dirty_table),
            crate::broker::AbandonedSessionCloseOutcome::ActiveProcess
        );
        assert!(is_open(&broker, dirty.id));

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

        // The holder moved on: a dirty checkout, and a process in a clean one.
        let (dirty, dirty_tree) = session(&mut broker, "moved on, dirty", false);
        std::fs::write(dirty_tree.join("base.txt"), "edited, not committed\n").unwrap();
        let mut dirty_table = held_by_live_holder(&mut broker, &dirty, &holder, 80);
        let (in_use, in_use_tree) = session(&mut broker, "moved on, in use", false);
        let mut in_use_table = held_by_live_holder(&mut broker, &in_use, &holder, 80);
        let (newer, _) = session(&mut broker, "the agent's next task", false);
        bind_holder(&mut broker, &newer, &holder);
        kept(
            &mut broker,
            &dirty,
            &mut dirty_table,
            Reason::HolderMovedOnDirty,
        );
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
            Reason::HolderUnknown,
            Reason::HolderAliveCurrent,
            Reason::HolderRecordUnavailable,
            Reason::HolderSharedService,
            Reason::HolderMovedOnDirty,
            Reason::TaskLeaseCurrent,
            Reason::OperationGuardBusy,
            Reason::OperationGuardUnavailable,
            Reason::LifecycleBusy,
            Reason::DeadlineReached,
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
                | Reason::HolderUnknown
                | Reason::HolderAliveCurrent
                | Reason::HolderRecordUnavailable
                | Reason::HolderSharedService
                | Reason::HolderMovedOnDirty
                | Reason::TaskLeaseCurrent
                | Reason::OperationGuardBusy
                | Reason::OperationGuardUnavailable
                | Reason::LifecycleBusy
                | Reason::DeadlineReached
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
}
