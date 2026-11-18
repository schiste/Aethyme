//! Where the detached background worker reads `[cleanup]` from.
//!
//! Every `[cleanup]` reader used to read the primary checkout's working copy
//! of `.aethyme/config.toml`. A primary checkout can be held far behind its
//! default branch (its sync is refused while a live session or a local change
//! holds it), and then a `[cleanup]` setting merged on the default branch
//! never takes effect: on the busiest repository a `venv_markers` entry
//! merged hours earlier was not in effect because the primary checkout was
//! about 5,000 commits behind.
//!
//! A worker run now resolves the source once, before anything reads
//! `[cleanup]`, and installs it process-wide (worker threads see it too,
//! unlike the thread-local Git deadline and isolation scopes). It reads the
//! default branch as last fetched (never fetching) only when all of these
//! hold:
//!
//! - the primary checkout's config file is byte-identical to the blob at its
//!   own `HEAD` (no local owner edit);
//! - the default branch holds a different config blob;
//! - `HEAD` is an ancestor of the default branch (the checkout is behind,
//!   not diverged);
//! - the default-branch config parses, and its `[cleanup]` table validates.
//!
//! Anything else (a local edit, divergence, no fetched default branch, a Git
//! error or timeout, an unreadable or invalid blob) keeps the working copy,
//! as before. The lookup never fails cleanup. Outside a worker run (every
//! foreground command) readers keep reading the working copy.
//!
//! Each run counts its source and reason in the command's phase metrics
//! (`cleanup_config_source_default_branch`,
//! `cleanup_config_source_working_copy_<reason>`), times the lookup
//! (`cleanup_config_resolve`), and records the result for `broker status`,
//! which shows advice `cleanup.config-source` when the working copy was kept
//! although the default branch differs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most the whole lookup may take; Git calls past it time out and the
/// working copy is kept.
const RESOLVE_BUDGET: Duration = Duration::from_secs(5);
/// Broker meta key: the last worker run's resolution
/// ([`RecordedResolution`]).
pub(crate) const SUMMARY_KEY: &str = "cleanup_config.last_resolution";
/// Status advice id.
pub(crate) const ADVICE_ID: &str = "cleanup.config-source";

/// The config text the run reads `[cleanup]` from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CleanupConfigSource {
    /// The primary checkout's working copy, read afresh by each reader.
    WorkingCopy,
    /// The default branch as last fetched: its config as TOML text (`None`
    /// when it holds no config file), and a label for messages.
    DefaultBranch {
        text: Option<String>,
        label: PathBuf,
    },
}

/// One resolution and why it came out that way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanupConfigResolution {
    pub(crate) source: CleanupConfigSource,
    /// `default_branch`, or why the working copy was kept: `same`,
    /// `no_default_branch`, `default_branch_unreadable`, `head_unreadable`,
    /// `git_timeout`, `working_copy_unverifiable`, `local_edit`, `diverged`
    /// or `default_branch_unparsable`.
    pub(crate) reason: &'static str,
    /// The default branch was seen to hold a different config than `HEAD`.
    pub(crate) default_differs: bool,
    /// The fetched default branch (`origin/main`), when one was found.
    pub(crate) default_ref: Option<String>,
    pub(crate) default_commit: Option<String>,
}

impl CleanupConfigResolution {
    fn working_copy(reason: &'static str) -> Self {
        Self {
            source: CleanupConfigSource::WorkingCopy,
            reason,
            default_differs: false,
            default_ref: None,
            default_commit: None,
        }
    }

    /// Whether the run reads the default branch.
    pub(crate) fn uses_default_branch(&self) -> bool {
        matches!(self.source, CleanupConfigSource::DefaultBranch { .. })
    }

    /// The phase counter naming this resolution's source and reason.
    pub(crate) fn counter(&self) -> &'static str {
        if self.uses_default_branch() {
            return "cleanup_config_source_default_branch";
        }
        match self.reason {
            "same" => "cleanup_config_source_working_copy_same",
            "no_default_branch" => "cleanup_config_source_working_copy_no_default_branch",
            "default_branch_unreadable" => {
                "cleanup_config_source_working_copy_default_branch_unreadable"
            }
            "head_unreadable" => "cleanup_config_source_working_copy_head_unreadable",
            "git_timeout" => "cleanup_config_source_working_copy_git_timeout",
            "working_copy_unverifiable" => {
                "cleanup_config_source_working_copy_working_copy_unverifiable"
            }
            "local_edit" => "cleanup_config_source_working_copy_local_edit",
            "diverged" => "cleanup_config_source_working_copy_diverged",
            "default_branch_unparsable" => {
                "cleanup_config_source_working_copy_default_branch_unparsable"
            }
            _ => "cleanup_config_source_working_copy_other",
        }
    }
}

/// The reason a Git failure keeps the working copy.
fn git_reason(error: &crate::GitError, otherwise: &'static str) -> &'static str {
    if matches!(error, crate::GitError::TimedOut { .. }) {
        "git_timeout"
    } else {
        otherwise
    }
}

/// Decide where `[cleanup]` comes from, uncached, within
/// [`RESOLVE_BUDGET`]. At most about eight Git calls, all read-only and
/// without optional locks; nothing is fetched.
pub(crate) fn resolve(main_root: &Path) -> CleanupConfigResolution {
    crate::git::with_git_deadline(RESOLVE_BUDGET, || resolve_unbounded(main_root))
}

fn resolve_unbounded(main_root: &Path) -> CleanupConfigResolution {
    let repo = crate::GitRepo::at_known_root(main_root);
    let (default_ref, default_commit) = match repo.upstream_default_checked() {
        Ok(Some(found)) => found,
        Ok(None) => return CleanupConfigResolution::working_copy("no_default_branch"),
        Err(error) => {
            return CleanupConfigResolution::working_copy(git_reason(
                &error,
                "default_branch_unreadable",
            ));
        }
    };
    let mut resolution = CleanupConfigResolution {
        default_ref: Some(default_ref.clone()),
        default_commit: Some(default_commit.clone()),
        ..CleanupConfigResolution::working_copy("same")
    };
    let keep = |mut resolution: CleanupConfigResolution, reason: &'static str| {
        resolution.reason = reason;
        resolution
    };
    let head = match repo.head_commit() {
        Ok(head) => head,
        Err(error) => return keep(resolution, git_reason(&error, "head_unreadable")),
    };
    // Every format twin (#1223), at HEAD and at the default branch, in one
    // batch: a twin added, removed or changed anywhere counts.
    let relpaths = aethyme_config::candidate_relpaths("config");
    let queries = relpaths
        .iter()
        .map(|relpath| (head.as_str(), relpath.as_str()))
        .chain(
            relpaths
                .iter()
                .map(|relpath| (default_commit.as_str(), relpath.as_str())),
        )
        .collect::<Vec<_>>();
    let objects = match repo.objects_at_many(&queries) {
        Ok(objects) if objects.len() == queries.len() => objects,
        Ok(_) => return keep(resolution, "default_branch_unreadable"),
        Err(error) => return keep(resolution, git_reason(&error, "default_branch_unreadable")),
    };
    let (at_head, at_default) = objects.split_at(relpaths.len());
    if at_head == at_default {
        return resolution;
    }
    resolution.default_differs = true;
    // The working copy, by the blob id of its exact bytes.
    let mut working = Vec::with_capacity(relpaths.len());
    for relpath in &relpaths {
        let path = main_root.join(relpath);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => working.push(None),
            Ok(metadata) if metadata.is_file() => match repo.hash_file_bytes(&path) {
                Ok(blob) => working.push(Some(blob)),
                Err(error) => {
                    return keep(resolution, git_reason(&error, "working_copy_unverifiable"));
                }
            },
            _ => return keep(resolution, "working_copy_unverifiable"),
        }
    }
    if working.as_slice() == at_default {
        // Edited or not, the working copy already says what the default
        // branch says.
        resolution.default_differs = false;
        return resolution;
    }
    if working.as_slice() != at_head {
        return keep(resolution, "local_edit");
    }
    match repo.is_ancestor_checked(&head, &default_commit) {
        Ok(true) => {}
        Ok(false) => return keep(resolution, "diverged"),
        Err(error) => return keep(resolution, git_reason(&error, "default_branch_unreadable")),
    }
    let text = match repo.config_at_resolved_commit_checked(&default_commit, "config") {
        Ok(Some(text)) => match text.toml_text() {
            Ok(toml) => Some(toml.into_owned()),
            Err(_) => return keep(resolution, "default_branch_unparsable"),
        },
        Ok(None) => None,
        Err(crate::GitError::Config(_)) => return keep(resolution, "default_branch_unparsable"),
        Err(error) => return keep(resolution, git_reason(&error, "default_branch_unreadable")),
    };
    let label = PathBuf::from(format!(
        "{default_ref}:{}/config",
        aethyme_config::CONFIG_DIR
    ));
    if let Some(text) = &text
        && validate_cleanup_table(&label, text).is_err()
    {
        return keep(resolution, "default_branch_unparsable");
    }
    resolution.source = CleanupConfigSource::DefaultBranch { text, label };
    resolution.reason = "default_branch";
    resolution
}

/// Whether `text` parses and its `[cleanup]` table is one every reader
/// accepts (the same validation [`crate::AutoCleanupPolicy::load`] applies).
fn validate_cleanup_table(label: &Path, text: &str) -> Result<(), String> {
    crate::AutoCleanupPolicy::from_config_toml(label, text).map(|_| ())
}

/// Resolutions installed by active [`RunScope`]s: (canonical root,
/// resolution, scopes holding it).
type Installed = Vec<(PathBuf, Arc<CleanupConfigResolution>, usize)>;
static INSTALLED: Mutex<Installed> = Mutex::new(Vec::new());

fn installed_for(main_root: &Path) -> Option<Arc<CleanupConfigResolution>> {
    let installed = INSTALLED.lock().ok()?;
    if installed.is_empty() {
        return None;
    }
    if let Some((_, resolution, _)) = installed.iter().find(|(root, _, _)| root == main_root) {
        return Some(resolution.clone());
    }
    let canonical = main_root.canonicalize().ok()?;
    installed
        .iter()
        .find(|(root, _, _)| *root == canonical)
        .map(|(_, resolution, _)| resolution.clone())
}

/// One worker run's `[cleanup]` source for one repository, visible to every
/// thread of the process until dropped. Nested scopes for the same root
/// share the first resolution.
pub(crate) struct RunScope {
    root: PathBuf,
}

impl RunScope {
    /// Resolve once (counted and timed in the phase metrics) and install it.
    pub(crate) fn enter(main_root: &Path) -> Self {
        let root = main_root
            .canonicalize()
            .unwrap_or_else(|_| main_root.to_path_buf());
        if let Ok(mut installed) = INSTALLED.lock()
            && let Some(entry) = installed
                .iter_mut()
                .find(|(existing, _, _)| *existing == root)
        {
            entry.2 += 1;
            return Self { root };
        }
        let resolution = {
            let _span = crate::phase_telemetry::span("cleanup_config_resolve");
            resolve(&root)
        };
        crate::phase_telemetry::add(resolution.counter(), Duration::ZERO);
        let source = if resolution.uses_default_branch() {
            "default_branch"
        } else {
            "working_copy"
        };
        crate::phase_telemetry::attach(
            "cleanup_config",
            serde_json::json!({
                "source": source,
                "reason": resolution.reason,
                "default_differs": resolution.default_differs,
            }),
        );
        if let Ok(mut installed) = INSTALLED.lock() {
            installed.push((root.clone(), Arc::new(resolution), 1));
        }
        Self { root }
    }
}

impl Drop for RunScope {
    fn drop(&mut self) {
        if let Ok(mut installed) = INSTALLED.lock()
            && let Some(index) = installed.iter().position(|(root, _, _)| *root == self.root)
        {
            installed[index].2 -= 1;
            if installed[index].2 == 0 {
                installed.remove(index);
            }
        }
    }
}

/// The `.aethyme/config` text every `[cleanup]` reader parses, as TOML, with
/// the path (or label) it came from: the default branch when a worker run
/// resolved it so, the primary checkout's working copy otherwise (exactly
/// [`aethyme_config::read_config_toml`]). Only the `[cleanup]` table may be
/// read from it.
pub(crate) fn cleanup_config_toml(
    main_root: &Path,
) -> Result<Option<(PathBuf, String)>, aethyme_config::ConfigSourceError> {
    if let Some(resolution) = installed_for(main_root)
        && let CleanupConfigSource::DefaultBranch { text, label } = &resolution.source
    {
        return Ok(text.as_ref().map(|text| (label.clone(), text.clone())));
    }
    aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
}

/// What `broker status` reads back about the last worker run's resolution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecordedResolution {
    pub(crate) at: i64,
    /// `default_branch` or `working_copy`.
    pub(crate) source: String,
    pub(crate) reason: String,
    pub(crate) default_differs: bool,
    pub(crate) default_ref: Option<String>,
    pub(crate) default_commit: Option<String>,
}

/// Record the resolution an active [`RunScope`] installed for `main_root`,
/// for `broker status`. Nothing when no scope is active.
pub(crate) fn record_active(store: &crate::BrokerStore, main_root: &Path) {
    let Some(resolution) = installed_for(main_root) else {
        return;
    };
    let source = if resolution.uses_default_branch() {
        "default_branch"
    } else {
        "working_copy"
    };
    let recorded = RecordedResolution {
        at: crate::clock::epoch_ms(),
        source: source.into(),
        reason: resolution.reason.into(),
        default_differs: resolution.default_differs,
        default_ref: resolution.default_ref.clone(),
        default_commit: resolution.default_commit.clone(),
    };
    crate::warn_unrecorded(
        "record the [cleanup] config source",
        serde_json::to_string(&recorded)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                store
                    .meta_set(SUMMARY_KEY, &text)
                    .map_err(|error| error.to_string())
            }),
    );
}

/// `cleanup.config-source`: the last worker run kept the working copy's
/// `[cleanup]` although the default branch holds a different config. Reads
/// the recorded resolution only.
pub(crate) fn status_advice(store: &crate::BrokerStore) -> Option<crate::StatusAdvice> {
    let recorded = store
        .meta_get(SUMMARY_KEY)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<RecordedResolution>(&text).ok())?;
    if recorded.source != "working_copy" || !recorded.default_differs {
        return None;
    }
    let default_ref = recorded
        .default_ref
        .clone()
        .unwrap_or_else(|| "the default branch".into());
    let command = match recorded.reason.as_str() {
        "local_edit" => format!(
            "commit or discard the local edit to {}/config.toml in the primary checkout; background cleanup then follows {default_ref}",
            aethyme_config::CONFIG_DIR
        ),
        "diverged" => format!(
            "bring the primary checkout's branch back onto {default_ref}: it has commits {default_ref} lacks"
        ),
        "default_branch_unparsable" => format!(
            "fix the {}/config file on {default_ref}: it does not parse, or its [cleanup] table is invalid",
            aethyme_config::CONFIG_DIR
        ),
        _ => "nothing to do if the next worker run reads the default branch; the lookup did not finish".into(),
    };
    let mut evidence = vec![
        format!("reason: {}", recorded.reason),
        format!("recorded at: {}", recorded.at),
    ];
    if let Some(commit) = &recorded.default_commit {
        evidence.push(format!("{default_ref} at {commit}"));
    }
    Some(crate::StatusAdvice {
        id: ADVICE_ID,
        severity: crate::StatusAdviceSeverity::Notice,
        reason: "background cleanup reads [cleanup] from the default branch only when the primary checkout's config is unedited and the checkout is simply behind",
        summary: format!(
            "background cleanup kept the primary checkout's [cleanup] settings although {default_ref} has a different config ({})",
            recorded.reason
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec![command],
    })
}

#[cfg(test)]
mod tests;
