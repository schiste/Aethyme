//! A content-addressed archive of exact source, so a captured contribution
//! can be rebuilt after its worktree, branch and Git objects are gone (#657,
//! plan §6.3-6.4, §10.1; D08, D20; T07-T10).
//!
//! ```text
//! <project dir>/
//!   objects/sha256/<2 hex>/<62 hex>   immutable objects, named by SHA-256
//!   spool/archive/                    temporaries; an orphan here is harmless
//! ```
//!
//! - **Objects.** Every object is named by the SHA-256 of its bytes. A blob
//!   is the raw committed content. A snapshot's manifest is the #652
//!   manifest itself, so its object name *is* the `SourceSnapshotId` digest
//!   and the snapshot needs no second index to be found or checked. Records
//!   (#653 layer) are stored as their canonical bytes.
//! - **Publication.** Write a temporary in the spool, flush it, re-read and
//!   hash what reached the file, publish it without replacing anything, and
//!   flush the directory. An existing object with the same name must hash
//!   the same, or the archive is refused as corrupt, and its directory is
//!   flushed too, so a caller never records an object whose entry a
//!   concurrent writer has not yet made durable. Never a hard link into a
//!   checkout or a Git alternate: the archive holds its own copy.
//! - **Capture reads Git, not the worktree.** The commit is named by a full
//!   object id, so a branch that moves cannot change what is retained. The
//!   commit, every tree and every blob are read through `git cat-file
//!   --batch` and checked against their object ids (Git does not check loose
//!   objects on read, and `ls-tree` would list a damaged tree). Replace
//!   refs, grafts and lazy fetching are off; inherited repository and
//!   configuration variables are cleared; a configured fsmonitor or hook
//!   never runs. A partial clone is reported incomplete up front.
//! - **Never a guessed success.** A missing object, a digest mismatch or
//!   missing history is *incomplete* ([`ArchiveError::is_incomplete`]). A
//!   submodule, a mode #652 refuses, or an attribute that makes a checkout
//!   differ from the committed bytes (`filter`, `working-tree-encoding`,
//!   `ident`; from the tree, `.git/info/attributes` or
//!   `core.attributesFile`) is *refused*. Either way
//!   no index row is written, and objects already copied stay as orphans
//!   that a retry reuses.
//! - **Lineage.** A contribution retains its complete base and result
//!   snapshots and checks that the base commit is an ancestor of the result.
//!   Replay needs only those two snapshots; the commits in between are
//!   recorded as provenance, not retained.
//! - **Rebuild.** [`reconstruct`] writes only a snapshot the archive
//!   retained (an index row and a manifest in canonical form, so a committed
//!   file that happens to parse as a manifest is not one), from the archive
//!   alone, verifying every byte. It refuses a symlinked destination and
//!   any path the destination filesystem folds (case or Unicode), as #652
//!   decided.
//!
//! Retention roots, receipts and the capture state machine are #658's;
//! reclamation is #659's. Nothing here deletes an object.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};
use aethyme_contracts::experimental_v0::{
    EntryKind, FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceEntry, SourceSnapshot,
    SourceSnapshotError, SourceSnapshotId,
};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

use crate::collaboration_state::{CollaborationStateError, CollaborationStore, sync_directory};

pub const RETAINED_SNAPSHOT_SCHEMA_NAME: &str = "aethyme.retained-snapshot/experimental-v0";
pub const CONTRIBUTION_LINEAGE_SCHEMA_NAME: &str = "aethyme.contribution-lineage/experimental-v0";

const fn field(name: &'static str, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        name,
        required: true,
        kind,
        capability: None,
    }
}

/// What the archive holds for one snapshot, and where it came from. The
/// commit is a locator, not identity: the snapshot ID is.
pub static RETAINED_SNAPSHOT_SCHEMA: RecordSchema = RecordSchema {
    name: RETAINED_SNAPSHOT_SCHEMA_NAME,
    fields: &[
        field("snapshot", FieldKind::String),
        field("entry_count", FieldKind::Integer),
        field("content_bytes", FieldKind::DecimalString),
        field("source", FieldKind::Opaque),
    ],
    capabilities: &[],
};

/// A contribution: the base it starts from and the result it proposes.
pub static CONTRIBUTION_LINEAGE_SCHEMA: RecordSchema = RecordSchema {
    name: CONTRIBUTION_LINEAGE_SCHEMA_NAME,
    fields: &[
        field("base", FieldKind::String),
        field("result", FieldKind::String),
        field("base_commit", FieldKind::String),
        field("result_commit", FieldKind::String),
        field("object_format", FieldKind::String),
        field("first_parent_commits", FieldKind::Integer),
    ],
    capabilities: &[],
};

/// A full Git object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
/// Never a ref name: what a name points to can change.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitOid(String);

impl CommitOid {
    pub fn parse(text: &str) -> Result<Self, ArchiveError> {
        if is_lower_hex(text, &[40, 64]) {
            Ok(Self(text.to_string()))
        } else {
            Err(ArchiveError::NotAnObjectId {
                text: text.to_string(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The SHA-256 naming one archive object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectDigest([u8; 32]);

impl ObjectDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest a `SourceSnapshotId` names: its manifest object.
    pub fn of_snapshot(id: &SourceSnapshotId) -> Self {
        let hex = &id.as_str()["sha256:".len()..];
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("canonical id");
        }
        Self(bytes)
    }

    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// A snapshot whose manifest, blobs and record are all in the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedSnapshot {
    pub snapshot_id: SourceSnapshotId,
    pub record_id: RecordId,
    pub commit: CommitOid,
    pub entry_count: u64,
    pub content_bytes: u64,
}

/// A contribution whose base and result are both retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedContribution {
    pub base: RetainedSnapshot,
    pub result: RetainedSnapshot,
    pub lineage_record_id: RecordId,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("{text:?} is not a full Git object id; pin the commit first (pin_commit)")]
    NotAnObjectId { text: String },
    #[error("{oid} is not a commit")]
    NotACommit { oid: String },
    #[error(
        "the repository is a partial clone ({setting}); objects it omits would have to be \
         fetched, which capture never does. Capture from a full clone"
    )]
    PartialClone { setting: String },
    #[error("source object {oid} matches its id but is not a well-formed {kind}")]
    MalformedSource { oid: String, kind: &'static str },
    #[error("{revision:?} does not name a commit in this repository")]
    UnknownRevision { revision: String },
    #[error(
        "{} has mode {mode}, which a v0 snapshot cannot retain (submodules and other modes are \
         refused rather than skipped)",
        String::from_utf8_lossy(path)
    )]
    UnsupportedEntry { path: Vec<u8>, mode: String },
    #[error(
        "{} sets {attribute}, so a Git checkout would not produce the committed bytes; the \
         archive cannot retain what replay needs",
        String::from_utf8_lossy(path)
    )]
    UnsupportedFilter { path: Vec<u8>, attribute: String },
    #[error("the commit's tree is not a valid v0 snapshot: {0}")]
    InvalidSnapshot(#[from] SourceSnapshotError),
    #[error("source object {oid} is unavailable: {detail}")]
    SourceUnavailable { oid: String, detail: String },
    #[error("source object {oid} did not match its object id while it was copied")]
    SourceMismatch { oid: String },
    #[error("history between {base} and {result} is unavailable: {detail}")]
    HistoryUnavailable {
        base: String,
        result: String,
        detail: String,
    },
    #[error("base {base} is not an ancestor of result {result}")]
    BaseNotAncestor { base: String, result: String },
    #[error("archive object {digest} does not hash to its name; the archive is corrupt")]
    CorruptObject { digest: String },
    #[error("archive object {digest} is missing")]
    MissingObject { digest: String },
    #[error("snapshot {id} is not retained in this archive")]
    NotRetained { id: String },
    #[error("{} must be absent or an empty directory", path.display())]
    DestinationNotEmpty { path: PathBuf },
    #[error("{} is a symbolic link; name the real directory", path.display())]
    DestinationIsSymlink { path: PathBuf },
    #[error(
        "{} collides with another path on this filesystem (letter case or Unicode \
         composition); materialize on a case-sensitive, non-normalizing filesystem",
        String::from_utf8_lossy(path)
    )]
    MaterializationCollision { path: Vec<u8> },
    #[error("git: {detail}")]
    Git { detail: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("collaboration state: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    State(#[from] CollaborationStateError),
}

impl ArchiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAnObjectId { .. } => "not_an_object_id",
            Self::NotACommit { .. } => "not_a_commit",
            Self::PartialClone { .. } => "partial_clone",
            Self::MalformedSource { .. } => "malformed_source",
            Self::UnknownRevision { .. } => "unknown_revision",
            Self::UnsupportedEntry { .. } => "unsupported_entry",
            Self::UnsupportedFilter { .. } => "unsupported_filter",
            Self::InvalidSnapshot(_) => "invalid_snapshot",
            Self::SourceUnavailable { .. } => "source_unavailable",
            Self::SourceMismatch { .. } => "source_mismatch",
            Self::HistoryUnavailable { .. } => "history_unavailable",
            Self::BaseNotAncestor { .. } => "base_not_ancestor",
            Self::CorruptObject { .. } => "corrupt_object",
            Self::MissingObject { .. } => "missing_object",
            Self::NotRetained { .. } => "not_retained",
            Self::DestinationNotEmpty { .. } => "destination_not_empty",
            Self::DestinationIsSymlink { .. } => "destination_is_symlink",
            Self::MaterializationCollision { .. } => "materialization_collision",
            Self::Git { .. } => "git",
            Self::Io { .. } => "io",
            Self::Sqlite(_) => "sqlite",
            Self::State(_) => "state",
        }
    }

    /// The source could not be read completely: retrying later, or from a
    /// fuller clone, may succeed. Distinct from a refusal, which no retry
    /// changes.
    pub fn is_incomplete(&self) -> bool {
        matches!(
            self,
            Self::SourceUnavailable { .. }
                | Self::SourceMismatch { .. }
                | Self::HistoryUnavailable { .. }
                | Self::PartialClone { .. }
        )
    }
}

fn io(path: &Path, source: std::io::Error) -> ArchiveError {
    ArchiveError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_lower_hex(text: &str, lengths: &[usize]) -> bool {
    lengths.contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---------------------------------------------------------------- objects

fn objects_dir(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join("objects/sha256")
}

fn spool_dir(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join("spool/archive")
}

fn object_path(store: &CollaborationStore, digest: &ObjectDigest) -> PathBuf {
    let hex = digest.hex();
    objects_dir(store).join(&hex[..2]).join(&hex[2..])
}

/// Create `path` and any missing parents privately, and make its entry in
/// its parent durable.
///
/// The parent is flushed even when the directory already exists: another
/// writer may have created it and not yet flushed the entry, and this
/// writer's index row must not become durable before the directory that
/// holds its objects does.
fn ensure_dir(path: &Path) -> Result<(), ArchiveError> {
    if path.is_dir() {
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            return Ok(());
        }
        Err(source) => return Err(io(path, source)),
    }
    crate::host_state::protect_host_state_path(path, true).map_err(|source| io(path, source))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

/// SHA-256 of a file's current bytes.
fn hash_file(path: &Path) -> Result<ObjectDigest, ArchiveError> {
    let mut file = std::fs::File::open(path).map_err(|source| io(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| io(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ObjectDigest(hasher.finalize().into()))
}

/// Publish a flushed temporary as the object `digest`, after re-reading it.
fn publish(
    store: &CollaborationStore,
    temporary: tempfile::NamedTempFile,
    digest: ObjectDigest,
) -> Result<ObjectDigest, ArchiveError> {
    // What reached the file, not what was handed to write().
    if hash_file(temporary.path())? != digest {
        return Err(ArchiveError::CorruptObject {
            digest: digest.hex(),
        });
    }
    let target = object_path(store, &digest);
    let fan_out = target.parent().expect("object paths have a parent");
    ensure_dir(fan_out)?;
    match temporary.persist_noclobber(&target) {
        Ok(_) => sync_directory(fan_out)?,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if hash_file(&target)? != digest {
                return Err(ArchiveError::CorruptObject {
                    digest: digest.hex(),
                });
            }
            // A concurrent writer may not have flushed its entry yet; a
            // caller that records this object must not outrun it.
            sync_directory(fan_out)?;
        }
        Err(error) => return Err(io(&target, error.error)),
    }
    Ok(digest)
}

fn new_temporary(store: &CollaborationStore) -> Result<tempfile::NamedTempFile, ArchiveError> {
    let spool = spool_dir(store);
    ensure_dir(&spool)?;
    tempfile::Builder::new()
        .prefix(".object-")
        .tempfile_in(&spool)
        .map_err(|source| io(&spool, source))
}

/// Store `bytes` and return their name.
pub fn put_object(store: &CollaborationStore, bytes: &[u8]) -> Result<ObjectDigest, ArchiveError> {
    let mut temporary = new_temporary(store)?;
    let path = temporary.path().to_path_buf();
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| io(&path, source))?;
    publish(store, temporary, ObjectDigest::of(bytes))
}

/// Read an object, refusing one that does not hash to its name.
pub fn read_object(
    store: &CollaborationStore,
    digest: &ObjectDigest,
) -> Result<Vec<u8>, ArchiveError> {
    let path = object_path(store, digest);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ArchiveError::MissingObject {
                digest: digest.hex(),
            });
        }
        Err(source) => return Err(io(&path, source)),
    };
    if ObjectDigest::of(&bytes) != *digest {
        return Err(ArchiveError::CorruptObject {
            digest: digest.hex(),
        });
    }
    Ok(bytes)
}

pub fn has_object(store: &CollaborationStore, digest: &ObjectDigest) -> bool {
    object_path(store, digest).is_file()
}

// -------------------------------------------------------------------- git

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--no-replace-objects")
        // Loading the index starts a configured fsmonitor, and `check-attr`
        // loads it: a repository's configuration must not run a program
        // during capture. Hooks are pointed nowhere for the same reason.
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .arg("-C")
        .arg(repo)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        // A partial clone must not fetch what it lacks: a missing object is
        // reported as missing.
        .env("GIT_NO_LAZY_FETCH", "1")
        // Grafts rewrite parents and would change the ancestry answer.
        .env("GIT_GRAFT_FILE", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
    // Configuration injected through the environment by the caller.
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(name.as_ref());
        }
    }
    command
}

fn git_output(repo: &Path, args: &[&str]) -> Result<std::process::Output, ArchiveError> {
    git(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| ArchiveError::Git {
            detail: format!("could not run git {args:?}: {error}"),
        })
}

fn git_text(repo: &Path, args: &[&str]) -> Result<String, ArchiveError> {
    let output = git_output(repo, args)?;
    if !output.status.success() {
        return Err(ArchiveError::Git {
            detail: format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve `revision` to the exact commit it names now. Capture takes the
/// result, so a branch that moves afterwards does not change what is retained.
pub fn pin_commit(repo: &Path, revision: &str) -> Result<CommitOid, ArchiveError> {
    let output = git_output(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?;
    if !output.status.success() {
        return Err(ArchiveError::UnknownRevision {
            revision: revision.to_string(),
        });
    }
    CommitOid::parse(String::from_utf8_lossy(&output.stdout).trim())
}

/// Refuse a partial clone: objects it omitted would otherwise surface as
/// missing part-way through, or be fetched from a promisor remote.
fn refuse_partial_clone(repo: &Path) -> Result<(), ArchiveError> {
    let output = git_output(
        repo,
        &[
            "config",
            "--get-regexp",
            r"^(extensions\.partialclone|remote\..*\.promisor|remote\..*\.partialclonefilter)$",
        ],
    )?;
    match output.status.code() {
        Some(0) => Err(ArchiveError::PartialClone {
            setting: String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
        }),
        // 1: no such key.
        Some(1) => Ok(()),
        _ => Err(ArchiveError::Git {
            detail: format!(
                "git config: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }),
    }
}

/// `git cat-file --batch`, one object at a time.
struct ObjectReader {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    format: ObjectFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
}

/// Hash content as a Git object of `kind`, in `format`.
enum GitHasher {
    Sha1(sha1::Sha1),
    Sha256(Sha256),
}

impl GitHasher {
    fn new(format: ObjectFormat, kind: &str, size: u64) -> Self {
        let header = format!("{kind} {size}\0");
        match format {
            ObjectFormat::Sha1 => {
                let mut hasher = <sha1::Sha1 as Digest>::new();
                hasher.update(header.as_bytes());
                Self::Sha1(hasher)
            }
            ObjectFormat::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(header.as_bytes());
                Self::Sha256(hasher)
            }
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hasher) => Digest::update(hasher, bytes),
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    fn hex(self) -> String {
        match self {
            Self::Sha1(hasher) => hex(&hasher.finalize()),
            Self::Sha256(hasher) => hex(&hasher.finalize()),
        }
    }
}

impl ObjectReader {
    fn open(repo: &Path) -> Result<Self, ArchiveError> {
        let format = match git_text(repo, &["rev-parse", "--show-object-format"])?.as_str() {
            "sha1" => ObjectFormat::Sha1,
            "sha256" => ObjectFormat::Sha256,
            other => {
                return Err(ArchiveError::Git {
                    detail: format!("unsupported object format {other:?}"),
                });
            }
        };
        let mut child = git(repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| ArchiveError::Git {
                detail: format!("could not start git cat-file: {error}"),
            })?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(Self {
            child,
            stdin,
            stdout,
            format,
        })
    }

    fn pipe_error(&self, error: std::io::Error) -> ArchiveError {
        ArchiveError::Git {
            detail: format!("git cat-file: {error}"),
        }
    }

    /// Ask for `oid`; return its type and size, or `None` when it is missing.
    fn request(&mut self, oid: &str) -> Result<Option<(String, u64)>, ArchiveError> {
        writeln!(self.stdin, "{oid}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| self.pipe_error(e))?;
        let mut header = String::new();
        self.stdout
            .read_line(&mut header)
            .map_err(|e| self.pipe_error(e))?;
        let fields: Vec<&str> = header.trim_end().split(' ').collect();
        match fields.as_slice() {
            [found, kind, size] if *found == oid => {
                let size = size.parse().map_err(|_| ArchiveError::Git {
                    detail: format!("unreadable cat-file header {header:?}"),
                })?;
                Ok(Some((kind.to_string(), size)))
            }
            [_, "missing"] | [_, "ambiguous"] => Ok(None),
            _ => Err(ArchiveError::Git {
                detail: format!("unexpected cat-file header {header:?}"),
            }),
        }
    }

    /// Stream the body of the object just requested into `sink`, checking it
    /// against `oid`. Returns the content SHA-256.
    fn copy_body(
        &mut self,
        oid: &str,
        kind: &str,
        size: u64,
        sink: &mut dyn Write,
    ) -> Result<[u8; 32], ArchiveError> {
        let mut git_hash = GitHasher::new(self.format, kind, size);
        let mut content_hash = Sha256::new();
        let mut remaining = size;
        let mut buffer = vec![0; 64 * 1024];
        while remaining > 0 {
            let want = buffer
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let read = self
                .stdout
                .read(&mut buffer[..want])
                .map_err(|e| self.pipe_error(e))?;
            if read == 0 {
                return Err(ArchiveError::SourceUnavailable {
                    oid: oid.to_string(),
                    detail: "git stopped before the end of the object".into(),
                });
            }
            git_hash.update(&buffer[..read]);
            content_hash.update(&buffer[..read]);
            sink.write_all(&buffer[..read])
                .map_err(|source| ArchiveError::Io {
                    path: PathBuf::from("<archive temporary>"),
                    source,
                })?;
            remaining -= read as u64;
        }
        let mut newline = [0; 1];
        self.stdout
            .read_exact(&mut newline)
            .map_err(|e| self.pipe_error(e))?;
        if git_hash.hex() != oid {
            return Err(ArchiveError::SourceMismatch {
                oid: oid.to_string(),
            });
        }
        Ok(content_hash.finalize().into())
    }

    /// Read a small object whole.
    fn read_whole(&mut self, oid: &str) -> Result<(String, Vec<u8>), ArchiveError> {
        let Some((kind, size)) = self.request(oid)? else {
            return Err(ArchiveError::SourceUnavailable {
                oid: oid.to_string(),
                detail: "missing from the repository".into(),
            });
        };
        let mut bytes = Vec::new();
        self.copy_body(oid, &kind, size, &mut bytes)?;
        Ok((kind, bytes))
    }
}

impl Drop for ObjectReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One file in a commit's tree.
struct TreeEntry {
    mode: String,
    oid: String,
    path: Vec<u8>,
}

/// The tree of a commit body: its first line, `tree <oid>`.
fn commit_tree(commit: &str, body: &[u8], format: ObjectFormat) -> Result<String, ArchiveError> {
    let length = match format {
        ObjectFormat::Sha1 => 40,
        ObjectFormat::Sha256 => 64,
    };
    body.strip_prefix(b"tree ")
        .and_then(|rest| rest.get(..length + 1))
        .filter(|line| line[length] == b'\n')
        .and_then(|line| std::str::from_utf8(&line[..length]).ok())
        .filter(|oid| is_lower_hex(oid, &[length]))
        .map(str::to_string)
        .ok_or_else(|| ArchiveError::MalformedSource {
            oid: commit.to_string(),
            kind: "commit",
        })
}

/// Every file under tree `oid`, reading each tree object through `reader`
/// so it is checked against its id. `git ls-tree` would parse a damaged
/// loose tree without noticing.
fn walk_tree(
    reader: &mut ObjectReader,
    oid: &str,
    prefix: &[u8],
    out: &mut Vec<TreeEntry>,
) -> Result<(), ArchiveError> {
    let (kind, bytes) = reader.read_whole(oid)?;
    if kind != "tree" {
        return Err(ArchiveError::SourceMismatch {
            oid: oid.to_string(),
        });
    }
    let malformed = || ArchiveError::MalformedSource {
        oid: oid.to_string(),
        kind: "tree",
    };
    let id_length = match reader.format {
        ObjectFormat::Sha1 => 20,
        ObjectFormat::Sha256 => 32,
    };
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        let space = rest.iter().position(|b| *b == b' ').ok_or_else(malformed)?;
        let mode = std::str::from_utf8(&rest[..space]).map_err(|_| malformed())?;
        rest = &rest[space + 1..];
        let nul = rest.iter().position(|b| *b == 0).ok_or_else(malformed)?;
        let name = &rest[..nul];
        rest = &rest[nul + 1..];
        if name.is_empty() || name.contains(&b'/') || rest.len() < id_length {
            return Err(malformed());
        }
        let child = hex(&rest[..id_length]);
        rest = &rest[id_length..];
        let mut path = prefix.to_vec();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(name);
        if mode == "40000" {
            walk_tree(reader, &child, &path, out)?;
        } else {
            out.push(TreeEntry {
                mode: mode.to_string(),
                oid: child,
                path,
            });
        }
    }
    Ok(())
}

/// Attributes that make a Git checkout differ from the committed bytes:
/// `filter` (Git LFS and any clean/smudge driver), `working-tree-encoding`
/// and `ident`. End-of-line conversion is not refused: it is a
/// materialization choice, and the archive restores the committed bytes.
const TRANSFORMING_ATTRIBUTES: &[&str] = &["filter", "working-tree-encoding", "ident"];

/// Refuse a tree any of whose paths has a transforming attribute, from the
/// tree's own `.gitattributes` (read at `commit`, not from the worktree),
/// `.git/info/attributes` or `core.attributesFile`. `git check-attr` reads
/// configuration and attribute files only; it runs no filter, hook or
/// fsmonitor (tested).
fn refuse_transforming_attributes(
    repo: &Path,
    commit: &CommitOid,
    paths: &[&[u8]],
) -> Result<(), ArchiveError> {
    let mut child = git(repo)
        .args([
            "check-attr",
            &format!("--source={}", commit.as_str()),
            "-z",
            "--stdin",
            "--all",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ArchiveError::Git {
            detail: format!("could not start git check-attr: {error}"),
        })?;
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path);
        input.push(0);
    }
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .map_err(|error| ArchiveError::Git {
            detail: format!("git check-attr: {error}"),
        })?;
    let written = writer
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("the path writer panicked")));
    if !output.status.success() || written.is_err() {
        return Err(ArchiveError::Git {
            detail: format!(
                "git check-attr: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let fields: Vec<&[u8]> = output.stdout.split(|b| *b == 0).collect();
    for triple in fields.chunks(3) {
        let [path, attribute, value] = triple else {
            continue;
        };
        let attribute = String::from_utf8_lossy(attribute);
        let value = String::from_utf8_lossy(value);
        if TRANSFORMING_ATTRIBUTES.contains(&attribute.as_ref())
            && value != "unset"
            && value != "unspecified"
        {
            return Err(ArchiveError::UnsupportedFilter {
                path: path.to_vec(),
                attribute: format!("{attribute}={value}"),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- capture

/// Retain the tree of `commit` from `repo`: every blob, the #652 manifest,
/// and a retained-snapshot record. Idempotent: retaining the same commit
/// again copies nothing new and returns the same snapshot.
pub fn retain_snapshot(
    store: &mut CollaborationStore,
    repo: &Path,
    commit: &CommitOid,
) -> Result<RetainedSnapshot, ArchiveError> {
    retain_snapshot_with(store, repo, commit, &mut |_| Ok(()))
}

/// [`retain_snapshot`] with a hook run before each blob is read, for tests
/// that make a source disappear or a copy fail part-way.
pub(crate) fn retain_snapshot_with(
    store: &mut CollaborationStore,
    repo: &Path,
    commit: &CommitOid,
    before_blob: &mut dyn FnMut(usize) -> Result<(), ArchiveError>,
) -> Result<RetainedSnapshot, ArchiveError> {
    refuse_partial_clone(repo)?;
    let mut reader = ObjectReader::open(repo)?;
    let format = reader.format;
    let (kind, body) = reader.read_whole(commit.as_str())?;
    if kind != "commit" {
        return Err(ArchiveError::NotACommit {
            oid: commit.as_str().to_string(),
        });
    }
    let root = commit_tree(commit.as_str(), &body, format)?;
    let mut tree = Vec::new();
    walk_tree(&mut reader, &root, b"", &mut tree)?;
    for entry in &tree {
        // Gitlinks (160000) and any other non-file mode are refused here; a
        // mode #652 accepts is always a blob.
        if EntryKind::from_git_mode(&entry.mode).is_err() {
            return Err(ArchiveError::UnsupportedEntry {
                path: entry.path.clone(),
                mode: entry.mode.clone(),
            });
        }
    }
    let paths: Vec<&[u8]> = tree.iter().map(|entry| entry.path.as_slice()).collect();
    refuse_transforming_attributes(repo, commit, &paths)?;
    // Validate paths before copying anything.
    let provisional = tree
        .iter()
        .map(|entry| {
            SourceEntry::from_content_digest(
                entry.path.clone(),
                EntryKind::from_git_mode(&entry.mode).expect("checked above"),
                [0; 32],
            )
        })
        .collect::<Vec<_>>();
    SourceSnapshot::new(provisional)?;

    let mut entries = Vec::with_capacity(tree.len());
    let mut content_bytes: u64 = 0;
    for (index, entry) in tree.iter().enumerate() {
        before_blob(index)?;
        let Some((kind, size)) = reader.request(&entry.oid)? else {
            return Err(ArchiveError::SourceUnavailable {
                oid: entry.oid.clone(),
                detail: format!(
                    "blob for {} is missing from the repository",
                    String::from_utf8_lossy(&entry.path)
                ),
            });
        };
        if kind != "blob" {
            return Err(ArchiveError::SourceMismatch {
                oid: entry.oid.clone(),
            });
        }
        let mut temporary = new_temporary(store)?;
        let content = reader.copy_body(&entry.oid, &kind, size, temporary.as_file_mut())?;
        let path = temporary.path().to_path_buf();
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| io(&path, source))?;
        publish(store, temporary, ObjectDigest(content))?;
        content_bytes += size;
        entries.push(SourceEntry::from_content_digest(
            entry.path.clone(),
            EntryKind::from_git_mode(&entry.mode).expect("checked above"),
            content,
        ));
    }
    drop(reader);

    let snapshot = SourceSnapshot::new(entries)?;
    let snapshot_id = snapshot.id();
    let manifest = put_object(store, &snapshot.manifest_bytes())?;
    debug_assert_eq!(manifest, ObjectDigest::of_snapshot(&snapshot_id));

    let entry_count = snapshot.entries().len() as u64;
    let (record_bytes, record_id) =
        snapshot_record(&snapshot_id, entry_count, content_bytes, commit, format);
    let record_digest = put_object(store, &record_bytes)?;

    store.connection().execute(
        "INSERT OR IGNORE INTO retained_snapshots
             (snapshot_id, record_id, record_sha256, commit_oid, entry_count, content_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            snapshot_id.as_str(),
            record_id.as_str(),
            record_digest.hex(),
            commit.as_str(),
            i64::try_from(entry_count).unwrap_or(i64::MAX),
            i64::try_from(content_bytes).unwrap_or(i64::MAX),
        ],
    )?;
    // A snapshot first retained from another commit keeps that row; the
    // content is the same, which is all the ID promises.
    retained(store, &snapshot_id)?.ok_or_else(|| ArchiveError::NotRetained {
        id: snapshot_id.to_string(),
    })
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn object(members: Vec<(&str, Value)>) -> Value {
    Value::Object(
        Object::new(
            members
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
        .expect("distinct keys"),
    )
}

fn encode(value: Value, schema: &'static RecordSchema) -> (Vec<u8>, RecordId) {
    let bytes = value.to_canonical_bytes();
    let id = Record::decode(&bytes, &[schema])
        .expect("archive records are valid by construction")
        .id();
    (bytes, id)
}

fn snapshot_record(
    id: &SourceSnapshotId,
    entry_count: u64,
    content_bytes: u64,
    commit: &CommitOid,
    format: ObjectFormat,
) -> (Vec<u8>, RecordId) {
    encode(
        object(vec![
            ("schema", text(RETAINED_SNAPSHOT_SCHEMA_NAME)),
            ("snapshot", text(id.as_str())),
            (
                "entry_count",
                Value::Integer(i64::try_from(entry_count).expect("bounded by memory")),
            ),
            ("content_bytes", text(&content_bytes.to_string())),
            (
                "source",
                object(vec![
                    ("kind", text("git_commit")),
                    ("commit", text(commit.as_str())),
                    ("object_format", text(format.name())),
                ]),
            ),
        ]),
        &RETAINED_SNAPSHOT_SCHEMA,
    )
}

/// Retain a contribution: its base and result snapshots, after checking that
/// `base` is an ancestor of `result`, plus a lineage record.
pub fn retain_contribution(
    store: &mut CollaborationStore,
    repo: &Path,
    base: &CommitOid,
    result: &CommitOid,
) -> Result<RetainedContribution, ArchiveError> {
    retain_contribution_with(store, repo, base, result, &mut |_| Ok(()))
}

/// [`retain_contribution`] with the per-blob hook of [`retain_snapshot_with`],
/// run for both snapshots, so capture tests can interrupt a copy.
pub(crate) fn retain_contribution_with(
    store: &mut CollaborationStore,
    repo: &Path,
    base: &CommitOid,
    result: &CommitOid,
    before_blob: &mut dyn FnMut(usize) -> Result<(), ArchiveError>,
) -> Result<RetainedContribution, ArchiveError> {
    let unavailable = |detail: String| ArchiveError::HistoryUnavailable {
        base: base.as_str().to_string(),
        result: result.as_str().to_string(),
        detail,
    };
    refuse_partial_clone(repo)?;
    // In a shallow clone the boundary hides history, so a "not an ancestor"
    // answer there means "unknown", never "rewritten". Checked first.
    if git_text(repo, &["rev-parse", "--is-shallow-repository"])? == "true" {
        return Err(unavailable("the repository is a shallow clone".into()));
    }
    let ancestry = git_output(
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            base.as_str(),
            result.as_str(),
        ],
    )?;
    match ancestry.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(ArchiveError::BaseNotAncestor {
                base: base.as_str().to_string(),
                result: result.as_str().to_string(),
            });
        }
        _ => {
            return Err(unavailable(
                String::from_utf8_lossy(&ancestry.stderr).trim().to_string(),
            ));
        }
    }
    let count = git_output(
        repo,
        &[
            "rev-list",
            "--count",
            "--first-parent",
            &format!("{}..{}", base.as_str(), result.as_str()),
        ],
    )?;
    if !count.status.success() {
        return Err(unavailable(
            String::from_utf8_lossy(&count.stderr).trim().to_string(),
        ));
    }
    let first_parent_commits: i64 = String::from_utf8_lossy(&count.stdout)
        .trim()
        .parse()
        .map_err(|_| unavailable("unreadable commit count".into()))?;

    let base_snapshot = retain_snapshot_with(store, repo, base, before_blob)?;
    let result_snapshot = retain_snapshot_with(store, repo, result, before_blob)?;
    let format = ObjectReader::open(repo)?.format;
    let (bytes, lineage_record_id) = encode(
        object(vec![
            ("schema", text(CONTRIBUTION_LINEAGE_SCHEMA_NAME)),
            ("base", text(base_snapshot.snapshot_id.as_str())),
            ("result", text(result_snapshot.snapshot_id.as_str())),
            ("base_commit", text(base.as_str())),
            ("result_commit", text(result.as_str())),
            ("object_format", text(format.name())),
            ("first_parent_commits", Value::Integer(first_parent_commits)),
        ]),
        &CONTRIBUTION_LINEAGE_SCHEMA,
    );
    let digest = put_object(store, &bytes)?;
    store.connection().execute(
        "INSERT OR IGNORE INTO retained_contributions
             (lineage_record_id, record_sha256, base_snapshot, result_snapshot)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            lineage_record_id.as_str(),
            digest.hex(),
            base_snapshot.snapshot_id.as_str(),
            result_snapshot.snapshot_id.as_str(),
        ],
    )?;
    Ok(RetainedContribution {
        base: base_snapshot,
        result: result_snapshot,
        lineage_record_id,
    })
}

/// The archive's entry for `id`, if it is fully retained.
pub fn retained(
    store: &CollaborationStore,
    id: &SourceSnapshotId,
) -> Result<Option<RetainedSnapshot>, ArchiveError> {
    let row = store
        .read_connection()
        .query_row(
            "SELECT record_id, commit_oid, entry_count, content_bytes
             FROM retained_snapshots WHERE snapshot_id = ?1",
            [id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((record_id, commit, entry_count, content_bytes)) = row else {
        return Ok(None);
    };
    Ok(Some(RetainedSnapshot {
        snapshot_id: id.clone(),
        record_id: RecordId::parse(&record_id).map_err(|_| ArchiveError::CorruptObject {
            digest: record_id.clone(),
        })?,
        commit: CommitOid::parse(&commit)?,
        entry_count: entry_count.try_into().unwrap_or(0),
        content_bytes: content_bytes.try_into().unwrap_or(0),
    }))
}

// ----------------------------------------------------------- reconstruct

/// Parse a #652 manifest back into a snapshot. The caller has already
/// checked that the bytes hash to the snapshot ID.
fn parse_manifest(bytes: &[u8]) -> Option<SourceSnapshot> {
    use aethyme_contracts::experimental_v0::source_snapshot::MANIFEST_HEADER;

    let mut rest = bytes.strip_prefix(MANIFEST_HEADER)?;
    let mut entries = Vec::new();
    while !rest.is_empty() {
        let end = rest.iter().position(|b| *b == 0)?;
        let record = &rest[..end];
        rest = &rest[end + 1..];
        let (mode, record) = record.split_at(record.iter().position(|b| *b == b' ')?);
        let record = &record[1..];
        let (hex, path) = record.split_at(record.iter().position(|b| *b == b' ')?);
        let path = &path[1..];
        let hex = std::str::from_utf8(hex).ok()?;
        if !is_lower_hex(hex, &[64]) {
            return None;
        }
        let mut digest = [0; 32];
        for (i, byte) in digest.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
        }
        let kind = EntryKind::from_git_mode(std::str::from_utf8(mode).ok()?).ok()?;
        entries.push(SourceEntry::from_content_digest(
            path.to_vec(),
            kind,
            digest,
        ));
    }
    SourceSnapshot::new(entries).ok()
}

/// Write snapshot `id` into `dest` from the archive alone, verifying every
/// object before anything is written. `dest` must be absent or empty.
/// Returns the snapshot, whose ID equals `id`.
pub fn reconstruct(
    store: &CollaborationStore,
    id: &SourceSnapshotId,
    dest: &Path,
) -> Result<SourceSnapshot, ArchiveError> {
    // Only a snapshot this archive retained: any object whose bytes happen
    // to parse as a manifest (a committed file, say) is not one.
    if retained(store, id)?.is_none() {
        return Err(ArchiveError::NotRetained { id: id.to_string() });
    }
    let manifest = read_object(store, &ObjectDigest::of_snapshot(id))?;
    let corrupt = || ArchiveError::CorruptObject {
        digest: ObjectDigest::of_snapshot(id).hex(),
    };
    let snapshot = parse_manifest(&manifest).ok_or_else(corrupt)?;
    // Parsing sorts entries; only the canonical encoding is this snapshot.
    if snapshot.manifest_bytes() != manifest {
        return Err(corrupt());
    }
    // Every object first, so a damaged archive writes nothing.
    for entry in snapshot.entries() {
        let digest = ObjectDigest(*entry.content_sha256());
        if hash_file(&object_path(store, &digest)).map_err(|error| match error {
            ArchiveError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                ArchiveError::MissingObject {
                    digest: digest.hex(),
                }
            }
            other => other,
        })? != digest
        {
            return Err(ArchiveError::CorruptObject {
                digest: digest.hex(),
            });
        }
    }
    if std::fs::symlink_metadata(dest).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(ArchiveError::DestinationIsSymlink {
            path: dest.to_path_buf(),
        });
    }
    match std::fs::read_dir(dest) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(ArchiveError::DestinationNotEmpty {
                    path: dest.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dest).map_err(|source| io(dest, source))?;
        }
        Err(source) => return Err(io(dest, source)),
    }
    materialize(store, &snapshot, dest)?;
    Ok(snapshot)
}

#[cfg(unix)]
fn materialize(
    store: &CollaborationStore,
    snapshot: &SourceSnapshot,
    dest: &Path,
) -> Result<(), ArchiveError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // Directories this call created. A directory that already exists but is
    // not in this set was created under another spelling and folded by the
    // filesystem: a collision, never a merge.
    let mut created: HashSet<Vec<u8>> = HashSet::new();
    for entry in snapshot.entries() {
        let path = entry.path();
        let components: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
        let mut prefix = Vec::new();
        for component in &components[..components.len() - 1] {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(component);
            if created.contains(&prefix) {
                continue;
            }
            let directory = dest.join(std::ffi::OsStr::from_bytes(&prefix));
            match std::fs::create_dir(&directory) {
                Ok(()) => {
                    created.insert(prefix.clone());
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(ArchiveError::MaterializationCollision {
                        path: prefix.clone(),
                    });
                }
                Err(source) => return Err(io(&directory, source)),
            }
        }
        let target = dest.join(std::ffi::OsStr::from_bytes(path));
        let digest = ObjectDigest(*entry.content_sha256());
        let bytes = read_object(store, &digest)?;
        let collision = |error: std::io::Error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ArchiveError::MaterializationCollision {
                    path: path.to_vec(),
                }
            } else {
                io(&target, error)
            }
        };
        match entry.kind() {
            EntryKind::Symlink => {
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&bytes), &target)
                    .map_err(collision)?;
            }
            kind => {
                let mode = if kind == EntryKind::Executable {
                    0o755
                } else {
                    0o644
                };
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&target)
                    .map_err(collision)?;
                file.write_all(&bytes)
                    .map_err(|source| io(&target, source))?;
                // The process umask may have narrowed the mode.
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                    .map_err(|source| io(&target, source))?;
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn materialize(
    _store: &CollaborationStore,
    _snapshot: &SourceSnapshot,
    _dest: &Path,
) -> Result<(), ArchiveError> {
    Err(ArchiveError::Git {
        detail: "materialization is supported on Unix only".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};
    use std::os::unix::fs::PermissionsExt;

    fn git_in(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write(repo: &Path, path: &str, bytes: &[u8]) {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// A repository with a regular file, an executable, a symlink, a nested
    /// directory and an empty file.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git_in(repo, &["init", "-q", "-b", "main"]);
        write(repo, "README.md", b"hello\n");
        write(repo, "bin/run.sh", b"#!/bin/sh\necho run\n");
        std::fs::set_permissions(
            repo.join("bin/run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        write(repo, "src/deep/lib.rs", b"pub fn f() {}\r\n");
        write(repo, "empty", b"");
        std::os::unix::fs::symlink("README.md", repo.join("link")).unwrap();
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "base"]);
        dir
    }

    fn store() -> (tempfile::TempDir, CollaborationStore) {
        let host = tempfile::tempdir().unwrap();
        let store = CollaborationStore::open(
            &CollaborationRoot::under_host_state(host.path()),
            &ProjectKey::parse("proj-a").unwrap(),
            &[],
        )
        .unwrap();
        (host, store)
    }

    fn head(repo: &Path) -> CommitOid {
        pin_commit(repo, "HEAD").unwrap()
    }

    fn object_count(store: &CollaborationStore) -> usize {
        let mut count = 0;
        let Ok(fans) = std::fs::read_dir(objects_dir(store)) else {
            return 0;
        };
        for fan in fans {
            count += std::fs::read_dir(fan.unwrap().path()).unwrap().count();
        }
        count
    }

    fn rows(store: &mut CollaborationStore) -> i64 {
        store
            .connection()
            .query_row("SELECT count(*) FROM retained_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn loose_object(repo: &Path, oid: &str) -> PathBuf {
        repo.join(".git/objects").join(&oid[..2]).join(&oid[2..])
    }

    /// An independent reading of a directory as a #652 snapshot ID: walk the
    /// files, hash their bytes, and hash the manifest, without the archive's
    /// manifest code.
    fn oracle_id(root: &Path) -> String {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(Vec<u8>, &'static str, Vec<u8>)>) {
            use std::os::unix::ffi::OsStrExt;
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec();
                if meta.file_type().is_symlink() {
                    let target = std::fs::read_link(&path).unwrap();
                    out.push((rel, "120000", target.as_os_str().as_bytes().to_vec()));
                } else if meta.is_dir() {
                    walk(root, &path, out);
                } else {
                    let mode = if meta.permissions().mode() & 0o111 != 0 {
                        "100755"
                    } else {
                        "100644"
                    };
                    out.push((rel, mode, std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut entries = Vec::new();
        walk(root, root, &mut entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut manifest = b"aethyme source-snapshot v0\0".to_vec();
        for (path, mode, content) in entries {
            manifest.extend_from_slice(mode.as_bytes());
            manifest.push(b' ');
            manifest.extend_from_slice(hex(&Sha256::digest(&content)).as_bytes());
            manifest.push(b' ');
            manifest.extend_from_slice(&path);
            manifest.push(0);
        }
        format!("sha256:{}", hex(&Sha256::digest(&manifest)))
    }

    /// T10: once retained, the source is rebuilt byte for byte after the
    /// contributor's repository is gone entirely.
    #[test]
    fn a_retained_snapshot_rebuilds_after_the_repository_is_deleted() {
        let source = repo();
        // The committed tree as Git itself checks it out, read independently.
        let checkout = tempfile::tempdir().unwrap();
        git_in(
            source.path(),
            &[
                "--work-tree",
                checkout.path().to_str().unwrap(),
                "checkout",
                "HEAD",
                "--",
                ".",
            ],
        );
        let expected = oracle_id(checkout.path());
        let (_host, mut store) = store();
        let retained = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        assert_eq!(retained.snapshot_id.as_str(), expected);
        assert_eq!(retained.entry_count, 5);
        assert_eq!(retained.content_bytes, 6 + 19 + 15 + 9);

        drop(source);
        let dest = tempfile::tempdir().unwrap();
        let snapshot = reconstruct(&store, &retained.snapshot_id, dest.path()).unwrap();
        assert_eq!(snapshot.id(), retained.snapshot_id);
        assert_eq!(oracle_id(dest.path()), expected);
        assert_eq!(
            std::fs::read(dest.path().join("src/deep/lib.rs")).unwrap(),
            b"pub fn f() {}\r\n"
        );
        assert_eq!(
            std::fs::read_link(dest.path().join("link")).unwrap(),
            Path::new("README.md")
        );
        let mode = std::fs::metadata(dest.path().join("bin/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    /// Objects are private, and their names are the SHA-256 of their bytes.
    #[test]
    fn objects_are_private_and_named_by_their_bytes() {
        let source = repo();
        let (_host, mut store) = store();
        let retained = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        let readme = ObjectDigest::of(b"hello\n");
        let path = object_path(&store, &readme);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let hex: String = store
            .connection()
            .query_row("SELECT record_sha256 FROM retained_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut bytes = [0; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        let record = read_object(&store, &ObjectDigest(bytes)).unwrap();
        let decoded = Record::decode(&record, &[&RETAINED_SNAPSHOT_SCHEMA]).unwrap();
        assert_eq!(decoded.id(), retained.record_id);
        assert_eq!(
            decoded.get("snapshot"),
            Some(&Value::String(retained.snapshot_id.to_string()))
        );
    }

    #[test]
    fn retaining_again_copies_nothing_new() {
        let source = repo();
        let (_host, mut store) = store();
        let first = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        let objects = object_count(&store);
        let second = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        assert_eq!(first, second);
        assert_eq!(object_count(&store), objects);
        assert_eq!(rows(&mut store), 1);
    }

    /// T07: capture takes a pinned commit, so a branch that moves before the
    /// copy does not change what is retained.
    #[test]
    fn a_moved_branch_does_not_change_a_pinned_capture() {
        let source = repo();
        let pinned = head(source.path());
        let (_host, mut store) = store();
        let before = {
            let (_h, mut other) = self::store();
            retain_snapshot(&mut other, source.path(), &pinned)
                .unwrap()
                .snapshot_id
        };
        write(source.path(), "README.md", b"moved\n");
        git_in(source.path(), &["commit", "-qam", "move the branch"]);
        let retained = retain_snapshot(&mut store, source.path(), &pinned).unwrap();
        assert_eq!(retained.snapshot_id, before);
        assert_ne!(head(source.path()), pinned);
        assert_eq!(
            CommitOid::parse("main").unwrap_err().code(),
            "not_an_object_id",
            "capture never takes a ref name"
        );
        let blob =
            CommitOid::parse(&git_in(source.path(), &["rev-parse", "HEAD:README.md"])).unwrap();
        assert_eq!(
            retain_snapshot(&mut store, source.path(), &blob)
                .unwrap_err()
                .code(),
            "not_a_commit"
        );
        assert_eq!(
            pin_commit(source.path(), "no-such-branch")
                .unwrap_err()
                .code(),
            "unknown_revision"
        );
    }

    /// T07: a blob that disappears while the capture runs leaves no complete
    /// snapshot, only orphans that a later capture can reuse.
    #[test]
    fn a_blob_removed_mid_capture_is_incomplete() {
        let source = repo();
        let commit = head(source.path());
        let victim = git_in(source.path(), &["rev-parse", "HEAD:src/deep/lib.rs"]);
        let (_host, mut store) = store();
        let error = retain_snapshot_with(&mut store, source.path(), &commit, &mut |index| {
            if index == 1 {
                std::fs::remove_file(loose_object(source.path(), &victim)).unwrap();
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.code(), "source_unavailable", "{error}");
        assert!(error.is_incomplete());
        assert_eq!(rows(&mut store), 0);
        // The four blobs before it, and no manifest or record.
        assert_eq!(object_count(&store), 4);
    }

    /// A loose object whose bytes are not what its name says is caught while
    /// it is copied, not trusted.
    #[test]
    fn a_source_object_that_does_not_match_its_id_is_incomplete() {
        let source = repo();
        let commit = head(source.path());
        let victim = git_in(source.path(), &["rev-parse", "HEAD:README.md"]);
        let other = git_in(source.path(), &["rev-parse", "HEAD:empty"]);
        let target = loose_object(source.path(), &victim);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::copy(loose_object(source.path(), &other), &target).unwrap();
        let (_host, mut store) = store();
        let error = retain_snapshot(&mut store, source.path(), &commit).unwrap_err();
        assert!(
            matches!(error.code(), "source_mismatch" | "source_unavailable"),
            "{error}"
        );
        assert!(error.is_incomplete());
        assert_eq!(rows(&mut store), 0);
    }

    /// A copy that stops part-way leaves orphans and no index row; a retry
    /// completes and reuses them.
    #[test]
    fn an_interrupted_copy_leaves_orphans_and_a_retry_completes() {
        let source = repo();
        let commit = head(source.path());
        let (_host, mut store) = store();
        let error = retain_snapshot_with(&mut store, source.path(), &commit, &mut |index| {
            if index == 3 {
                Err(ArchiveError::Git {
                    detail: "simulated crash".into(),
                })
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.code(), "git");
        assert_eq!(rows(&mut store), 0);
        assert_eq!(object_count(&store), 3);
        assert_eq!(std::fs::read_dir(spool_dir(&store)).unwrap().count(), 0);

        let retained = retain_snapshot(&mut store, source.path(), &commit).unwrap();
        assert_eq!(rows(&mut store), 1);
        // Five blobs (two share no content), the manifest and the record.
        assert_eq!(object_count(&store), 7);
        let dest = tempfile::tempdir().unwrap();
        reconstruct(&store, &retained.snapshot_id, dest.path()).unwrap();
    }

    #[test]
    fn a_submodule_is_refused_not_skipped() {
        let source = repo();
        let oid = git_in(source.path(), &["rev-parse", "HEAD"]);
        git_in(
            source.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{oid},vendor/sub"),
            ],
        );
        git_in(source.path(), &["commit", "-qm", "add a gitlink"]);
        let (_host, mut store) = store();
        let error = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap_err();
        assert_eq!(error.code(), "unsupported_entry", "{error}");
        assert!(!error.is_incomplete());
        assert_eq!(object_count(&store), 0, "refused before anything is copied");
    }

    /// Commit `files`, then `attributes` as the root `.gitattributes`, so
    /// Git applies no conversion while the fixture itself is built.
    fn commit_with_attributes(repo: &Path, files: &[(&str, &[u8])], attributes: &[u8]) {
        for (path, bytes) in files {
            write(repo, path, bytes);
        }
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "files"]);
        write(repo, ".gitattributes", attributes);
        git_in(repo, &["add", ".gitattributes"]);
        git_in(repo, &["commit", "-qm", "attributes"]);
    }

    #[test]
    fn a_checkout_transforming_attribute_is_refused_and_eol_attributes_are_not() {
        let harmless = repo();
        commit_with_attributes(
            harmless.path(),
            &[("assets/a.png", b"png"), ("assets/b.txt", b"text\n")],
            b"# comment\n*.png -filter\n*.txt text eol=lf\n*.bin filter=lfs\n",
        );
        let (_host, mut store) = store();
        // `*.bin filter=lfs` matches no path in this tree, so nothing differs.
        retain_snapshot(&mut store, harmless.path(), &head(harmless.path())).unwrap();

        for (file, attributes) in [
            (
                "data.bin",
                b"*.bin filter=lfs diff=lfs merge=lfs -text\n".as_slice(),
            ),
            ("notes.txt", b"*.txt working-tree-encoding=UTF-16\n"),
            ("main.c", b"*.c ident\n"),
        ] {
            let source = repo();
            commit_with_attributes(source.path(), &[(file, b"content\n")], attributes);
            let error =
                retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap_err();
            assert_eq!(error.code(), "unsupported_filter", "{file}: {error}");
            assert!(error.to_string().contains(file), "{error}");
        }
    }

    /// Attributes from outside the tree change a checkout too.
    #[test]
    fn attributes_from_outside_the_tree_are_honoured() {
        let source = repo();
        write(source.path(), "data.bin", b"blob\n");
        git_in(source.path(), &["add", "-A"]);
        git_in(source.path(), &["commit", "-qm", "bin"]);
        let (_host, mut store) = store();
        retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();

        write(source.path(), ".git/info/attributes", b"*.bin filter=lfs\n");
        let error = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap_err();
        assert_eq!(error.code(), "unsupported_filter", "{error}");
        std::fs::remove_file(source.path().join(".git/info/attributes")).unwrap();

        let global = source.path().join("global-attributes");
        std::fs::write(&global, b"*.bin ident\n").unwrap();
        git_in(
            source.path(),
            &["config", "core.attributesFile", global.to_str().unwrap()],
        );
        let error = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap_err();
        assert_eq!(error.code(), "unsupported_filter", "{error}");
    }

    /// Capture reads the repository's configuration but runs nothing it
    /// names: no fsmonitor, filter driver or hook.
    #[test]
    fn capture_executes_nothing_the_repository_configures() {
        let source = repo();
        let marker = source.path().join("ran");
        let script = source.path().join("probe.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hooks = source.path().join(".git/hooks");
        for hook in [
            "post-checkout",
            "pre-commit",
            "reference-transaction",
            "post-index-change",
        ] {
            std::fs::copy(&script, hooks.join(hook)).unwrap();
        }
        let script = script.to_str().unwrap();
        for (key, value) in [
            ("core.fsmonitor", script),
            ("filter.probe.clean", script),
            ("filter.probe.smudge", script),
            ("filter.probe.process", script),
            ("diff.probe.textconv", script),
        ] {
            git_in(source.path(), &["config", key, value]);
        }
        write(source.path(), ".git/info/attributes", b"*.md diff=probe\n");
        let (_host, mut store) = store();
        let base = head(source.path());
        retain_contribution(&mut store, source.path(), &base, &base).unwrap();
        assert!(!marker.exists(), "capture ran a configured program");
    }

    #[test]
    fn a_contribution_retains_both_ends_and_its_lineage() {
        let source = repo();
        let base = head(source.path());
        write(source.path(), "README.md", b"changed\n");
        git_in(source.path(), &["commit", "-qam", "one"]);
        write(source.path(), "new.txt", b"new\n");
        git_in(source.path(), &["add", "-A"]);
        git_in(source.path(), &["commit", "-qm", "two"]);
        let result = head(source.path());
        let (_host, mut store) = store();
        let contribution = retain_contribution(&mut store, source.path(), &base, &result).unwrap();
        assert_ne!(
            contribution.base.snapshot_id,
            contribution.result.snapshot_id
        );
        let (digest, base_row): (String, String) = store
            .connection()
            .query_row(
                "SELECT record_sha256, base_snapshot FROM retained_contributions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(base_row, contribution.base.snapshot_id.as_str());
        let mut bytes = [0; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&digest[2 * i..2 * i + 2], 16).unwrap();
        }
        let record = Record::decode(
            &read_object(&store, &ObjectDigest(bytes)).unwrap(),
            &[&CONTRIBUTION_LINEAGE_SCHEMA],
        )
        .unwrap();
        assert_eq!(record.id(), contribution.lineage_record_id);
        assert_eq!(record.get("first_parent_commits"), Some(&Value::Integer(2)));

        // Both ends rebuild with the repository gone.
        drop(source);
        for id in [
            &contribution.base.snapshot_id,
            &contribution.result.snapshot_id,
        ] {
            let dest = tempfile::tempdir().unwrap();
            assert_eq!(&reconstruct(&store, id, dest.path()).unwrap().id(), id);
        }
    }

    /// T07: rewritten history (the base is no longer an ancestor) and missing
    /// history (a shallow clone) never yield a contribution.
    #[test]
    fn rewritten_or_missing_history_is_not_a_contribution() {
        let source = repo();
        let base = head(source.path());
        write(source.path(), "README.md", b"one\n");
        git_in(source.path(), &["commit", "-qam", "one"]);
        let (_host, mut store) = store();

        git_in(
            source.path(),
            &["commit", "-q", "--amend", "-m", "rewritten base"],
        );
        git_in(source.path(), &["checkout", "-q", "-b", "other", "HEAD~1"]);
        write(source.path(), "README.md", b"rewritten\n");
        git_in(
            source.path(),
            &["commit", "-q", "--amend", "-am", "rewrite"],
        );
        let rewritten = head(source.path());
        let error = retain_contribution(&mut store, source.path(), &base, &rewritten).unwrap_err();
        assert_eq!(error.code(), "base_not_ancestor", "{error}");

        git_in(source.path(), &["checkout", "-q", "main"]);
        write(source.path(), "README.md", b"two\n");
        git_in(source.path(), &["commit", "-qam", "two"]);
        let shallow = tempfile::tempdir().unwrap();
        git_in(
            shallow.path(),
            &[
                "clone",
                "-q",
                "--depth",
                "1",
                &format!("file://{}", source.path().display()),
                ".",
            ],
        );
        let tip = head(shallow.path());
        let parent = CommitOid::parse(&git_in(source.path(), &["rev-parse", "HEAD~1"])).unwrap();
        for (base, result) in [(&parent, &tip), (&tip, &tip)] {
            let error = retain_contribution(&mut store, shallow.path(), base, result).unwrap_err();
            assert!(error.is_incomplete(), "{error}");
        }
        assert_eq!(rows(&mut store), 0);
    }

    #[test]
    fn a_damaged_archive_is_refused_and_writes_nothing() {
        let source = repo();
        let (_host, mut store) = store();
        let retained = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        let readme = object_path(&store, &ObjectDigest::of(b"hello\n"));
        std::fs::set_permissions(&readme, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&readme, b"jello\n").unwrap();

        assert_eq!(
            read_object(&store, &ObjectDigest::of(b"hello\n"))
                .unwrap_err()
                .code(),
            "corrupt_object"
        );
        let dest = tempfile::tempdir().unwrap();
        let target = dest.path().join("out");
        let error = reconstruct(&store, &retained.snapshot_id, &target).unwrap_err();
        assert_eq!(error.code(), "corrupt_object", "{error}");
        assert!(!target.exists());

        // Publishing over a damaged object is refused, never overwritten.
        let error = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap_err();
        assert_eq!(error.code(), "corrupt_object", "{error}");
        assert_eq!(std::fs::read(&readme).unwrap(), b"jello\n");

        std::fs::remove_file(&readme).unwrap();
        let error = reconstruct(&store, &retained.snapshot_id, &target).unwrap_err();
        assert_eq!(error.code(), "missing_object", "{error}");
        let unknown = SourceSnapshotId::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap();
        assert_eq!(
            reconstruct(&store, &unknown, &target).unwrap_err().code(),
            "not_retained"
        );
    }

    #[test]
    fn reconstruction_needs_an_empty_destination() {
        let source = repo();
        let (_host, mut store) = store();
        let retained = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(dest.path().join("keep"), b"mine").unwrap();
        let error = reconstruct(&store, &retained.snapshot_id, dest.path()).unwrap_err();
        assert_eq!(error.code(), "destination_not_empty");
        assert_eq!(std::fs::read(dest.path().join("keep")).unwrap(), b"mine");
    }

    /// #652 keeps `A.txt` and `a.txt` distinct. On a case-insensitive
    /// filesystem rebuilding them is refused, whether the fold hits a file or
    /// a directory; on a case-sensitive one every file arrives intact. Never
    /// one file silently replacing another, nor two directories merged.
    #[test]
    fn paths_folded_by_the_filesystem_are_refused_at_materialization() {
        for pair in [["A.txt", "a.txt"], ["Dir/x.txt", "dir/y.txt"]] {
            let source = tempfile::tempdir().unwrap();
            let repo = source.path();
            git_in(repo, &["init", "-q", "-b", "main"]);
            for (path, content) in pair.iter().zip(["upper\n", "lower\n"]) {
                write(repo, "tmp-blob", content.as_bytes());
                let oid = git_in(repo, &["hash-object", "-w", "tmp-blob"]);
                git_in(
                    repo,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("100644,{oid},{path}"),
                    ],
                );
            }
            git_in(repo, &["commit", "-qm", "case pair"]);
            let (_host, mut store) = store();
            let retained = retain_snapshot(&mut store, repo, &head(repo)).unwrap();
            assert_eq!(retained.entry_count, 2);

            let dest = tempfile::tempdir().unwrap();
            let folds = {
                std::fs::write(dest.path().join("Probe"), b"").unwrap();
                let folds = dest.path().join("probe").exists();
                std::fs::remove_file(dest.path().join("Probe")).unwrap();
                folds
            };
            match reconstruct(&store, &retained.snapshot_id, dest.path()) {
                Err(error) => {
                    assert!(folds, "{error}");
                    assert_eq!(error.code(), "materialization_collision", "{error}");
                }
                Ok(_) => {
                    assert!(!folds, "{pair:?} materialized on a folding filesystem");
                    assert_eq!(
                        std::fs::read(dest.path().join(pair[0])).unwrap(),
                        b"upper\n"
                    );
                    assert_eq!(
                        std::fs::read(dest.path().join(pair[1])).unwrap(),
                        b"lower\n"
                    );
                }
            }
        }
    }

    /// Both ends present, each as a shallow boundary: Git answers "not an
    /// ancestor" because it cannot see the history, so the answer is
    /// unknown, never a rewrite.
    #[test]
    fn shallow_boundaries_are_unknown_history_not_a_rewrite() {
        let source = repo();
        git_in(source.path(), &["branch", "base"]);
        write(source.path(), "README.md", b"one\n");
        git_in(source.path(), &["commit", "-qam", "one"]);
        let shallow = tempfile::tempdir().unwrap();
        git_in(
            shallow.path(),
            &[
                "clone",
                "-q",
                "--depth",
                "1",
                "--no-single-branch",
                &format!("file://{}", source.path().display()),
                ".",
            ],
        );
        let base = pin_commit(shallow.path(), "origin/base").unwrap();
        let result = pin_commit(shallow.path(), "origin/main").unwrap();
        let (_host, mut store) = store();
        let error = retain_contribution(&mut store, shallow.path(), &base, &result).unwrap_err();
        assert_eq!(error.code(), "history_unavailable", "{error}");
        assert!(error.is_incomplete());
    }

    /// `git ls-tree` would list a damaged loose tree as if it were genuine.
    /// Every tree is read and checked against its id.
    #[test]
    fn a_tampered_tree_object_is_never_a_complete_capture() {
        let source = repo();
        let root = git_in(source.path(), &["rev-parse", "HEAD^{tree}"]);
        let src = git_in(source.path(), &["rev-parse", "HEAD:src"]);
        let target = loose_object(source.path(), &root);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Still a well-formed tree, just not the one this id names.
        std::fs::copy(loose_object(source.path(), &src), &target).unwrap();
        let (_host, mut store) = store();
        let error = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap_err();
        assert_eq!(error.code(), "source_mismatch", "{error}");
        assert!(error.is_incomplete());
        assert_eq!(rows(&mut store), 0);
    }

    /// A graft file rewrites parents; capture ignores it, so an unrelated
    /// commit never becomes a descendant of the base.
    #[test]
    fn grafts_do_not_change_the_ancestry_answer() {
        let source = repo();
        let base = head(source.path());
        git_in(source.path(), &["checkout", "-q", "--orphan", "unrelated"]);
        write(source.path(), "README.md", b"unrelated\n");
        git_in(source.path(), &["commit", "-qam", "unrelated"]);
        let unrelated = head(source.path());
        write(
            source.path(),
            ".git/info/grafts",
            format!("{} {}\n", unrelated.as_str(), base.as_str()).as_bytes(),
        );
        let (_host, mut store) = store();
        let error = retain_contribution(&mut store, source.path(), &base, &unrelated).unwrap_err();
        assert_eq!(error.code(), "base_not_ancestor", "{error}");
    }

    /// A partial clone could omit objects; capture never fetches them, so
    /// it is reported as incomplete up front.
    #[test]
    fn a_partial_clone_is_incomplete() {
        for (key, value) in [
            ("extensions.partialClone", "origin"),
            ("remote.origin.promisor", "true"),
        ] {
            let source = repo();
            git_in(source.path(), &["config", key, value]);
            let base = head(source.path());
            let (_host, mut store) = store();
            let error = retain_snapshot(&mut store, source.path(), &base).unwrap_err();
            assert_eq!(error.code(), "partial_clone", "{key}: {error}");
            assert!(error.is_incomplete());
            let error = retain_contribution(&mut store, source.path(), &base, &base).unwrap_err();
            assert_eq!(error.code(), "partial_clone", "{key}: {error}");
        }
    }

    /// Only a snapshot the archive retained rebuilds: a committed file whose
    /// bytes are a valid manifest is an ordinary blob, and a manifest that
    /// is not in canonical form is not the snapshot its name claims.
    #[test]
    fn only_a_retained_canonical_manifest_reconstructs() {
        let source = repo();
        let (_host, mut store) = store();
        retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        let readme =
            SourceEntry::from_content(b"README.md".to_vec(), EntryKind::Regular, b"hello\n");
        let empty = SourceEntry::from_content(b"empty".to_vec(), EntryKind::Regular, b"");
        let crafted = SourceSnapshot::new([readme.clone()]).unwrap();
        write(source.path(), "evil", &crafted.manifest_bytes());
        git_in(source.path(), &["add", "-A"]);
        git_in(
            source.path(),
            &["commit", "-qm", "a blob that is a manifest"],
        );
        retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        assert!(has_object(
            &store,
            &ObjectDigest::of_snapshot(&crafted.id())
        ));
        let dest = tempfile::tempdir().unwrap();
        let error = reconstruct(&store, &crafted.id(), dest.path()).unwrap_err();
        assert_eq!(error.code(), "not_retained", "{error}");

        // Entries out of canonical order, recorded as if retained.
        let canonical = SourceSnapshot::new([readme, empty]).unwrap();
        let mut reordered = b"aethyme source-snapshot v0\0".to_vec();
        for entry in canonical.entries().iter().rev() {
            reordered.extend_from_slice(entry.kind().git_mode().as_bytes());
            reordered.push(b' ');
            reordered.extend_from_slice(hex(entry.content_sha256()).as_bytes());
            reordered.push(b' ');
            reordered.extend_from_slice(entry.path());
            reordered.push(0);
        }
        let digest = put_object(&store, &reordered).unwrap();
        let id = SourceSnapshotId::parse(&format!("sha256:{}", digest.hex())).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO retained_snapshots VALUES (?1, ?2, ?3, ?4, 2, 6)",
                rusqlite::params![
                    id.as_str(),
                    format!("sha256:{}", "cd".repeat(32)),
                    "cd".repeat(32),
                    head(source.path()).as_str(),
                ],
            )
            .unwrap();
        let error = reconstruct(&store, &id, dest.path()).unwrap_err();
        assert_eq!(error.code(), "corrupt_object", "{error}");
        assert_eq!(std::fs::read_dir(dest.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_destination_is_refused() {
        let source = repo();
        let (_host, mut store) = store();
        let retained = retain_snapshot(&mut store, source.path(), &head(source.path())).unwrap();
        let place = tempfile::tempdir().unwrap();
        let real = place.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = place.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = reconstruct(&store, &retained.snapshot_id, &link).unwrap_err();
        assert_eq!(error.code(), "destination_is_symlink", "{error}");
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
    }

    /// A version 1 database from #656 gains the archive tables on open.
    #[test]
    fn a_version_one_database_is_migrated() {
        let (host, mut store) = store();
        store
            .connection()
            .execute_batch(
                "DROP TABLE retained_contributions; DROP TABLE retained_snapshots;
                 UPDATE meta SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(store);
        let mut store = CollaborationStore::open(
            &CollaborationRoot::under_host_state(host.path()),
            &ProjectKey::parse("proj-a").unwrap(),
            &[],
        )
        .unwrap();
        assert_eq!(
            store.schema_version(),
            crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION
        );
        assert_eq!(rows(&mut store), 0);
    }
}
