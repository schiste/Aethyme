//! Read-only Git queries answered in-process by gitoxide (`gix`), with the
//! `git` program behind every answer gitoxide cannot prove equal.
//!
//! On the busiest repository, 0.8.162 started 323,258 Git processes in about
//! 13 hours, most of them one-line reads such as `rev-parse --verify` and
//! `merge-base --is-ancestor`. A process costs milliseconds on a loaded
//! machine; the same read in-process costs microseconds.
//!
//! Each query has two implementations of [`GitRead`]: [`InProcess`] here and
//! `ProgramRead` in `git.rs`, which keeps the exact command the broker ran
//! before. [`answer`] tries the in-process one and runs the program on any
//! [`Fallback`], so an in-process failure is never an answer. The rules that
//! keep the two equal:
//!
//! - Only a positive ref resolution is answered here. A name that resolves to
//!   nothing goes to Git, which also knows short object ids, reflog syntax
//!   and pseudo refs.
//! - A name is resolved by Git's own rule order (`<name>`, `refs/<name>`,
//!   `refs/tags/<name>`, `refs/heads/<name>`, `refs/remotes/<name>`,
//!   `refs/remotes/<name>/HEAD`), each candidate looked up exactly.
//! - `packed-refs` must be the same file before and after the lookup, so a
//!   concurrent `pack-refs` or ref deletion falls back instead of reading a
//!   ref as missing.
//! - A missing object is a fallback, never "absent" or "not contained": the
//!   build has no network features, so a partial clone is never fetched.
//! - Grafts, replace refs, shallow history (for ancestry), SHA-256, non-file
//!   ref storage, reduced trust, inherited `GIT_*` routing or configuration
//!   and an expired Git deadline all fall back, preserving each call site's
//!   current semantics.
//!
//! `AETHYME_GIT_IN_PROCESS=0` (or `false`, `off`, `no`) and `[git] in_process
//! = false` in the main checkout's `.aethyme/config.toml` force the program
//! everywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use crate::git::GitError;

/// Environment kill switch: `0`, `false`, `off` or `no` forces the program.
pub(crate) const IN_PROCESS_ENV: &str = "AETHYME_GIT_IN_PROCESS";

/// Git follows at most five symbolic refs (`SYMREF_MAXDEPTH`).
const MAX_SYMREF_DEPTH: usize = 5;
/// Tag chains longer than this go to Git rather than being followed here.
const MAX_TAG_DEPTH: usize = 32;
/// Names longer than this go to Git.
const MAX_NAME_BYTES: usize = 1024;

/// One kind of read-only query, the key of its counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadKind {
    /// `rev-parse --verify --quiet <spec>^{commit}`
    ResolveCommit,
    /// `rev-parse HEAD`
    HeadCommit,
    /// `rev-parse <spec>^{tree}`
    CommitTree,
    /// `merge-base --is-ancestor <ancestor> <descendant>`
    IsAncestor,
}

impl ReadKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ResolveCommit => "resolve_commit",
            Self::HeadCommit => "head_commit",
            Self::CommitTree => "commit_tree",
            Self::IsAncestor => "is_ancestor",
        }
    }
}

/// Why a query went to the `git` program instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// The kill switch is set (environment or `[git] in_process`).
    Disabled,
    /// Inherited `GIT_*` routing or configuration, or isolated graph Git.
    Environment,
    /// The enclosing Git deadline has expired: Git reports the timeout.
    Deadline,
    /// gitoxide could not open the repository.
    Open,
    /// The repository is not fully trusted (ownership, `safe.directory`).
    Untrusted,
    /// Not a SHA-1 repository with files ref storage.
    Format,
    /// Grafts, replace refs, or shallow history for an ancestry query.
    HistoryOverride,
    /// Revision syntax this module does not parse.
    UnsupportedSpec,
    /// No candidate ref exists: Git decides (short ids, absence).
    NotFound,
    /// A file in the Git directory shares the name, which Git may read.
    RootFileShadow,
    /// A dangling or too deep symbolic ref.
    Unresolved,
    /// An object is not in the local object database.
    ObjectMissing,
    /// The object does not peel to a commit.
    NotACommit,
    /// `packed-refs` changed during the lookup.
    ConcurrentRefUpdate,
    /// Any other gitoxide error.
    Gix,
    /// gitoxide panicked.
    Panic,
}

impl Fallback {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Environment => "environment",
            Self::Deadline => "deadline",
            Self::Open => "open",
            Self::Untrusted => "untrusted",
            Self::Format => "format",
            Self::HistoryOverride => "history_override",
            Self::UnsupportedSpec => "unsupported_spec",
            Self::NotFound => "not_found",
            Self::RootFileShadow => "root_file_shadow",
            Self::Unresolved => "unresolved",
            Self::ObjectMissing => "object_missing",
            Self::NotACommit => "not_a_commit",
            Self::ConcurrentRefUpdate => "concurrent_ref_update",
            Self::Gix => "gix",
            Self::Panic => "panic",
        }
    }
}

/// The read-only queries with an in-process implementation. `ProgramRead`
/// (in `git.rs`) runs the exact Git command named on each method.
pub(crate) trait GitRead {
    type Error;
    /// `git rev-parse --verify --quiet <spec>^{commit}`.
    fn resolve_commit(&self, spec: &str) -> Result<String, Self::Error>;
    /// `git rev-parse HEAD`.
    fn head_commit(&self) -> Result<String, Self::Error>;
    /// `git rev-parse <spec>^{tree}`.
    fn commit_tree(&self, spec: &str) -> Result<String, Self::Error>;
    /// `git merge-base --is-ancestor <ancestor> <descendant>`.
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, Self::Error>;
}

/// Answer `kind` in-process when possible, else through `program`. Every
/// fallback is counted under its reason and then answered by Git.
pub(crate) fn answer<T>(
    root: &Path,
    kind: ReadKind,
    in_process: impl FnOnce(&InProcess) -> Result<T, Fallback>,
    program: impl FnOnce() -> Result<T, GitError>,
) -> Result<T, GitError> {
    match attempt(root, kind, in_process) {
        Ok(value) => Ok(value),
        Err(_) => program(),
    }
}

fn attempt<T>(
    root: &Path,
    kind: ReadKind,
    query: impl FnOnce(&InProcess) -> Result<T, Fallback>,
) -> Result<T, Fallback> {
    let started = Instant::now();
    let outcome = {
        let _span = crate::phase_telemetry::span("git_read_inprocess");
        preconditions().and_then(|()| in_process(root, kind, query))
    };
    let key = match &outcome {
        Ok(_) => format!("{}.answered", kind.as_str()),
        Err(reason) => format!("{}.fallback.{}", kind.as_str(), reason.as_str()),
    };
    crate::phase_telemetry::add_keyed("git_read", &key, started.elapsed());
    outcome
}

/// Open the repository and run `query` against it, a panic included in the
/// fallbacks.
fn in_process<T>(
    root: &Path,
    kind: ReadKind,
    query: impl FnOnce(&InProcess) -> Result<T, Fallback>,
) -> Result<T, Fallback> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        InProcess::open(root, kind)?.guarded(query)
    }))
    .unwrap_or(Err(Fallback::Panic))
}

/// Process-wide reasons to leave every query to Git.
fn preconditions() -> Result<(), Fallback> {
    if disabled_by_environment(std::env::var(IN_PROCESS_ENV).ok().as_deref()) {
        return Err(Fallback::Disabled);
    }
    if crate::git::in_process_git_environment_refused() {
        return Err(Fallback::Environment);
    }
    if crate::git::active_git_deadline().is_some_and(|deadline| deadline <= Instant::now()) {
        return Err(Fallback::Deadline);
    }
    Ok(())
}

fn disabled_by_environment(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// A repository opened by gitoxide for one query, after every
/// repository-wide reason to fall back has been ruled out.
pub(crate) struct InProcess {
    repo: gix::Repository,
}

impl InProcess {
    /// A fresh open per query: gitoxide caches `packed-refs` by modification
    /// time only, which a same-second rewrite would defeat.
    fn open(root: &Path, kind: ReadKind) -> Result<Self, Fallback> {
        let options = gix::open::Options::default().bail_if_untrusted(true);
        let repo = gix::open_opts(root, options).map_err(|_| Fallback::Open)?;
        if repo.git_dir_trust() != gix::sec::Trust::Full {
            return Err(Fallback::Untrusted);
        }
        if repo.object_hash() != gix::hash::Kind::Sha1 {
            return Err(Fallback::Format);
        }
        let other_ref_storage = repo
            .config_snapshot()
            .string("extensions.refStorage")
            .is_some_and(|storage| !storage.eq_ignore_ascii_case(b"files"));
        if other_ref_storage {
            return Err(Fallback::Format);
        }
        if configured_off(repo.common_dir()) {
            return Err(Fallback::Disabled);
        }
        let reader = Self { repo };
        reader.require_unmodified_history(kind)?;
        Ok(reader)
    }

    /// Grafts and replace refs change what Git reports for every query here;
    /// shallow boundaries change ancestry. Their presence leaves the query to
    /// Git, whose current semantics each call site was written against.
    fn require_unmodified_history(&self, kind: ReadKind) -> Result<(), Fallback> {
        match std::fs::symlink_metadata(self.repo.common_dir().join("info/grafts")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(Fallback::HistoryOverride),
        }
        let references = self.repo.references().map_err(|_| Fallback::Gix)?;
        let mut replaced = references
            .prefixed("refs/replace/")
            .map_err(|_| Fallback::Gix)?;
        if replaced.next().is_some() {
            return Err(Fallback::HistoryOverride);
        }
        if kind == ReadKind::IsAncestor && self.repo.is_shallow().map_err(|_| Fallback::Gix)? {
            return Err(Fallback::HistoryOverride);
        }
        Ok(())
    }

    /// Run `query` with `packed-refs` unchanged around it.
    fn guarded<T>(&self, query: impl FnOnce(&Self) -> Result<T, Fallback>) -> Result<T, Fallback> {
        let packed = self.repo.common_dir().join("packed-refs");
        let before = FileStamp::of(&packed)?;
        let value = query(self)?;
        if FileStamp::of(&packed)? != before {
            return Err(Fallback::ConcurrentRefUpdate);
        }
        Ok(value)
    }

    /// The commit `spec` names, by Git's lookup rules, peeled through tags.
    fn commit_id(&self, spec: &str) -> Result<gix::ObjectId, Fallback> {
        let id = match classify(spec)? {
            Spec::Id(id) => id,
            Spec::Name(name) => self.resolve_name(name)?,
        };
        self.peel_to_commit(id)
    }

    fn resolve_name(&self, name: &str) -> Result<gix::ObjectId, Fallback> {
        if name != "HEAD" && !name.starts_with("refs/") {
            self.require_no_root_file(name)?;
        }
        for (index, candidate) in candidates(name).iter().enumerate() {
            // A one-level `<name>` that is not `HEAD` is read by Git as a file
            // in the Git directory, which `require_no_root_file` ruled out.
            if index == 0 && name != "HEAD" && !name.starts_with("refs/") {
                continue;
            }
            if let Some(reference) = self.exact_reference(candidate)? {
                return self.follow(reference);
            }
        }
        Err(Fallback::NotFound)
    }

    /// Git reads a non-`refs/` candidate from the Git directory or the common
    /// directory; gitoxide does not. Any file there leaves the name to Git.
    fn require_no_root_file(&self, name: &str) -> Result<(), Fallback> {
        for dir in [self.repo.git_dir(), self.repo.common_dir()] {
            match std::fs::symlink_metadata(dir.join(name)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(Fallback::RootFileShadow),
            }
        }
        Ok(())
    }

    /// The ref named exactly `name`. gitoxide's lookup also tries the other
    /// rule prefixes, so a match under a different name is not this one.
    fn exact_reference(&self, name: &str) -> Result<Option<gix::Reference<'_>>, Fallback> {
        let found = self
            .repo
            .try_find_reference(name)
            .map_err(|_| Fallback::Gix)?;
        Ok(found.filter(|reference| {
            let found: &[u8] = reference.name().as_bstr().as_ref();
            found == name.as_bytes()
        }))
    }

    /// Follow symbolic refs to an object id, as Git does with `SYMREF_MAXDEPTH`.
    fn follow(&self, mut reference: gix::Reference<'_>) -> Result<gix::ObjectId, Fallback> {
        for _ in 0..=MAX_SYMREF_DEPTH {
            let target = match reference.target() {
                gix::refs::TargetRef::Object(id) => return Ok(id.to_owned()),
                gix::refs::TargetRef::Symbolic(name) => {
                    let name: &[u8] = name.as_bstr().as_ref();
                    std::str::from_utf8(name)
                        .map_err(|_| Fallback::Unresolved)?
                        .to_string()
                }
            };
            reference = self.exact_reference(&target)?.ok_or(Fallback::Unresolved)?;
        }
        Err(Fallback::Unresolved)
    }

    fn object(&self, id: gix::ObjectId) -> Result<gix::Object<'_>, Fallback> {
        self.repo
            .try_find_object(id)
            .map_err(|_| Fallback::Gix)?
            .ok_or(Fallback::ObjectMissing)
    }

    fn peel_to_commit(&self, mut id: gix::ObjectId) -> Result<gix::ObjectId, Fallback> {
        for _ in 0..MAX_TAG_DEPTH {
            let object = self.object(id)?;
            match object.kind {
                gix::objs::Kind::Commit => return Ok(object.id),
                gix::objs::Kind::Tag => {
                    id = gix::objs::TagRefIter::from_bytes(&object.data, self.repo.object_hash())
                        .target_id()
                        .map_err(|_| Fallback::Gix)?;
                }
                gix::objs::Kind::Tree | gix::objs::Kind::Blob => {
                    return Err(Fallback::NotACommit);
                }
            }
        }
        Err(Fallback::Unresolved)
    }
}

impl GitRead for InProcess {
    type Error = Fallback;

    fn resolve_commit(&self, spec: &str) -> Result<String, Fallback> {
        Ok(self.commit_id(spec)?.to_string())
    }

    fn head_commit(&self) -> Result<String, Fallback> {
        // `rev-parse HEAD` prints HEAD's target unpeeled; only a commit
        // target is answered here, so peeling is the identity.
        let id = self.resolve_name("HEAD")?;
        let object = self.object(id)?;
        if object.kind != gix::objs::Kind::Commit {
            return Err(Fallback::NotACommit);
        }
        Ok(id.to_string())
    }

    fn commit_tree(&self, spec: &str) -> Result<String, Fallback> {
        let commit = self.object(self.commit_id(spec)?)?;
        let tree = gix::objs::CommitRefIter::from_bytes(&commit.data, self.repo.object_hash())
            .tree_id()
            .map_err(|_| Fallback::Gix)?;
        // Git parses the tree it peels to; a missing one is Git's to report.
        if !self.repo.has_object(tree) {
            return Err(Fallback::ObjectMissing);
        }
        Ok(tree.to_string())
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, Fallback> {
        let ancestor = self.commit_id(ancestor)?;
        let descendant = self.commit_id(descendant)?;
        // Missing parents are errors in gitoxide's walk, so `false` is only
        // returned after a complete one.
        self.repo
            .is_ancestor(ancestor, descendant)
            .map_err(|_| Fallback::Gix)
    }
}

/// A revision this module resolves: a full object id or a plain ref name.
#[derive(Debug, PartialEq, Eq)]
enum Spec<'a> {
    Id(gix::ObjectId),
    Name(&'a str),
}

/// Anything with revision syntax, a possible short object id, a pseudo ref
/// other than `HEAD`, or a per-worktree ref namespace is unsupported.
fn classify(spec: &str) -> Result<Spec<'_>, Fallback> {
    let bytes = spec.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES {
        return Err(Fallback::UnsupportedSpec);
    }
    if bytes.iter().all(u8::is_ascii_hexdigit) {
        if bytes.len() == 40 && bytes.iter().all(|byte| !byte.is_ascii_uppercase()) {
            return gix::ObjectId::from_hex(bytes)
                .map(Spec::Id)
                .map_err(|_| Fallback::UnsupportedSpec);
        }
        return Err(Fallback::UnsupportedSpec);
    }
    let plain = bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'.' | b'-'));
    let well_formed = plain
        && !spec.starts_with('-')
        && !spec.ends_with('.')
        && spec
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
        && !spec.contains("..");
    if !well_formed {
        return Err(Fallback::UnsupportedSpec);
    }
    let pseudo = spec
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte == b'_');
    if pseudo && spec != "HEAD" {
        return Err(Fallback::UnsupportedSpec);
    }
    const PER_WORKTREE: [&str; 8] = [
        "refs/worktree/",
        "refs/bisect/",
        "refs/rewritten/",
        "worktree/",
        "bisect/",
        "rewritten/",
        "main-worktree/",
        "worktrees/",
    ];
    if PER_WORKTREE.iter().any(|prefix| spec.starts_with(prefix)) {
        return Err(Fallback::UnsupportedSpec);
    }
    Ok(Spec::Name(spec))
}

/// Git's `ref_rev_parse_rules`, in order.
fn candidates(name: &str) -> [String; 6] {
    [
        name.to_string(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ]
}

/// Identity of a file at one moment; `None` when it does not exist.
#[derive(Debug, PartialEq, Eq)]
struct FileStamp(Option<(u64, Option<SystemTime>, u64, u64)>);

impl FileStamp {
    fn of(path: &Path) -> Result<Self, Fallback> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Self(Some((
                metadata.len(),
                metadata.modified().ok(),
                inode(&metadata),
                change_time(&metadata),
            )))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self(None)),
            Err(_) => Err(Fallback::Gix),
        }
    }
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn change_time(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    (metadata.ctime() as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(metadata.ctime_nsec() as u64)
}

#[cfg(not(unix))]
fn change_time(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// `[git] in_process = false` in the main checkout's `.aethyme/config.toml`
/// (or its YAML twin). Re-read whenever one of the files changes; an
/// unreadable or invalid file leaves every query to Git.
fn configured_off(common_dir: &Path) -> bool {
    static SWITCHES: Mutex<Option<HashMap<PathBuf, (Vec<FileStamp>, bool)>>> = Mutex::new(None);
    // A linked worktree names its common directory relatively (`../..`).
    let Ok(common_dir) = std::fs::canonicalize(common_dir) else {
        return true;
    };
    let Some(main) = common_dir
        .file_name()
        .filter(|name| *name == std::ffi::OsStr::new(".git"))
        .and_then(|_| common_dir.parent())
    else {
        return false;
    };
    let dir = main.join(aethyme_config::CONFIG_DIR);
    let Ok(stamps) = aethyme_config::candidate_file_names("config")
        .iter()
        .map(|name| FileStamp::of(&dir.join(name)))
        .collect::<Result<Vec<_>, _>>()
    else {
        return true;
    };
    let Ok(mut switches) = SWITCHES.lock() else {
        return true;
    };
    let switches = switches.get_or_insert_with(HashMap::new);
    if let Some((seen, off)) = switches.get(&dir)
        && *seen == stamps
    {
        return *off;
    }
    let off = match aethyme_config::load_config(&dir, "config") {
        Ok(None) => false,
        Ok(Some(document)) => in_process_off(&document.value),
        Err(_) => true,
    };
    switches.insert(dir, (stamps, off));
    off
}

fn in_process_off(config: &toml::Value) -> bool {
    match config.get("git").map(|git| git.get("in_process")) {
        None | Some(None) => false,
        Some(Some(toml::Value::Boolean(on))) => !on,
        Some(Some(_)) => true,
    }
}

/// Run `query` in-process only, without the process-wide preconditions,
/// for the parity tests.
#[cfg(test)]
fn in_process_only<T>(
    root: &Path,
    kind: ReadKind,
    query: impl FnOnce(&InProcess) -> Result<T, Fallback>,
) -> Result<T, Fallback> {
    in_process(root, kind, query)
}

#[cfg(test)]
#[path = "git_read_tests.rs"]
mod tests;
