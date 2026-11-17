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
//! The class is the more protective of what HEAD and the session branch's
//! tip each show, so a detached or switched checkout cannot hide commits.
//!
//! Closing deletes nothing in any class. A session holding work (the last two
//! classes) closes only after a closing-soon broker note has stood for
//! `stale_closing_notice_hours` (default 24) with no activity since; a note
//! that cannot be written keeps the session. A live or unknown holder keeps
//! every class open; that is decided by the close path. `stale_session_auto_close
//! = false` turns the abandonment and no-work windows off.

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
/// `[cleanup]` key: hours a closing-soon note stands before the close.
pub(crate) const NOTICE_KEY: &str = "stale_closing_notice_hours";
/// `[cleanup]` key: `false` keeps every session the abandonment and no-work
/// windows would close.
pub(crate) const AUTO_CLOSE_KEY: &str = "stale_session_auto_close";

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
    /// Every recorded pull request merged and the tip is contained in a
    /// merged pull request's recorded head.
    pub merged_pull_request_contains_tip: bool,
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
    if evidence.merged_pull_request_contains_tip
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
    /// `stale_session_auto_close`.
    pub auto_close: bool,
}

impl Default for StaleSessionPolicy {
    fn default() -> Self {
        Self {
            landed_ms: DEFAULT_LANDED_HOURS * HOUR_MS,
            pushed_ms: DEFAULT_PUSHED_HOURS * HOUR_MS,
            unpushed_ms: DEFAULT_UNPUSHED_HOURS * HOUR_MS,
            notice_ms: DEFAULT_NOTICE_HOURS * HOUR_MS,
            auto_close: true,
        }
    }
}

/// Whether `key` is one of this policy's `[cleanup]` keys.
pub(crate) fn is_policy_key(key: &str) -> bool {
    matches!(
        key,
        LANDED_KEY | PUSHED_KEY | UNPUSHED_KEY | NOTICE_KEY | AUTO_CLOSE_KEY
    )
}

/// Validate one of this policy's `[cleanup]` values.
pub(crate) fn validate_key(key: &str, value: &toml::Value) -> Result<(), String> {
    if key == AUTO_CLOSE_KEY {
        return value
            .as_bool()
            .map(|_| ())
            .ok_or_else(|| format!("[cleanup] {key} must be true or false"));
    }
    parse_hours(key, value).map(|_| ())
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

/// What one class's window and the closing-soon note allow now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaleClassVerdict {
    /// The session may close.
    Due { due_at_ms: i64 },
    /// Not yet; judge it again at `revisit_at_ms`.
    NotDue { due_at_ms: i64, revisit_at_ms: i64 },
    /// It holds work, its notice period has begun and no note stands since
    /// its last activity: leave one, and close no earlier than `due_at_ms`.
    NeedsNotice { due_at_ms: i64 },
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
            validate_key(key, value)?;
            if key == AUTO_CLOSE_KEY {
                policy.auto_close = value.as_bool().unwrap_or(true);
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
    /// `noted_at_ms` is when the latest closing-soon note was left; a note
    /// older than the activity no longer counts. A session holding work
    /// closes only once such a note has stood for the notice period, so a
    /// worker that did not run, a sleeping host or a clock jump never skips
    /// the notice.
    pub(crate) fn judge(
        &self,
        class: StaleWorkClass,
        activity_ms: i64,
        now_ms: i64,
        noted_at_ms: Option<i64>,
    ) -> StaleClassVerdict {
        let window_due = activity_ms.saturating_add(self.window_ms(class));
        if !class.holds_work() || self.notice_ms <= 0 {
            return if now_ms >= window_due {
                StaleClassVerdict::Due {
                    due_at_ms: window_due,
                }
            } else {
                StaleClassVerdict::NotDue {
                    due_at_ms: window_due,
                    revisit_at_ms: window_due,
                }
            };
        }
        let notice_at = window_due.saturating_sub(self.notice_ms);
        if now_ms < notice_at {
            return StaleClassVerdict::NotDue {
                due_at_ms: window_due,
                revisit_at_ms: notice_at,
            };
        }
        match noted_at_ms.filter(|noted| *noted >= activity_ms) {
            None => StaleClassVerdict::NeedsNotice {
                due_at_ms: window_due.max(now_ms.saturating_add(self.notice_ms)),
            },
            Some(noted) => {
                let close_at = window_due.max(noted.saturating_add(self.notice_ms));
                if now_ms >= close_at {
                    StaleClassVerdict::Due {
                        due_at_ms: close_at,
                    }
                } else {
                    StaleClassVerdict::NotDue {
                        due_at_ms: close_at,
                        revisit_at_ms: close_at,
                    }
                }
            }
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

/// Compare-and-set attempts for stale-session bookkeeping: a few, a short
/// pause apart, in the detached worker (nobody waits on it); one anywhere
/// else, so a foreground command never waits on another writer.
fn bookkeeping_attempts() -> usize {
    if crate::file_lock::in_detached_background_worker() {
        5
    } else {
        1
    }
}
const BOOKKEEPING_RETRY_PAUSE: Duration = Duration::from_millis(50);

/// Rewrite `key` with `update(current)` by compare-and-set, never waiting on
/// another database writer and retrying only per [`bookkeeping_attempts`].
/// `update` returns `None` when nothing needs writing. A write lost to a busy
/// database only makes the status report one pass stale.
fn update_without_waiting(
    store: &mut crate::BrokerStore,
    key: &str,
    update: impl Fn(Option<&str>) -> Option<String>,
) {
    let attempts = bookkeeping_attempts();
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(BOOKKEEPING_RETRY_PAUSE);
        }
        let current = match store.meta_get_for_session_retirement(key) {
            Ok(current) => current,
            Err(crate::BrokerError::SessionRetirementDatabaseBusy) => continue,
            Err(error) => {
                crate::warn_unrecorded("read stale-session bookkeeping", Err::<(), _>(error));
                return;
            }
        };
        let Some(value) = update(current.as_deref()) else {
            return;
        };
        match store.meta_compare_and_set_for_session_retirement(key, current.as_deref(), &value) {
            Ok(true) => return,
            Ok(false) | Err(crate::BrokerError::SessionRetirementDatabaseBusy) => {}
            Err(error) => {
                crate::warn_unrecorded("record stale-session bookkeeping", Err::<(), _>(error));
                return;
            }
        }
    }
    crate::phase_telemetry::add("stale_bookkeeping_write_lost", Duration::ZERO);
}

/// Remember `record` for `session_id` (best effort, never waiting on another
/// database writer), keeping the newest [`CLASSES_MAX`] sessions.
pub(crate) fn remember_class(store: &mut crate::BrokerStore, session_id: i64, record: ClassRecord) {
    update_without_waiting(store, CLASSES_META_KEY, |raw| {
        let mut records = raw
            .and_then(|raw| serde_json::from_str::<BTreeMap<i64, ClassRecord>>(raw).ok())
            .unwrap_or_default();
        if records.get(&session_id) == Some(&record) {
            return None;
        }
        records.insert(session_id, record);
        while records.len() > CLASSES_MAX {
            records.pop_first();
        }
        Some(serde_json::to_string(&records).unwrap_or_default())
    });
}

/// Record what a retirement pass learned about the process table: a complete
/// observation, a refusal, or nothing (best effort).
pub(crate) fn record_process_table_observation(
    store: &mut crate::BrokerStore,
    observation: Option<bool>,
    now_ms: i64,
) {
    LAST_PASS_DENIED.with(|denied| denied.set(observation == Some(false)));
    let key = match observation {
        Some(true) => PROCESS_TABLE_OBSERVED_META_KEY,
        Some(false) => PROCESS_TABLE_DENIED_META_KEY,
        None => return,
    };
    update_without_waiting(store, key, |_| Some(now_ms.to_string()));
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

/// When the latest closing-soon note for `session_id` was left, and that
/// note's id while it is unread; `None` when none was ever left.
pub(crate) fn latest_closing_soon_note(
    store: &crate::BrokerStore,
    session_id: i64,
) -> Result<Option<(i64, Option<i64>)>, crate::BrokerError> {
    Ok(store
        .automation_notes(session_id)?
        .into_iter()
        .find(|note| {
            note.sender == crate::session_auto_close::NOTE_SENDER
                && note.fingerprint.as_deref() == Some(CLOSING_SOON_NOTE_FINGERPRINT)
        })
        .map(|note| {
            (
                note.created_at,
                note.acknowledged_at.is_none().then_some(note.id),
            )
        }))
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_NOTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The next closing-soon note write fails, as a full disk or a locked
/// database would make it.
#[cfg(test)]
pub(crate) fn fail_next_closing_soon_note_for_test() {
    FAIL_NEXT_NOTE.with(|fail| fail.set(true));
}

/// Whether a test asked the next closing-soon note write to fail.
pub(crate) fn closing_soon_note_write_fails_for_test() -> bool {
    #[cfg(test)]
    {
        FAIL_NEXT_NOTE.with(std::cell::Cell::take)
    }
    #[cfg(not(test))]
    {
        false
    }
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
        let branch_tip = repo.resolve_ref(&format!("refs/heads/{}", session.branch));
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
            (Some(false), branch_tip.clone())
        };
        let all_merged =
            crate::session_auto_close::PullRequestMerges::read(self.store_ref(), session.id)
                .is_some_and(|merges| merges.all_merged());
        let merged_heads = if all_merged {
            self.store_ref()
                .merged_pull_request_heads(session.id)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let upstream = repo.upstream_default();
        let remote = upstream
            .as_ref()
            .and_then(|(name, _)| name.split_once('/').map(|(remote, _)| remote.to_string()))
            .unwrap_or_else(|| "origin".to_string());
        let remote_tip = repo.resolve_ref(&format!("refs/remotes/{remote}/{}", session.branch));
        let contains = |ancestor: &str, descendant: &str| {
            ancestor == descendant
                || matches!(repo.is_ancestor_checked(ancestor, descendant), Ok(true))
        };
        // HEAD and the branch tip are classed apart and the more protective
        // class wins: a detached HEAD at the base or a landed commit, or a
        // checkout switched to another branch, cannot hide the branch's
        // commits, nor the branch hide commits made on a detached HEAD.
        let class_of = |tip: Option<&str>| -> StaleWorkClass {
            let Some(tip) = tip else {
                return StaleWorkClass::Unpushed;
            };
            classify(&WorkEvidence {
                dirty,
                merged_pull_request_contains_tip: merged_heads
                    .iter()
                    .any(|merged| contains(tip, merged.as_str())),
                head_landed: upstream
                    .as_ref()
                    .and_then(|(_, upstream_tip)| repo.is_ancestor_checked(tip, upstream_tip).ok()),
                no_commits: session.diff_base.as_deref().map(|base| tip == base),
                head_pushed: remote_tip
                    .as_deref()
                    .map(|remote_tip| contains(tip, remote_tip)),
            })
        };
        let class = class_of(head.as_deref()).max(class_of(branch_tip.as_deref()));
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
            merged_pull_request_contains_tip: false,
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
                merged_pull_request_contains_tip: true,
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
                merged_pull_request_contains_tip: true,
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
        // Landed work needs no notice.
        assert_eq!(
            policy.judge(
                StaleWorkClass::Landed,
                activity,
                activity + 24 * HOUR_MS,
                None
            ),
            StaleClassVerdict::Due {
                due_at_ms: activity + 24 * HOUR_MS
            }
        );
        assert_eq!(
            policy.judge(
                StaleWorkClass::Landed,
                activity,
                activity + 23 * HOUR_MS,
                None
            ),
            StaleClassVerdict::NotDue {
                due_at_ms: activity + 24 * HOUR_MS,
                revisit_at_ms: activity + 24 * HOUR_MS,
            }
        );
        // Work is revisited when its notice period begins.
        let due = activity + 168 * HOUR_MS;
        assert_eq!(
            policy.judge(
                StaleWorkClass::Unpushed,
                activity,
                activity + 80 * HOUR_MS,
                None
            ),
            StaleClassVerdict::NotDue {
                due_at_ms: due,
                revisit_at_ms: due - 24 * HOUR_MS,
            }
        );
    }

    #[test]
    fn work_closes_only_after_a_note_has_stood_for_the_notice_period() {
        let policy = StaleSessionPolicy::default();
        let activity = 1_000 * HOUR_MS;
        let due = activity + 72 * HOUR_MS;
        let class = StaleWorkClass::PushedUnmerged;
        // Inside the notice period with no note: leave one, which must then
        // stand a full notice period.
        assert_eq!(
            policy.judge(class, activity, due - 2 * HOUR_MS, None),
            StaleClassVerdict::NeedsNotice {
                due_at_ms: due + 22 * HOUR_MS
            }
        );
        // Long past the window (the worker did not run, the host slept or
        // the clock jumped) with no note: still a note first, and a full
        // notice period from now.
        let late = due + 100 * HOUR_MS;
        assert_eq!(
            policy.judge(class, activity, late, None),
            StaleClassVerdict::NeedsNotice {
                due_at_ms: late + 24 * HOUR_MS
            }
        );
        // A note left late counts from when it was left.
        assert_eq!(
            policy.judge(class, activity, late + 23 * HOUR_MS, Some(late)),
            StaleClassVerdict::NotDue {
                due_at_ms: late + 24 * HOUR_MS,
                revisit_at_ms: late + 24 * HOUR_MS,
            }
        );
        assert_eq!(
            policy.judge(class, activity, late + 24 * HOUR_MS, Some(late)),
            StaleClassVerdict::Due {
                due_at_ms: late + 24 * HOUR_MS
            }
        );
        // A note older than the latest activity no longer counts.
        assert_eq!(
            policy.judge(class, activity, late, Some(activity - 1)),
            StaleClassVerdict::NeedsNotice {
                due_at_ms: late + 24 * HOUR_MS
            }
        );
        // With the notice off, work closes at its window.
        let silent = StaleSessionPolicy {
            notice_ms: 0,
            ..policy
        };
        assert_eq!(
            silent.judge(class, activity, due, None),
            StaleClassVerdict::Due { due_at_ms: due }
        );
    }

    #[test]
    fn the_windows_are_read_from_cleanup_and_validated() {
        let table: toml::Value = toml::from_str(
            "stale_landed_close_after_hours = 12\nstale_pushed_close_after_hours = 48\n\
             stale_unpushed_close_after_hours = 336\nstale_closing_notice_hours = 0\n\
             stale_session_auto_close = false\nauto_remove = true\n",
        )
        .unwrap();
        let policy = StaleSessionPolicy::from_cleanup_table(Some(&table)).unwrap();
        assert_eq!(policy.landed_ms, 12 * HOUR_MS);
        assert_eq!(policy.pushed_ms, 48 * HOUR_MS);
        assert_eq!(policy.unpushed_ms, 336 * HOUR_MS);
        assert_eq!(policy.notice_ms, 0);
        assert!(!policy.auto_close);
        assert!(StaleSessionPolicy::default().auto_close);
        assert_eq!(
            StaleSessionPolicy::from_cleanup_table(None).unwrap(),
            StaleSessionPolicy::default()
        );
        for invalid in [
            "stale_pushed_close_after_hours = 0",
            "stale_unpushed_close_after_hours = 9000",
            "stale_landed_close_after_hours = \"a day\"",
            "stale_closing_notice_hours = -1",
            "stale_session_auto_close = 1",
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
        let (_owner, _storage, mut broker) = repository();
        let store = broker.store();
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
        let (_owner, _storage, mut broker) = repository();
        let store = broker.store();
        let record = ClassRecord {
            class: StaleWorkClass::PushedUnmerged,
            due_at_ms: 7,
        };
        remember_class(store, 42, record);
        assert_eq!(read_class_records(store).get(&42), Some(&record));
    }
}
