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
pub(crate) fn landed_close_payload(
    close_reason: &str,
    idle_ms: i64,
    window_ms: i64,
    pid_alive: Option<bool>,
    retirement_basis: &str,
    proof: &crate::broker::LandedProof,
    pull_requests: &PullRequestMerges,
) -> String {
    json!({
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
    }
    payload.to_string()
}

/// Why the payload says the session was closed, in words.
fn close_reason_text(payload: &Value) -> String {
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

/// The note left for a session the broker just closed automatically.
pub(crate) fn close_note(session_id: i64, payload: &str) -> String {
    let payload = serde_json::from_str::<Value>(payload).unwrap_or(Value::Null);
    format!(
        "The broker closed session {session_id} automatically: {}. Nothing was deleted. \
         To continue, run `aethyme broker start --reuse --task \"<task>\" --short-name \
         \"<name>\"`.",
        close_reason_text(&payload)
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
    let payload = event
        .payload_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or(Value::Null);
    Some(format!(
        "the broker closed session {session_id} automatically because {}; nothing was \
         deleted. Continue with `aethyme broker start --reuse --task \"<task>\" --short-name \
         \"<name>\"`",
        close_reason_text(&payload)
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
        let none = abandonment_close_payload(&base, Some(&merges(&[])), Some("abc"), Some(true));
        let value: Value = serde_json::from_str(&none).unwrap();
        assert_eq!(value["close_reason"], CLOSE_REASON_INACTIVE_NO_PULL_REQUEST);
        assert_eq!(value["unlanded_commits"], true);
        assert_eq!(value["checkout_deleted"], false);
        assert_eq!(value["idle_ms"], 3 * 86_400_000, "existing fields are kept");
        let with_pr =
            abandonment_close_payload(&base, Some(&merges(&[(4, None)])), Some("abc"), None);
        let value: Value = serde_json::from_str(&with_pr).unwrap();
        assert_eq!(value["close_reason"], CLOSE_REASON_INACTIVE);
        let note = close_note(9, &none);
        assert!(note.contains("no pull request was recorded"), "{note}");
        assert!(note.contains("3 days"), "{note}");
        assert!(note.contains("branch and checkout are kept"), "{note}");
        assert!(note.contains("broker start --reuse"), "{note}");
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
        let note = close_note(5, &payload);
        assert!(note.contains("#12"), "{note}");
        assert!(note.contains("0123456789ab"), "{note}");
        assert!(note.contains("3 hours"), "{note}");
        assert!(note.contains("Nothing was deleted"), "{note}");
    }
}

/// The two automatic closes end to end, on a real repository with a local
/// `origin`: the merged-pull-request window and the abandonment window.
#[cfg(all(test, unix))]
mod lane_tests {
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
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder, "fixture", None,
                )),
            )
            .unwrap();
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
}
