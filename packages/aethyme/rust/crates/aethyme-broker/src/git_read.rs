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
//!   The ancestry walk records every object it cannot find (gitoxide alone
//!   would skip it) and falls back, and never runs in a promisor repository.
//! - Grafts, replace refs, shallow history (for ancestry), SHA-256, non-file
//!   ref storage, unknown repository extensions, reduced trust, inherited
//!   `GIT_*` routing or history variables, an expired Git deadline, and (for
//!   the uninterruptible ancestry walk) any active Git deadline all fall back,
//!   preserving each call site's current semantics.
//! - Symbolic refs are followed for at most four hops (Git reads five refs),
//!   loose refs must have a form Git accepts, and a tag whose `type` header
//!   disagrees with its target falls back.
//!
//! `AETHYME_GIT_IN_PROCESS` set to anything but `1`, `true`, `on` or `yes`,
//! and `[git] in_process = false` in the main checkout's
//! `.aethyme/config.toml`, force the program everywhere. A repository whose
//! common directory is not `<main checkout>/.git` (bare, `--separate-git-dir`)
//! always uses the program, since the switch cannot be located.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use crate::git::GitError;

/// Environment kill switch: unset, `1`, `true`, `on` or `yes` allows
/// in-process reads; any other value forces the program.
pub(crate) const IN_PROCESS_ENV: &str = "AETHYME_GIT_IN_PROCESS";

/// Git reads at most five refs per resolution (`SYMREF_MAXDEPTH`), the named
/// one included, so a chain of more than four symbolic hops is dangling to
/// Git.
const MAX_SYMREF_HOPS: usize = 4;
/// Extensions whose presence does not change what these queries answer;
/// `objectFormat` and `refStorage` are checked for their values separately.
const KNOWN_EXTENSIONS: [&str; 6] = [
    "noop",
    "preciousobjects",
    "partialclone",
    "worktreeconfig",
    "objectformat",
    "refstorage",
];
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
    /// A loose ref file Git would treat as broken.
    BrokenRef,
    /// A tag whose `type` header disagrees with the object it names.
    TagTypeMismatch,
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
            Self::BrokenRef => "broken_ref",
            Self::TagTypeMismatch => "tag_type_mismatch",
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
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

/// An ancestry walk cannot be interrupted, so it runs in-process only when
/// no caller bounds Git: under an active deadline or a shortened
/// `AETHYME_GIT_TIMEOUT_SECS`, Git keeps enforcing the bound callers expect.
fn ancestry_walk_unbounded_ok() -> bool {
    crate::git::active_git_deadline().is_none()
        && crate::git::git_timeout() >= crate::git::default_git_timeout()
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
        if other_ref_storage || !known_extensions_only(repo.common_dir()) {
            return Err(Fallback::Format);
        }
        if kind == ReadKind::IsAncestor && !ancestry_walk_unbounded_ok() {
            return Err(Fallback::Deadline);
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
        // gitoxide may already hold a snapshot from opening, and reloads only
        // on a strictly newer mtime; reread it so every lookup sees a
        // snapshot no older than `before`. An absent file drops the snapshot.
        if before.0.is_some() {
            self.repo
                .refs
                .force_refresh_packed_buffer()
                .map_err(|_| Fallback::Gix)?;
        }
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
        let Some(reference) = found.filter(|reference| {
            let found: &[u8] = reference.name().as_bstr().as_ref();
            found == name.as_bytes()
        }) else {
            return Ok(None);
        };
        self.require_well_formed_loose(name)?;
        Ok(Some(reference))
    }

    /// gitoxide accepts an object id followed by garbage in a loose ref; Git
    /// treats anything but whitespace after the id as a broken ref and skips
    /// the candidate. Only the two forms Git accepts are answered here.
    fn require_well_formed_loose(&self, name: &str) -> Result<(), Fallback> {
        let dir = if name.starts_with("refs/") {
            self.repo.common_dir()
        } else {
            self.repo.git_dir()
        };
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(metadata) if metadata.is_dir() => return Ok(()),
            Ok(metadata) if metadata.is_file() && metadata.len() <= 4096 => {}
            _ => return Err(Fallback::BrokenRef),
        }
        let contents = match std::fs::read(&path) {
            Ok(contents) => contents,
            // Packed meanwhile: the packed-refs guard decides.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Fallback::BrokenRef),
        };
        if well_formed_loose_ref(&contents) {
            Ok(())
        } else {
            Err(Fallback::BrokenRef)
        }
    }

    /// Follow symbolic refs to an object id: the named ref plus at most
    /// [`MAX_SYMREF_HOPS`] more reads, Git's five in total.
    fn follow<'repo>(
        &'repo self,
        mut reference: gix::Reference<'repo>,
    ) -> Result<gix::ObjectId, Fallback> {
        for hop in 0..=MAX_SYMREF_HOPS {
            let target = match reference.target() {
                gix::refs::TargetRef::Object(id) => return Ok(id.to_owned()),
                gix::refs::TargetRef::Symbolic(_) if hop == MAX_SYMREF_HOPS => break,
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
        let mut expected = None;
        for _ in 0..MAX_TAG_DEPTH {
            let object = self.object(id)?;
            // Git refuses a tag whose `type` header names another kind.
            if expected.is_some_and(|kind| kind != object.kind) {
                return Err(Fallback::TagTypeMismatch);
            }
            match object.kind {
                gix::objs::Kind::Commit => return Ok(object.id),
                gix::objs::Kind::Tag => {
                    let tag = gix::objs::TagRef::from_bytes(&object.data, self.repo.object_hash())
                        .map_err(|_| Fallback::Gix)?;
                    expected = Some(tag.target_kind);
                    id = tag.target();
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
        if ancestor == descendant {
            return Ok(true);
        }
        // A promisor repository may lack commits Git would fetch.
        if self
            .repo
            .config_snapshot()
            .string("extensions.partialClone")
            .is_some()
        {
            return Err(Fallback::ObjectMissing);
        }
        // gitoxide's walk skips a parent it cannot find, which would truncate
        // it into a wrong "no"; the finder records every miss instead, and
        // any miss leaves the answer to Git.
        let missing = std::cell::Cell::new(false);
        let finder = MissIsRecorded {
            objects: &self.repo.objects,
            missing: &missing,
        };
        let cache = self
            .repo
            .commit_graph_if_enabled()
            .map_err(|_| Fallback::Gix)?;
        let mut graph: gix::revwalk::Graph<
            '_,
            '_,
            gix::revwalk::graph::Commit<gix::revision::plumbing::merge_base::Flags>,
        > = gix::revwalk::Graph::new(finder, cache.as_ref());
        // An ancestor of `descendant` is their only best merge base.
        let bases = gix::revision::plumbing::merge_base(ancestor, &[descendant], &mut graph)
            .map_err(|_| Fallback::Gix)?;
        if missing.get() {
            return Err(Fallback::ObjectMissing);
        }
        Ok(bases.is_some_and(|bases| bases.iter().any(|base| *base == ancestor)))
    }
}

/// An object finder for the ancestry walk that records a missing object, or
/// one that is not a commit, instead of letting the walk skip it silently.
struct MissIsRecorded<'a> {
    objects: &'a gix::OdbHandle,
    missing: &'a std::cell::Cell<bool>,
}

impl gix::objs::Find for MissIsRecorded<'_> {
    fn try_find<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix::error::Result<Option<gix::objs::Data<'a>>> {
        let found = gix::objs::Find::try_find(self.objects, id, buffer)?;
        if found
            .as_ref()
            .is_none_or(|data| data.kind != gix::objs::Kind::Commit)
        {
            self.missing.set(true);
        }
        Ok(found)
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

/// The config files' stamps when last read, and whether they switch off.
type ConfigSwitch = (Vec<FileStamp>, bool);

/// `[git] in_process = false` in the main checkout's `.aethyme/config.toml`
/// (or its YAML twin). Re-read whenever one of the files changes; an
/// unreadable or invalid file leaves every query to Git.
fn configured_off(common_dir: &Path) -> bool {
    static SWITCHES: Mutex<Option<HashMap<PathBuf, ConfigSwitch>>> = Mutex::new(None);
    // A linked worktree names its common directory relatively (`../..`).
    let Ok(common_dir) = std::fs::canonicalize(common_dir) else {
        return true;
    };
    // Without a `<main checkout>/.git` common directory (a bare repository,
    // `--separate-git-dir`) the switch cannot be located, so Git answers.
    let Some(main) = common_dir
        .file_name()
        .filter(|name| *name == std::ffi::OsStr::new(".git"))
        .and_then(|_| common_dir.parent())
    else {
        return true;
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

/// A loose ref Git reads: an object id followed by nothing or whitespace, or
/// `ref:` with a target.
fn well_formed_loose_ref(contents: &[u8]) -> bool {
    // gitoxide reads only `ref: <target>`; Git's other spellings fall back.
    if let Some(target) = contents.strip_prefix(b"ref: ") {
        return !target.trim_ascii().is_empty();
    }
    contents.len() >= 40
        && contents[..40].iter().all(u8::is_ascii_hexdigit)
        && contents[40..].first().is_none_or(u8::is_ascii_whitespace)
}

/// Whether the repository configuration names only extensions in
/// [`KNOWN_EXTENSIONS`]. Git reads extensions from this one file, without
/// includes; anything this parser does not follow leaves the query to Git.
fn known_extensions_only(common_dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(common_dir.join("config")) else {
        return false;
    };
    let mut in_extensions = false;
    for line in text.lines() {
        let line = line.trim();
        if line.ends_with('\\') {
            return false;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some((header, after)) = rest.split_once(']') else {
                return false;
            };
            let after = after.trim();
            if !after.is_empty() && !after.starts_with(['#', ';']) {
                return false;
            }
            let header = header.trim();
            in_extensions = header.eq_ignore_ascii_case("extensions");
            if !in_extensions && header.to_ascii_lowercase().starts_with("extensions") {
                return false;
            }
            continue;
        }
        if !in_extensions || line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        let key = line.split(['=', ' ', '\t']).next().unwrap_or_default();
        if !KNOWN_EXTENSIONS.contains(&key.to_ascii_lowercase().as_str()) {
            return false;
        }
    }
    true
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
