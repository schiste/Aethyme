//! How long a stale session whose holder is gone waits before the
//! abandonment window closes it, by what closing it would put at risk.
//!
//! The abandonment window (`[retention] session_abandoned_after_hours`) is
//! the earliest such a session may close. Past it, the session's work decides
//! the rest (`[cleanup]` in `.aethyme/config.toml`):
//!
//! - `landed`: every commit is on the fetched default branch, the session
//!   never committed past its diff base, or every recorded pull request
//!   merged (`stale_landed_close_after_hours`, default 24);
//! - `pushed_unmerged`: the session branch's remote-tracking ref contains
//!   HEAD, but the work has not landed (`stale_pushed_close_after_hours`,
//!   default 72); the close leaves the branch and the pull request alone;
//! - `unpushed`: commits no remote-tracking ref holds, uncommitted or
//!   untracked content, or evidence that could not be read
//!   (`stale_unpushed_close_after_hours`, default 168); the checkout is then
//!   retired only through the recovery-archive path.
//!
//! Closing deletes nothing in any class. A session within
//! `stale_closing_notice_hours` (default 24) of its close in the two
//! classes that hold work gets a broker note and `sessions.closing-soon`.
//! A live or unknown holder keeps every class open; that is decided by the
//! close path before this policy is asked.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// `[cleanup]` key: idle hours before a landed stale session closes.
pub(crate) const LANDED_KEY: &str = "stale_landed_close_after_hours";
/// `[cleanup]` key: idle hours before a pushed but unmerged session closes.
pub(crate) const PUSHED_KEY: &str = "stale_pushed_close_after_hours";
/// `[cleanup]` key: idle hours before a session with unpushed or
/// uncommitted work closes.
pub(crate) const UNPUSHED_KEY: &str = "stale_unpushed_close_after_hours";
/// `[cleanup]` key: hours ahead of a close the note and advice appear.
pub(crate) const NOTICE_KEY: &str = "stale_closing_notice_hours";

pub(crate) const DEFAULT_LANDED_HOURS: i64 = 24;
pub(crate) const DEFAULT_PUSHED_HOURS: i64 = 72;
pub(crate) const DEFAULT_UNPUSHED_HOURS: i64 = 168;
pub(crate) const DEFAULT_NOTICE_HOURS: i64 = 24;

/// Longest window a key accepts: one year.
const MAX_HOURS: i64 = 8_760;
const HOUR_MS: i64 = 3_600_000;

/// Meta key holding the class and due time of every stale session the
/// abandonment window classified, for `broker status`.
pub(crate) const CLASSES_META_KEY: &str = "stale_sessions.classes";
/// Sessions [`CLASSES_META_KEY`] remembers at most (the newest ids).
const CLASSES_MAX: usize = 4_096;
/// Meta keys: when a retirement pass last saw the complete process table,
/// and when one was last refused it.
pub(crate) const PROCESS_TABLE_OBSERVED_META_KEY: &str = "stale_sessions.process_table_observed_at";
pub(crate) const PROCESS_TABLE_DENIED_META_KEY: &str = "stale_sessions.process_table_denied_at";
/// How long a refusal with no complete observation since stays in advice.
pub(crate) const PROCESS_TABLE_DENIED_ADVICE_MS: i64 = 24 * HOUR_MS;

/// Sender and fingerprint of the closing-soon note (one unread at most).
pub(crate) const CLOSING_SOON_NOTE_FINGERPRINT: &str = "session-closing-soon";

/// What closing a stale session would put at risk.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StaleWorkClass {
    Landed,
    PushedUnmerged,
    Unpushed,
}

impl StaleWorkClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::PushedUnmerged => "pushed_unmerged",
            Self::Unpushed => "unpushed",
        }
    }

    /// The `phase_counts` name one classification adds.
    pub(crate) const fn telemetry_name(self) -> &'static str {
        match self {
            Self::Landed => "stale_work_class_landed",
            Self::PushedUnmerged => "stale_work_class_pushed_unmerged",
            Self::Unpushed => "stale_work_class_unpushed",
        }
    }

    /// Whether the session holds work nowhere else on the default branch, so
    /// an agent is told before it closes.
    pub(crate) const fn holds_work(self) -> bool {
        !matches!(self, Self::Landed)
    }

    /// In words, for notes and advice.
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Self::Landed => "its work is on the default branch",
            Self::PushedUnmerged => "its commits are pushed but not merged",
            Self::Unpushed => "it holds unpushed commits or uncommitted work",
        }
    }
}

/// What the classifier read about one session. `None` is evidence that could
/// not be read, which never shortens a window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WorkEvidence {
    /// Uncommitted, untracked or in-progress content in the checkout.
    pub dirty: Option<bool>,
    /// At least one pull request is recorded and every one merged.
    pub all_pull_requests_merged: bool,
    /// HEAD is an ancestor of the fetched default branch.
    pub head_landed: Option<bool>,
    /// HEAD is still the session's diff base.
    pub no_commits: Option<bool>,
    /// HEAD is an ancestor of the session branch's remote-tracking ref.
    pub head_pushed: Option<bool>,
}

/// The class `evidence` puts a session in: any uncommitted or unreadable
/// checkout content is unpushed work, whatever its commits.
pub(crate) fn classify(evidence: &WorkEvidence) -> StaleWorkClass {
    if evidence.dirty != Some(false) {
        return StaleWorkClass::Unpushed;
    }
    if evidence.all_pull_requests_merged
        || evidence.head_landed == Some(true)
        || evidence.no_commits == Some(true)
    {
        return StaleWorkClass::Landed;
    }
    if evidence.head_pushed == Some(true) {
        return StaleWorkClass::PushedUnmerged;
    }
    StaleWorkClass::Unpushed
}

/// The per-class windows, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaleSessionPolicy {
    pub landed_ms: i64,
    pub pushed_ms: i64,
    pub unpushed_ms: i64,
    pub notice_ms: i64,
}

impl Default for StaleSessionPolicy {
    fn default() -> Self {
        Self {
            landed_ms: DEFAULT_LANDED_HOURS * HOUR_MS,
            pushed_ms: DEFAULT_PUSHED_HOURS * HOUR_MS,
            unpushed_ms: DEFAULT_UNPUSHED_HOURS * HOUR_MS,
            notice_ms: DEFAULT_NOTICE_HOURS * HOUR_MS,
        }
    }
}

/// Whether `key` is one of this policy's `[cleanup]` keys.
pub(crate) fn is_policy_key(key: &str) -> bool {
    matches!(key, LANDED_KEY | PUSHED_KEY | UNPUSHED_KEY | NOTICE_KEY)
}

/// The hours one key names, validated: a whole number of hours, at least one
/// for a window (zero allowed for the notice, which then is off), at most a
/// year.
pub(crate) fn parse_hours(key: &str, value: &toml::Value) -> Result<i64, String> {
    let minimum = if key == NOTICE_KEY { 0 } else { 1 };
    match value.as_integer() {
        Some(hours) if (minimum..=MAX_HOURS).contains(&hours) => Ok(hours),
        _ => Err(format!(
            "[cleanup] {key} must be a whole number of hours from {minimum} to {MAX_HOURS}"
        )),
    }
}

/// When one class's window lets the session close, and whether it is close
/// enough to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaleClassVerdict {
    /// Idle past the class window.
    Due { due_at_ms: i64 },
    /// Not yet. `revisit_at_ms` is when to judge it again: the start of the
    /// notice period for a class that holds work, otherwise `due_at_ms`.
    NotDue {
        due_at_ms: i64,
        revisit_at_ms: i64,
        closing_soon: bool,
    },
}

impl StaleSessionPolicy {
    pub(crate) fn window_ms(&self, class: StaleWorkClass) -> i64 {
        match class {
            StaleWorkClass::Landed => self.landed_ms,
            StaleWorkClass::PushedUnmerged => self.pushed_ms,
            StaleWorkClass::Unpushed => self.unpushed_ms,
        }
    }

    /// Read the policy from `[cleanup]` of the primary checkout's config. A
    /// missing file, table or key is the default; an invalid value is an
    /// error, which keeps the session open.
    pub(crate) fn load(main_root: &Path) -> Result<Self, String> {
        let Some((path, text)) =
            aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
                .map_err(|error| error.to_string())?
        else {
            return Ok(Self::default());
        };
        let root: toml::Value = toml::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        Self::from_cleanup_table(root.get("cleanup"))
    }

    /// The policy one parsed `[cleanup]` value describes.
    pub(crate) fn from_cleanup_table(table: Option<&toml::Value>) -> Result<Self, String> {
        let mut policy = Self::default();
        let Some(table) = table else {
            return Ok(policy);
        };
        let table = table
            .as_table()
            .ok_or_else(|| "[cleanup] must be a table".to_string())?;
        for (key, value) in table {
            if !is_policy_key(key) {
                continue;
            }
            let ms = parse_hours(key, value)?.saturating_mul(HOUR_MS);
            match key.as_str() {
                LANDED_KEY => policy.landed_ms = ms,
                PUSHED_KEY => policy.pushed_ms = ms,
                UNPUSHED_KEY => policy.unpushed_ms = ms,
                _ => policy.notice_ms = ms,
            }
        }
        Ok(policy)
    }

    /// Judge a session of `class` whose newest activity is `activity_ms`.
    pub(crate) fn judge(
        &self,
        class: StaleWorkClass,
        activity_ms: i64,
        now_ms: i64,
    ) -> StaleClassVerdict {
        let due_at_ms = activity_ms.saturating_add(self.window_ms(class));
        if now_ms >= due_at_ms {
            return StaleClassVerdict::Due { due_at_ms };
        }
        let notice_at_ms = if class.holds_work() && self.notice_ms > 0 {
            Some(due_at_ms.saturating_sub(self.notice_ms))
        } else {
            None
        };
        match notice_at_ms {
            Some(notice_at) if now_ms < notice_at => StaleClassVerdict::NotDue {
                due_at_ms,
                revisit_at_ms: notice_at,
                closing_soon: false,
            },
            Some(_) => StaleClassVerdict::NotDue {
                due_at_ms,
                revisit_at_ms: due_at_ms,
                closing_soon: true,
            },
            None => StaleClassVerdict::NotDue {
                due_at_ms,
                revisit_at_ms: due_at_ms,
                closing_soon: false,
            },
        }
    }
}

/// One remembered classification, for `broker status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClassRecord {
    pub class: StaleWorkClass,
    pub due_at_ms: i64,
}

/// Every remembered classification by session id; unreadable state is empty.
pub(crate) fn read_class_records(store: &crate::BrokerStore) -> BTreeMap<i64, ClassRecord> {
    store
        .meta_get(CLASSES_META_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<BTreeMap<i64, ClassRecord>>(&raw).ok())
        .unwrap_or_default()
}

/// Remember `record` for `session_id` (best effort), keeping the newest
/// [`CLASSES_MAX`] sessions.
pub(crate) fn remember_class(store: &crate::BrokerStore, session_id: i64, record: ClassRecord) {
    let mut records = read_class_records(store);
    if records.get(&session_id) == Some(&record) {
        return;
    }
    records.insert(session_id, record);
    while records.len() > CLASSES_MAX {
        records.pop_first();
    }
    crate::warn_unrecorded(
        "remember a stale session's work class",
        store.meta_set(
            CLASSES_META_KEY,
            &serde_json::to_string(&records).unwrap_or_default(),
        ),
    );
}

/// Record what a retirement pass learned about the process table: a complete
/// observation, a refusal, or nothing (best effort).
pub(crate) fn record_process_table_observation(
    store: &crate::BrokerStore,
    observation: Option<bool>,
    now_ms: i64,
) {
    LAST_PASS_DENIED.with(|denied| denied.set(observation == Some(false)));
    let key = match observation {
        Some(true) => PROCESS_TABLE_OBSERVED_META_KEY,
        Some(false) => PROCESS_TABLE_DENIED_META_KEY,
        None => return,
    };
    crate::warn_unrecorded(
        "record the retirement pass's process-table observation",
        store.meta_set(key, &now_ms.to_string()),
    );
}

thread_local! {
    /// Whether the last retirement pass on this thread was refused every
    /// process listing it tried.
    static LAST_PASS_DENIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the last retirement pass on this thread was refused the process
/// listing; resets. The detached worker stops its retirement lane on it.
pub(crate) fn last_pass_process_table_denied() -> bool {
    LAST_PASS_DENIED.with(std::cell::Cell::take)
}

fn meta_ms(store: &crate::BrokerStore, key: &str) -> Option<i64> {
    store
        .meta_get(key)
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
}

/// When the process table was last refused with no complete observation
/// since, within [`PROCESS_TABLE_DENIED_ADVICE_MS`]: no retirement pass on
/// this host could judge holders.
pub(crate) fn process_table_denied_since(store: &crate::BrokerStore, now_ms: i64) -> Option<i64> {
    let denied = meta_ms(store, PROCESS_TABLE_DENIED_META_KEY)?;
    let observed = meta_ms(store, PROCESS_TABLE_OBSERVED_META_KEY).unwrap_or(0);
    (denied > observed && now_ms.saturating_sub(denied) <= PROCESS_TABLE_DENIED_ADVICE_MS)
        .then_some(denied)
}

/// The closing-soon note left for one session.
pub(crate) fn closing_soon_note(
    session_id: i64,
    class: StaleWorkClass,
    due_at_ms: i64,
    now_ms: i64,
) -> String {
    let hours = due_at_ms.saturating_sub(now_ms).max(0) / HOUR_MS;
    format!(
        "Session {session_id} has been idle with no live agent and {}; the broker closes it \
         automatically in about {hours} hour(s). Closing deletes nothing: committed work stays \
         on the session branch, a pull request is left as it is, and anything uncommitted goes \
         into a verified recovery archive before the checkout is retired. Any broker command \
         for this session, or a commit in its checkout, resets the idle clock; after a close, \
         `aethyme broker start --reuse` from the checkout (or with its path) resumes it.",
        class.describe()
    )
}

impl crate::Broker {
    /// Classify a stale session's work. Read-only Git, never a fetch: a
    /// remote-tracking ref older than the remote only makes the class more
    /// conservative.
    pub(crate) fn stale_work_class(&self, session: &crate::Session) -> StaleWorkClass {
        let started = std::time::Instant::now();
        let worktree = Path::new(&session.worktree_path);
        let checkout_exists = std::fs::symlink_metadata(worktree).is_ok();
        let repo = self.repo_handle();
        let branch_ref = format!("refs/heads/{}", session.branch);
        // A checkout that is gone holds nothing beyond its branch.
        let (dirty, head) = if checkout_exists {
            let dirty = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
                .ok()
                .and_then(|policy| {
                    crate::auto_cleanup::uncommitted_reason(
                        worktree,
                        Some(session.branch.as_str()),
                        self.main_root(),
                        &policy,
                    )
                    .ok()
                })
                .map(|reason| reason.is_some());
            let head = crate::git::GitRepo::discover(worktree)
                .ok()
                .and_then(|checkout| checkout.head_commit().ok());
            (dirty, head)
        } else {
            (Some(false), repo.resolve_ref(&branch_ref))
        };
        let all_pull_requests_merged =
            crate::session_auto_close::PullRequestMerges::read(self.store_ref(), session.id)
                .is_some_and(|merges| merges.all_merged());
        let upstream = repo.upstream_default();
        let head_landed = match (head.as_deref(), upstream.as_ref()) {
            (Some(head), Some((_, tip))) => repo.is_ancestor_checked(head, tip).ok(),
            _ => None,
        };
        let no_commits = match (head.as_deref(), session.diff_base.as_deref()) {
            (Some(head), Some(base)) => Some(head == base),
            _ => None,
        };
        let remote = upstream
            .as_ref()
            .and_then(|(name, _)| name.split_once('/').map(|(remote, _)| remote.to_string()))
            .unwrap_or_else(|| "origin".to_string());
        let head_pushed = head.as_deref().and_then(|head| {
            let tip = repo.resolve_ref(&format!("refs/remotes/{remote}/{}", session.branch))?;
            repo.is_ancestor_checked(head, &tip).ok()
        });
        let class = classify(&WorkEvidence {
            dirty,
            all_pull_requests_merged,
            head_landed,
            no_commits,
            head_pushed,
        });
        crate::phase_telemetry::add(class.telemetry_name(), Duration::ZERO);
        crate::phase_telemetry::add("stale_work_classification", started.elapsed());
        class
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A committed repository with a local `origin`, and a broker on it.
    fn repository() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Stale policy test"]);
        git(
            &root,
            &["config", "user.email", "stale-policy@example.invalid"],
        );
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&root, &["add", ".gitignore"]);
        git(&root, &["commit", "-qm", "fixture"]);
        aethyme_testkit::ensure_origin(&root);
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        (owner, storage, broker)
    }

    fn git(root: &Path, args: &[&str]) {
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn evidence() -> WorkEvidence {
        WorkEvidence {
            dirty: Some(false),
            all_pull_requests_merged: false,
            head_landed: Some(false),
            no_commits: Some(false),
            head_pushed: Some(false),
        }
    }

    #[test]
    fn landed_commits_a_merged_pull_request_or_no_commits_are_landed() {
        for landed in [
            WorkEvidence {
                head_landed: Some(true),
                ..evidence()
            },
            WorkEvidence {
                all_pull_requests_merged: true,
                ..evidence()
            },
            WorkEvidence {
                no_commits: Some(true),
                ..evidence()
            },
        ] {
            assert_eq!(classify(&landed), StaleWorkClass::Landed, "{landed:?}");
        }
    }

    #[test]
    fn pushed_unlanded_commits_are_pushed_unmerged() {
        let pushed = WorkEvidence {
            head_pushed: Some(true),
            ..evidence()
        };
        assert_eq!(classify(&pushed), StaleWorkClass::PushedUnmerged);
    }

    #[test]
    fn dirty_unreadable_or_unpushed_work_is_unpushed() {
        for unpushed in [
            evidence(),
            WorkEvidence {
                head_pushed: None,
                ..evidence()
            },
            // Uncommitted content outranks a landed HEAD or a merged PR.
            WorkEvidence {
                dirty: Some(true),
                head_landed: Some(true),
                all_pull_requests_merged: true,
                ..evidence()
            },
            WorkEvidence {
                dirty: None,
                no_commits: Some(true),
                ..evidence()
            },
        ] {
            assert_eq!(
                classify(&unpushed),
                StaleWorkClass::Unpushed,
                "{unpushed:?}"
            );
        }
    }

    #[test]
    fn the_default_windows_are_one_three_and_seven_days() {
        let policy = StaleSessionPolicy::default();
        assert_eq!(policy.window_ms(StaleWorkClass::Landed), 24 * HOUR_MS);
        assert_eq!(
            policy.window_ms(StaleWorkClass::PushedUnmerged),
            72 * HOUR_MS
        );
        assert_eq!(policy.window_ms(StaleWorkClass::Unpushed), 168 * HOUR_MS);
        assert_eq!(policy.notice_ms, 24 * HOUR_MS);
    }

    #[test]
    fn a_class_closes_only_past_its_window() {
        let policy = StaleSessionPolicy::default();
        let activity = 1_000 * HOUR_MS;
        assert_eq!(
            policy.judge(
                StaleWorkClass::PushedUnmerged,
                activity,
                activity + 72 * HOUR_MS
            ),
            StaleClassVerdict::Due {
                due_at_ms: activity + 72 * HOUR_MS
            }
        );
        assert!(matches!(
            policy.judge(StaleWorkClass::Unpushed, activity, activity + 100 * HOUR_MS),
            StaleClassVerdict::NotDue { .. }
        ));
    }

    #[test]
    fn work_is_announced_a_notice_period_ahead_and_revisited_then() {
        let policy = StaleSessionPolicy::default();
        let activity = 1_000 * HOUR_MS;
        let due = activity + 168 * HOUR_MS;
        assert_eq!(
            policy.judge(StaleWorkClass::Unpushed, activity, activity + 80 * HOUR_MS),
            StaleClassVerdict::NotDue {
                due_at_ms: due,
                revisit_at_ms: due - 24 * HOUR_MS,
                closing_soon: false,
            }
        );
        assert_eq!(
            policy.judge(StaleWorkClass::Unpushed, activity, due - 2 * HOUR_MS),
            StaleClassVerdict::NotDue {
                due_at_ms: due,
                revisit_at_ms: due,
                closing_soon: true,
            }
        );
        // Landed work is not announced: nothing is lost by its close.
        assert_eq!(
            policy.judge(StaleWorkClass::Landed, activity, activity + 23 * HOUR_MS),
            StaleClassVerdict::NotDue {
                due_at_ms: activity + 24 * HOUR_MS,
                revisit_at_ms: activity + 24 * HOUR_MS,
                closing_soon: false,
            }
        );
    }

    #[test]
    fn the_windows_are_read_from_cleanup_and_validated() {
        let table: toml::Value = toml::from_str(
            "stale_landed_close_after_hours = 12\nstale_pushed_close_after_hours = 48\n\
             stale_unpushed_close_after_hours = 336\nstale_closing_notice_hours = 0\n\
             auto_remove = true\n",
        )
        .unwrap();
        let policy = StaleSessionPolicy::from_cleanup_table(Some(&table)).unwrap();
        assert_eq!(policy.landed_ms, 12 * HOUR_MS);
        assert_eq!(policy.pushed_ms, 48 * HOUR_MS);
        assert_eq!(policy.unpushed_ms, 336 * HOUR_MS);
        assert_eq!(policy.notice_ms, 0);
        assert_eq!(
            StaleSessionPolicy::from_cleanup_table(None).unwrap(),
            StaleSessionPolicy::default()
        );
        for invalid in [
            "stale_pushed_close_after_hours = 0",
            "stale_unpushed_close_after_hours = 9000",
            "stale_landed_close_after_hours = \"a day\"",
            "stale_closing_notice_hours = -1",
        ] {
            let table: toml::Value = toml::from_str(invalid).unwrap();
            assert!(
                StaleSessionPolicy::from_cleanup_table(Some(&table)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn a_refusal_is_advised_until_a_pass_sees_the_table() {
        let (_owner, _storage, broker) = repository();
        let store = broker.store_ref();
        let now = 10 * PROCESS_TABLE_DENIED_ADVICE_MS;
        assert_eq!(process_table_denied_since(store, now), None);
        record_process_table_observation(store, Some(false), now - 10);
        assert!(last_pass_process_table_denied());
        assert!(!last_pass_process_table_denied(), "reading resets it");
        assert_eq!(process_table_denied_since(store, now), Some(now - 10));
        assert_eq!(
            process_table_denied_since(store, now + PROCESS_TABLE_DENIED_ADVICE_MS),
            None,
            "an old refusal stops being advised"
        );
        record_process_table_observation(store, Some(true), now - 5);
        assert_eq!(process_table_denied_since(store, now), None);
        record_process_table_observation(store, None, now);
        assert_eq!(process_table_denied_since(store, now), None);
    }

    #[test]
    fn classifications_are_remembered_per_session() {
        let (_owner, _storage, broker) = repository();
        let store = broker.store_ref();
        let record = ClassRecord {
            class: StaleWorkClass::PushedUnmerged,
            due_at_ms: 7,
        };
        remember_class(store, 42, record);
        assert_eq!(read_class_records(store).get(&42), Some(&record));
    }
}
