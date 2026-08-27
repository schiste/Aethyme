//! Git hook management (`aethyme broker hooks ...`): derived pre-commit
//! gates and the post-commit conflict radar.
//!
//! Hooks are **repo policy**, not broker mechanism — installation is
//! always an explicit `hooks install`, never a side effect of any other
//! command. The installed scripts are thin shims: each is a
//! marker-delimited block embedding the absolute path of the aethyme
//! binary captured at install time, calling back into
//! `aethyme broker hooks pre-commit|post-commit` so the logic stays in
//! this (tested) crate and upgrades with the binary.
//!
//! Placement contract: hooks go in `<git-common-dir>/hooks`, so every
//! linked worktree shares one installation. Ownership contract: a hook
//! file without our marker belongs to the user (or another tool) and is
//! never touched; with our marker, only the marker block is replaced or
//! removed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::error::BrokerError;
use crate::gates::{GateConfigError, load_gates, select_gates};
use crate::git::{GitError, GitRepo};
use crate::store::BrokerStore;

pub const MARKER_BEGIN: &str = "# >>> aethyme hooks >>>";
pub const MARKER_END: &str = "# <<< aethyme hooks <<<";

/// The hooks this module manages, in install/report order.
pub const MANAGED_HOOKS: [&str; 2] = ["pre-commit", "post-commit"];

/// Only gates this cheap run at commit time: the pre-commit hook must
/// stay in the "instant feedback" budget, and everything heavier belongs
/// to `gates run` / `submit` where caching and cancellation apply.
const PRE_COMMIT_MAX_COST: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum HooksError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Store(#[from] BrokerError),
    #[error(transparent)]
    GateConfig(#[from] GateConfigError),
    #[error("hooks i/o at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to replay pre-commit gate {stream}: {source}")]
    ReplayOutput {
        stream: &'static str,
        source: std::io::Error,
    },
    #[error(
        "refusing to install: {path} exists without the aethyme marker — that hook belongs \
         to you (or another tool) and is never clobbered. Move it aside or merge it \
         manually, then re-run `aethyme broker hooks install`."
    )]
    ForeignHook { path: PathBuf },
    #[error(
        "refusing to install: this repository routes hooks through core.hooksPath = \
         {configured:?} (a hook manager like husky?), so scripts written to the default \
         hooks directory would never run. Wire `aethyme broker hooks pre-commit` and \
         `hooks post-commit` into the scripts there yourself, or unset core.hooksPath \
         and re-run `aethyme broker hooks install`."
    )]
    HooksPathOverride { configured: String },
    #[error(
        "gate {gate} failed (exit {code}) — commit blocked. Fix and retry, or bypass once \
         with `git commit --no-verify`."
    )]
    GateFailed { gate: String, code: i32 },
}

impl HooksError {
    /// Original non-zero gate exit code when this is a gate failure.
    pub fn exit_code(&self) -> Option<u8> {
        let Self::GateFailed { code, .. } = self else {
            return None;
        };
        Some(
            u8::try_from(*code)
                .ok()
                .filter(|code| *code != 0)
                .unwrap_or(1),
        )
    }
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> HooksError + '_ {
    move |source| HooksError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Per-hook outcome state, across install/uninstall/status. Serialized
/// as its lowercase string (the --json contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    /// install: freshly written; status: our marker block is present.
    Installed,
    /// install: existing marker block replaced in place.
    Updated,
    /// status/uninstall: no hook file.
    Absent,
    /// status: a hook file exists without our marker (user-owned).
    Foreign,
    /// uninstall: marker block removed, user content kept.
    Removed,
    /// uninstall: file deleted — nothing but our block (and a shebang)
    /// remained.
    Deleted,
}

impl HookState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Absent => "absent",
            Self::Foreign => "foreign",
            Self::Removed => "removed",
            Self::Deleted => "deleted",
        }
    }
}

impl serde::Serialize for HookState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// One hook's report line — what install/uninstall/status render.
#[derive(Debug, serde::Serialize)]
pub struct HookReport {
    pub hook: &'static str,
    pub path: String,
    pub state: HookState,
}

/// Where managed hooks live: `<git-common-dir>/hooks`, shared by all
/// worktrees.
pub fn hooks_dir(repo: &GitRepo) -> Result<PathBuf, HooksError> {
    Ok(repo.git_common_dir()?.join("hooks"))
}

/// The marker-delimited shim for one hook. Embeds the aethyme binary's
/// absolute path (captured at install time). The pre-commit shim blocks
/// on failure but degrades to warn-and-pass when the binary moved —
/// hooks must never brick commits. The post-commit shim is purely
/// informational and swallows everything.
/// POSIX-quote a value for embedding in the generated shims: single
/// quotes, with embedded single quotes as `'\''`. Every other byte is
/// inert — `$`, backticks, double quotes, and backslashes in an install
/// path must never be shell-expanded when the hook runs.
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn hook_block(hook: &str, binary: &Path) -> String {
    let bin = sh_quote(&binary.display().to_string());
    let invoke = if hook == "pre-commit" {
        "if [ -x \"$AETHYME\" ]; then\n    \
             \"$AETHYME\" broker hooks pre-commit || exit $?\n\
         else\n    \
             echo \"aethyme hooks: $AETHYME missing — skipping pre-commit gates\" >&2\n\
         fi"
        .to_string()
    } else {
        format!(
            "if [ -x \"$AETHYME\" ]; then\n    \
                 \"$AETHYME\" broker hooks {hook} || true\n\
             fi"
        )
    };
    format!(
        "{MARKER_BEGIN}\n\
         # Managed by `aethyme broker hooks install` — edits inside the markers are overwritten.\n\
         AETHYME={bin}\n\
         {invoke}\n\
         {MARKER_END}\n"
    )
}

/// Remove the marker block from `text`. `None` when no block is present;
/// `Some(remainder)` otherwise (everything outside the markers, order
/// preserved).
fn strip_marker_block(text: &str) -> Option<String> {
    if !text.contains(MARKER_BEGIN) {
        return None;
    }
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == MARKER_BEGIN {
            inside = true;
            continue;
        }
        if line.trim() == MARKER_END {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

/// Replace the existing marker block in `text` with `block` in place,
/// preserving everything outside the markers.
fn replace_marker_block(text: &str, block: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == MARKER_BEGIN && !inside {
            inside = true;
            out.push_str(block);
            continue;
        }
        if inside {
            if line.trim() == MARKER_END {
                inside = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// True when nothing but whitespace and a shebang remains — the "we can
/// delete the file on uninstall" test.
fn only_shebang_left(text: &str) -> bool {
    text.lines()
        .all(|line| line.trim().is_empty() || line.starts_with("#!"))
}

fn write_executable(path: &Path, content: &str) -> Result<(), HooksError> {
    std::fs::write(path, content).map_err(io_err(path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(io_err(path))?;
    }
    Ok(())
}

/// Install (or refresh) both managed hooks. All-or-nothing: a foreign
/// hook file refuses the whole install before anything is written, so a
/// partial policy never lands silently.
pub fn install(repo: &GitRepo, binary: &Path) -> Result<Vec<HookReport>, HooksError> {
    let dir = hooks_dir(repo)?;
    std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;

    // Preflight: a core.hooksPath override (husky and friends) reroutes
    // hook lookup away from <common>/hooks entirely — installing there
    // would report success while the hooks silently never run. Allowed
    // only when the override resolves to the default dir itself.
    if let Some(configured) = repo.config_get("core.hooksPath") {
        // Relative core.hooksPath is taken relative to the checkout root
        // (where git runs hooks from).
        let resolved = {
            let path = PathBuf::from(&configured);
            if path.is_relative() {
                repo.root().join(path)
            } else {
                path
            }
        };
        let same_dir = match (resolved.canonicalize(), dir.canonicalize()) {
            (Ok(resolved), Ok(dir)) => resolved == dir,
            _ => false,
        };
        if !same_dir {
            return Err(HooksError::HooksPathOverride { configured });
        }
    }

    // Preflight: refuse before the first write.
    for hook in MANAGED_HOOKS {
        let path = dir.join(hook);
        if let Ok(existing) = std::fs::read_to_string(&path)
            && !existing.contains(MARKER_BEGIN)
        {
            return Err(HooksError::ForeignHook { path });
        }
    }

    let mut reports = Vec::new();
    for hook in MANAGED_HOOKS {
        let path = dir.join(hook);
        let block = hook_block(hook, binary);
        let state = match std::fs::read_to_string(&path) {
            Ok(existing) => {
                write_executable(&path, &replace_marker_block(&existing, &block))?;
                HookState::Updated
            }
            Err(_) => {
                write_executable(&path, &format!("#!/bin/sh\n{block}"))?;
                HookState::Installed
            }
        };
        reports.push(HookReport {
            hook,
            path: path.to_string_lossy().into_owned(),
            state,
        });
    }
    Ok(reports)
}

/// Remove the marker blocks. A hook file that was nothing but our shim
/// is deleted; user content outside the markers is preserved; files
/// without our marker are left untouched.
pub fn uninstall(repo: &GitRepo) -> Result<Vec<HookReport>, HooksError> {
    let dir = hooks_dir(repo)?;
    let mut reports = Vec::new();
    for hook in MANAGED_HOOKS {
        let path = dir.join(hook);
        let state = match std::fs::read_to_string(&path) {
            Err(_) => HookState::Absent,
            Ok(existing) => match strip_marker_block(&existing) {
                None => HookState::Foreign,
                Some(remainder) if only_shebang_left(&remainder) => {
                    std::fs::remove_file(&path).map_err(io_err(&path))?;
                    HookState::Deleted
                }
                Some(remainder) => {
                    write_executable(&path, &remainder)?;
                    HookState::Removed
                }
            },
        };
        reports.push(HookReport {
            hook,
            path: path.to_string_lossy().into_owned(),
            state,
        });
    }
    Ok(reports)
}

/// Report installed/absent/foreign per managed hook.
pub fn status(repo: &GitRepo) -> Result<Vec<HookReport>, HooksError> {
    let dir = hooks_dir(repo)?;
    let mut reports = Vec::new();
    for hook in MANAGED_HOOKS {
        let path = dir.join(hook);
        let state = match std::fs::read_to_string(&path) {
            Err(_) => HookState::Absent,
            Ok(existing) if existing.contains(MARKER_BEGIN) => HookState::Installed,
            Ok(_) => HookState::Foreign,
        };
        reports.push(HookReport {
            hook,
            path: path.to_string_lossy().into_owned(),
            state,
        });
    }
    Ok(reports)
}

/// The pre-commit shim's target: run the cost≤1 gates whose triggers
/// match the staged files, in the checkout the commit is happening in.
/// No gates config, or no matching gates, is an instant pass; the first
/// failing gate blocks the commit and is named in the error. Successful
/// gate output stays quiet; a failing gate's complete stdout and stderr
/// are replayed before the actionable error.
pub fn run_pre_commit(cwd: &Path) -> Result<(), HooksError> {
    let checkout = GitRepo::discover(cwd)?;
    let main_root = checkout.main_root()?;
    let gates = match load_gates(&main_root) {
        // No config = no commit-time policy. A *broken* config still
        // blocks: silently skipping validation would be worse.
        Err(GateConfigError::Missing(_)) => return Ok(()),
        other => other?,
    };
    let cheap: Vec<_> = gates
        .into_iter()
        .filter(|gate| gate.cost <= PRE_COMMIT_MAX_COST)
        .collect();
    let staged = checkout.staged_files()?;
    // load_gates sorts cheap-first, so selections run in cost order.
    for selection in select_gates(&cheap, &staged) {
        let gate = selection.gate;
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&gate.command)
            .current_dir(checkout.root())
            .output()
            .map_err(io_err(checkout.root()))?;
        if !output.status.success() {
            replay_gate_output(&output)?;
            return Err(HooksError::GateFailed {
                gate: gate.name.clone(),
                code: output.status.code().unwrap_or(-1),
            });
        }
    }
    Ok(())
}

fn replay_gate_output(output: &std::process::Output) -> Result<(), HooksError> {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    stdout
        .write_all(&output.stdout)
        .and_then(|()| stdout.flush())
        .map_err(|source| HooksError::ReplayOutput {
            stream: "stdout",
            source,
        })?;

    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    stderr
        .write_all(&output.stderr)
        .and_then(|()| stderr.flush())
        .map_err(|source| HooksError::ReplayOutput {
            stream: "stderr",
            source,
        })?;
    Ok(())
}

/// The post-commit shim's target: print the conflict-radar warnings (if
/// any) to stderr. Purely informational — errors are swallowed and the
/// caller always exits 0, because a radar must never block a commit.
pub fn run_post_commit(cwd: &Path) {
    if let Ok(warnings) = conflict_radar(cwd) {
        for line in warnings {
            eprintln!("{line}");
        }
    }
    if let Ok(notices) = session_advisory_notices(cwd) {
        for line in notices {
            eprintln!("{line}");
        }
    }
}

/// Read-only advisory notices for the live session owning this worktree.
/// Called after the conflict radar so post-commit output reflects the new
/// commit first and durable integration drift second.
pub fn session_advisory_notices(cwd: &Path) -> Result<Vec<String>, HooksError> {
    let checkout = GitRepo::discover(cwd)?;
    let main_root = checkout.main_root()?;
    if !main_root.join(crate::BROKER_DB_RELPATH).exists() {
        return Ok(Vec::new());
    }
    let store = BrokerStore::open_snapshot_in_repo(&main_root)?;
    let Some(session) = store.session_for_worktree(checkout.root().to_string_lossy().as_ref())?
    else {
        return Ok(Vec::new());
    };
    let advisories = store.outstanding_advisories_for_session(session.id)?;
    Ok(crate::advisories::session_notice_lines(&advisories))
}

/// Compare the files changed in HEAD against other live sessions'
/// leases and describe each overlapping session. Silent (empty) when the
/// broker database does not exist — the radar only speaks where the
/// broker is actually in use.
pub fn conflict_radar(cwd: &Path) -> Result<Vec<String>, HooksError> {
    let checkout = GitRepo::discover(cwd)?;
    let main_root = checkout.main_root()?;
    if !main_root.join(crate::BROKER_DB_RELPATH).exists() {
        return Ok(Vec::new());
    }
    let changed = checkout.head_changed_files()?;
    if changed.is_empty() {
        return Ok(Vec::new());
    }

    let store = BrokerStore::open_in_repo(&main_root)?;
    // Exclude this checkout's own session: its leases naturally cover
    // the files it just committed. Worktree paths are stored exactly as
    // `GitRepo::root()` renders them, so string equality is the match.
    let own_worktree = checkout.root().to_string_lossy().into_owned();
    let sessions: HashMap<i64, crate::types::Session> = store
        .live_sessions()?
        .into_iter()
        .filter(|session| session.worktree_path != own_worktree)
        .map(|session| (session.id, session))
        .collect();

    let mut per_session: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for lease in store.active_leases()? {
        if !sessions.contains_key(&lease.session_id) {
            continue;
        }
        for file in &changed {
            if crate::leases::paths_overlap(&lease.path, file) {
                per_session
                    .entry(lease.session_id)
                    .or_default()
                    .insert(file.clone());
            }
        }
    }

    let mut warnings = Vec::new();
    for (session_id, files) in per_session {
        let task = sessions[&session_id]
            .task
            .as_deref()
            .map(|task| format!(" ({task:?})"))
            .unwrap_or_default();
        let files: Vec<String> = files.into_iter().collect();
        warnings.push(format!(
            "⚠ aethyme conflict radar: session {session_id}{task} is also editing files \
             this commit touches: {}",
            files.join(", ")
        ));
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_block_strip_and_replace_preserve_user_content() {
        let block = hook_block("pre-commit", Path::new("/usr/bin/aethyme"));
        let user = format!("#!/bin/sh\necho before\n{block}echo after\n");

        let stripped = strip_marker_block(&user).unwrap();
        assert_eq!(stripped, "#!/bin/sh\necho before\necho after\n");
        assert!(strip_marker_block("#!/bin/sh\necho mine\n").is_none());

        let newer = hook_block("pre-commit", Path::new("/opt/aethyme"));
        let replaced = replace_marker_block(&user, &newer);
        assert!(replaced.contains("/opt/aethyme"));
        assert!(!replaced.contains("/usr/bin/aethyme"));
        assert!(replaced.starts_with("#!/bin/sh\necho before\n"));
        assert!(replaced.ends_with("echo after\n"));
        assert_eq!(replaced.matches(MARKER_BEGIN).count(), 1);
    }

    #[test]
    fn binary_path_is_inert_in_the_generated_shim() {
        // The path is embedded in a shell script: `$`, backticks, quotes
        // and backslashes in an install location must never be expanded
        // or executed when the hook runs. Round-trip through a real sh.
        let hostile = r#"/tmp/$(touch pwned)/it's "aethyme" `id` \x"#;
        let quoted = sh_quote(hostile);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), hostile);

        let block = hook_block("pre-commit", Path::new(hostile));
        assert!(
            block.contains(&format!("AETHYME={quoted}")),
            "shim embeds the sh-quoted path: {block}"
        );
    }

    #[test]
    fn only_shebang_left_gates_file_deletion() {
        assert!(only_shebang_left("#!/bin/sh\n\n"));
        assert!(only_shebang_left(""));
        assert!(!only_shebang_left("#!/bin/sh\necho user\n"));
    }
}
