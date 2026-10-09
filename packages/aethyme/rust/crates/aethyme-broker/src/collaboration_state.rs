//! Retained collaboration state, kept apart from broker state and from every
//! legacy cleanup root (#656, plan §6.3, §10.1; D09, D18, D20, D25).
//!
//! ```text
//! <host state>/collaboration/
//!   .aethyme-collaboration-root.json
//!   <project key>/
//!     state.db          one database per project
//!     objects/, spool/  added by the archive and capture slices (#657, #658)
//! ```
//!
//! - **Location.** The root is derived from the host state directory only,
//!   never from a worktree container: no existing deleter enumerates the host
//!   state directory itself (L0 slice C §2-3), so a binary that predates this
//!   module cannot reach it. Opening refuses a root that equals, contains or
//!   lies inside a worktree container, the host cache or the repository
//!   checkout, and a root inside any Git working tree. A refusal names the
//!   setting to change; nothing is moved or repaired implicitly.
//! - **Ownership.** Directories are created `0700` and must stay owned by
//!   the caller with no group or other access; looser ones are refused, not
//!   repaired. A non-empty root without the root marker is refused as foreign.
//! - **Durability.** `state.db` runs in WAL mode with `synchronous=FULL` and
//!   `fullfsync`, so a returned commit has been flushed to the device where
//!   the platform offers that. The filesystem decides whether this is the
//!   supported profile; an unsupported one still opens, and says so in
//!   [`DurabilityProfile`] so a receipt can be labelled differently.
//! - **No cross-store transaction.** Nothing here attaches `broker.db` or
//!   any other database. A collaboration record may name a broker session
//!   as a plain value; it never relies on a write to `broker.db` landing
//!   with its own.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;

pub use crate::host_state::HostStateSource;

/// The schema this binary writes.
pub const COLLABORATION_STATE_SCHEMA_VERSION: i64 = 4;
/// The oldest schema a database written by this binary can be read by.
/// Unlike `host-operations.db`, a newer database stays readable by an older
/// binary until a release raises this floor.
const MIN_COMPATIBLE_SCHEMA: i64 = 1;
const ROOT_DIRECTORY: &str = "collaboration";
/// Additive schema steps after the version 1 layout (`meta` only), applied in
/// order. Each only adds tables, so none raises the compatibility floor.
const MIGRATIONS: &[(i64, &str)] = &[
    (
        2,
        // The archive index (#657). Rows are written only after every object a
        // snapshot or contribution names has been published and verified; the
        // objects themselves are the authority, and these rows are rebuildable.
        "CREATE TABLE IF NOT EXISTS retained_snapshots (
         snapshot_id TEXT PRIMARY KEY NOT NULL,
         record_id TEXT NOT NULL,
         record_sha256 TEXT NOT NULL,
         commit_oid TEXT NOT NULL,
         entry_count INTEGER NOT NULL,
         content_bytes INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE IF NOT EXISTS retained_contributions (
         lineage_record_id TEXT PRIMARY KEY NOT NULL,
         record_sha256 TEXT NOT NULL,
         base_snapshot TEXT NOT NULL REFERENCES retained_snapshots (snapshot_id),
         result_snapshot TEXT NOT NULL REFERENCES retained_snapshots (snapshot_id)
     ) STRICT;",
    ),
    (
        3,
        // The capture journal (#658). An operation row is the retry key and
        // the recovery owner; a receipt, its retention root and its outbox
        // row are committed in one transaction. Retention roots, active
        // operations and reservations are what reclamation (#659) consults.
        "CREATE TABLE IF NOT EXISTS capture_operations (
             operation_id TEXT PRIMARY KEY NOT NULL,
             request_digest TEXT NOT NULL,
             repository TEXT NOT NULL,
             base_commit TEXT NOT NULL,
             result_commit TEXT NOT NULL,
             policy TEXT NOT NULL,
             retention_until_ms INTEGER,
             reserved_bytes INTEGER NOT NULL,
             state TEXT NOT NULL CHECK (state IN ('intent', 'copying', 'sealed', 'committed',
                 'acknowledged', 'incomplete', 'failed', 'refused', 'aborted')),
             lineage_record_id TEXT,
             outcome_code TEXT,
             outcome_detail TEXT,
             created_ms INTEGER NOT NULL,
             updated_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS capture_receipts (
             operation_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES capture_operations (operation_id),
             receipt_record_id TEXT NOT NULL,
             receipt_sha256 TEXT NOT NULL,
             lineage_record_id TEXT NOT NULL
                 REFERENCES retained_contributions (lineage_record_id),
             base_snapshot TEXT NOT NULL,
             result_snapshot TEXT NOT NULL,
             durability TEXT NOT NULL,
             committed_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS retention_roots (
             root_id INTEGER PRIMARY KEY,
             kind TEXT NOT NULL CHECK (kind IN ('capture_intent', 'contribution')),
             operation_id TEXT NOT NULL REFERENCES capture_operations (operation_id),
             lineage_record_id TEXT,
             until_ms INTEGER,
             created_ms INTEGER NOT NULL,
             released_ms INTEGER
         ) STRICT;
         CREATE INDEX IF NOT EXISTS retention_roots_live
             ON retention_roots (kind) WHERE released_ms IS NULL;
         CREATE TABLE IF NOT EXISTS outbox (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             operation_id TEXT NOT NULL UNIQUE REFERENCES capture_operations (operation_id),
             kind TEXT NOT NULL,
             payload_record_id TEXT NOT NULL,
             created_ms INTEGER NOT NULL,
             delivered_ms INTEGER
         ) STRICT;",
    ),
    (
        4,
        // Reclamation (#659). Reader leases and object pins are roots beside
        // retention roots. A reclaimed snapshot or contribution keeps its
        // index row (receipts name it) and gets a marker instead. A
        // generation journals one apply so an interrupted one resumes.
        "CREATE TABLE IF NOT EXISTS reader_leases (
             lease_id INTEGER PRIMARY KEY,
             target_kind TEXT NOT NULL CHECK (target_kind IN ('snapshot', 'contribution')),
             target TEXT NOT NULL,
             holder TEXT NOT NULL,
             until_ms INTEGER NOT NULL,
             created_ms INTEGER NOT NULL,
             released_ms INTEGER
         ) STRICT;
         CREATE TABLE IF NOT EXISTS object_pins (
             pin_id INTEGER PRIMARY KEY,
             class TEXT NOT NULL CHECK (class IN ('analysis_view', 'cited_evidence')),
             object_sha256 TEXT NOT NULL,
             holder TEXT NOT NULL,
             until_ms INTEGER,
             created_ms INTEGER NOT NULL,
             released_ms INTEGER
         ) STRICT;
         CREATE TABLE IF NOT EXISTS reclaimed_snapshots (
             snapshot_id TEXT PRIMARY KEY NOT NULL,
             generation INTEGER NOT NULL,
             reclaimed_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS reclaimed_contributions (
             lineage_record_id TEXT PRIMARY KEY NOT NULL,
             generation INTEGER NOT NULL,
             reclaimed_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS gc_plans (
             digest TEXT PRIMARY KEY NOT NULL,
             body TEXT NOT NULL,
             created_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS gc_generations (
             generation INTEGER PRIMARY KEY,
             plan_digest TEXT NOT NULL,
             state TEXT NOT NULL CHECK (state IN ('trashing', 'trashed', 'done')),
             started_ms INTEGER NOT NULL,
             finished_ms INTEGER
         ) STRICT;
         CREATE TABLE IF NOT EXISTS gc_trash (
             generation INTEGER NOT NULL REFERENCES gc_generations (generation),
             relpath TEXT NOT NULL,
             bytes INTEGER NOT NULL,
             PRIMARY KEY (generation, relpath)
         ) STRICT;",
    ),
];
/// The file that marks a directory as a collaboration root.
pub const ROOT_MARKER: &str = ".aethyme-collaboration-root.json";
const ROOT_MARKER_KIND: &str = "aethyme-collaboration-root";
const STATE_DATABASE: &str = "state.db";

/// The directory name of one project under the collaboration root.
///
/// Opaque: lowercase ASCII letters, digits and `-`, at most 64 bytes. It will
/// be the enrolled ProjectId (#652; `proj:<base32>` stored as `proj-<base32>`)
/// and must never be the path-derived repository key, which changes when a
/// checkout moves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectKey(String);

impl ProjectKey {
    pub fn parse(text: &str) -> Result<Self, CollaborationStateError> {
        let bytes = text.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 64
            && bytes[0] != b'-'
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err(CollaborationStateError::InvalidProjectKey {
                key: text.to_string(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the collaboration root came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "setting")]
pub enum RootSource {
    /// `<host state>/collaboration`, with the setting that chose the host
    /// state directory.
    HostState(HostStateSource),
    /// Named by the caller.
    Explicit,
}

/// The directory holding every project's collaboration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationRoot {
    path: PathBuf,
    source: RootSource,
}

impl CollaborationRoot {
    /// `<host state>/collaboration`.
    pub fn resolve() -> Result<Self, CollaborationStateError> {
        let (host_state, source) = crate::host_state::resolve_host_state_dir()
            .ok_or(CollaborationStateError::Unavailable)?;
        Ok(Self {
            path: host_state.join(ROOT_DIRECTORY),
            source: RootSource::HostState(source),
        })
    }

    /// The root under an explicitly named host state directory.
    pub fn under_host_state(host_state: &Path) -> Self {
        Self {
            path: host_state.join(ROOT_DIRECTORY),
            source: RootSource::Explicit,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source(&self) -> RootSource {
        self.source
    }
}

/// A directory the collaboration root must stay disjoint from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForbiddenRoot {
    pub path: PathBuf,
    pub kind: ForbiddenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForbiddenKind {
    /// Holds session worktree roots; the orphan sweep and storage reclaim
    /// enumerate its children.
    WorktreeContainer,
    /// Swept by gate-cache GC and by the operating system.
    HostCache,
    /// The repository checkout, including its `.aethyme/` runtime files.
    RepositoryCheckout,
}

impl ForbiddenKind {
    fn describe(self) -> &'static str {
        match self {
            Self::WorktreeContainer => "worktree container",
            Self::HostCache => "host cache",
            Self::RepositoryCheckout => "repository checkout",
        }
    }

    fn remedy(self) -> &'static str {
        match self {
            Self::WorktreeContainer => {
                "move the worktree container (AETHYME_WORKTREE_ROOT or the repository's \
                 [worktrees] root) so it neither contains nor sits inside the host state directory"
            }
            Self::HostCache => {
                "point AETHYME_HOST_CACHE_DIR (or XDG_CACHE_HOME) away from the host state directory"
            }
            Self::RepositoryCheckout => {
                "point AETHYME_HOST_STATE_DIR (or XDG_STATE_HOME) outside the repository"
            }
        }
    }
}

/// Every directory the collaboration root of `main_root` must avoid: each
/// worktree container and host cache any process on this host could resolve
/// for this repository, and the checkout itself.
///
/// A container is listed for every host state directory a process could
/// resolve, not only this one, because a shell with different settings
/// sweeps its own (L0 slice C §4.1): a state directory named inside the
/// platform default's `worktrees/` would otherwise pass here and be
/// enumerated there.
pub fn forbidden_roots(main_root: &Path) -> Vec<ForbiddenRoot> {
    let absolute = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            main_root.join(path)
        }
    };
    let entry = |path: PathBuf, kind| ForbiddenRoot {
        path: absolute(path),
        kind,
    };
    let mut roots = vec![entry(
        main_root.to_path_buf(),
        ForbiddenKind::RepositoryCheckout,
    )];
    if let Some(container) =
        std::env::var_os("AETHYME_WORKTREE_ROOT").filter(|value| !value.is_empty())
    {
        roots.push(entry(container.into(), ForbiddenKind::WorktreeContainer));
    }
    if let Ok(Some(config)) = crate::worktree_location::WorktreeLocationConfig::load(main_root)
        && let Some(container) = config.root
    {
        roots.push(entry(container, ForbiddenKind::WorktreeContainer));
    }
    let (states, caches) = crate::host_state::host_directory_candidates();
    for state in states {
        roots.push(entry(
            state.join("worktrees"),
            ForbiddenKind::WorktreeContainer,
        ));
    }
    for cache in caches {
        roots.push(entry(cache, ForbiddenKind::HostCache));
    }
    roots
}

/// How a forbidden root overlaps the collaboration root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    Equals,
    Inside,
    Contains,
}

impl Overlap {
    fn describe(self) -> &'static str {
        match self {
            Self::Equals => "is",
            Self::Inside => "is inside",
            Self::Contains => "contains",
        }
    }
}

/// What a returned commit survives on this filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurabilityProfile {
    /// The filesystem type as the kernel names it (`apfs`, `ext4`, ...).
    pub filesystem: String,
    /// False for network and FUSE mounts.
    pub local: bool,
    pub journal_mode: String,
    /// SQLite's `synchronous` level; 2 is `FULL`.
    pub synchronous: i64,
    /// Whether SQLite flushes with `F_FULLFSYNC` (macOS only).
    pub full_fsync: bool,
    /// True only when the filesystem and every setting match the supported
    /// profile documented in `local-v3-l2-state.md`.
    pub supported: bool,
    /// Why the profile is not supported, when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitation: Option<String>,
}

impl DurabilityProfile {
    /// The label a local receipt carries. An unsupported filesystem must not
    /// produce a receipt that reads the same as a supported one (§10.1).
    pub fn receipt_label(&self) -> &'static str {
        if self.supported {
            "local_durable"
        } else {
            "local_unverified"
        }
    }
}

/// One project's collaboration database, open.
#[derive(Debug)]
pub struct CollaborationStore {
    project: ProjectKey,
    project_dir: PathBuf,
    connection: Connection,
    durability: DurabilityProfile,
    schema_version: i64,
}

impl CollaborationStore {
    /// Open, creating if absent, `project`'s state under `root`, after
    /// checking `root` against `forbidden`.
    pub fn open(
        root: &CollaborationRoot,
        project: &ProjectKey,
        forbidden: &[ForbiddenRoot],
    ) -> Result<Self, CollaborationStateError> {
        let root_path = check_location(root.path(), forbidden)?;
        prepare_root(&root_path)?;
        let project_dir = root_path.join(project.as_str());
        ensure_private_directory(&project_dir)?;
        let database = project_dir.join(STATE_DATABASE);
        if !database.exists() {
            // Created private before SQLite opens it, so the database and the
            // WAL and shared-memory files SQLite derives from it are never
            // readable by others, even for a moment.
            create_private_file(&database)?;
            sync_directory(&project_dir)?;
        }
        let mut connection =
            Connection::open(&database).map_err(|source| sqlite(&database, source))?;
        let durability = configure(&connection, &project_dir).map_err(|e| sqlite(&database, e))?;
        let schema_version = initialise(&mut connection, project, &database)?;
        Ok(Self {
            project: project.clone(),
            project_dir,
            connection,
            durability,
            schema_version,
        })
    }

    pub fn project(&self) -> &ProjectKey {
        &self.project
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn durability(&self) -> &DurabilityProfile {
        &self.durability
    }

    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    /// Pretend the store sits on another filesystem profile.
    #[cfg(test)]
    pub(crate) fn set_durability_for_test(&mut self, profile: DurabilityProfile) {
        self.durability = profile;
    }

    /// The database, read-only use.
    pub(crate) fn read_connection(&self) -> &Connection {
        &self.connection
    }

    /// The database, for the capture and archive slices built on this one.
    #[allow(dead_code)]
    pub(crate) fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

/// Open `project`'s state under `<host state>/collaboration`, checked
/// against every root [`forbidden_roots`] lists for `main_root`.
///
/// Like worktree placement, an implicit host state directory is withheld
/// from a repository under the system temporary directory, so a scratch
/// clone or test fixture never writes durable state into the real one.
pub fn open_for_repository(
    main_root: &Path,
    project: &ProjectKey,
) -> Result<CollaborationStore, CollaborationStateError> {
    if !crate::host_state::host_state_dir_is_explicit()
        && crate::host_state::path_is_ephemeral(main_root)
    {
        return Err(CollaborationStateError::EphemeralRepository {
            repository: main_root.to_path_buf(),
        });
    }
    CollaborationStore::open(
        &CollaborationRoot::resolve()?,
        project,
        &forbidden_roots(main_root),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum CollaborationStateError {
    #[error(
        "no host state directory is available because HOME is unset; set AETHYME_HOST_STATE_DIR"
    )]
    Unavailable,
    #[error(
        "repository {} is under the system temporary directory, so the implicit host state \
         directory is withheld; set AETHYME_HOST_STATE_DIR explicitly",
        repository.display()
    )]
    EphemeralRepository { repository: PathBuf },
    #[error("collaboration root {} must be an absolute path", root.display())]
    RelativeRoot { root: PathBuf },
    #[error(
        "collaboration root {} {} the {} {}, which legacy cleanup can reach; {}",
        root.display(),
        overlap.describe(),
        kind.describe(),
        forbidden.display(),
        kind.remedy()
    )]
    Overlap {
        root: PathBuf,
        forbidden: PathBuf,
        kind: ForbiddenKind,
        overlap: Overlap,
    },
    #[error(
        "collaboration root {} is inside the Git working tree {}, where a clean or checkout \
         can remove it; point AETHYME_HOST_STATE_DIR (or XDG_STATE_HOME) outside it",
        root.display(),
        worktree.display()
    )]
    InsideGitWorktree { root: PathBuf, worktree: PathBuf },
    #[error(
        "{} must be owned by this user with no group or other access (found mode {:o}, owner \
         uid {}); fix it by hand with chmod 700, or remove it if it is not Aethyme's",
        path.display(),
        mode,
        owner
    )]
    InsecurePermissions {
        path: PathBuf,
        mode: u32,
        owner: u32,
    },
    #[error(
        "{} already holds files but has no {ROOT_MARKER}, so it is not a collaboration root; \
         move them away or choose another host state directory",
        root.display()
    )]
    ForeignRoot { root: PathBuf },
    #[error(
        "project key {key:?} is invalid: use 1-64 lowercase ASCII letters, digits and '-', \
         not starting with '-'"
    )]
    InvalidProjectKey { key: String },
    #[error(
        "{} has schema {found}, readable only by binaries supporting schema {min_compatible} or \
         later; this binary supports {COLLABORATION_STATE_SCHEMA_VERSION}. Upgrade Aethyme",
        path.display()
    )]
    SchemaTooNew {
        path: PathBuf,
        found: i64,
        min_compatible: i64,
    },
    #[error(
        "{} belongs to project {found:?}, not {expected:?}; a project directory is never \
         renamed or copied into place",
        path.display()
    )]
    ProjectMismatch {
        path: PathBuf,
        expected: String,
        found: String,
    },
    #[error("{} is not a collaboration state database", path.display())]
    NotACollaborationDatabase { path: PathBuf },
    #[error("{}", crate::host_state::describe_host_state_io(path, source))]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {}", path.display(), crate::host_state::describe_host_state_sqlite(source))]
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },
}

impl CollaborationStateError {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::EphemeralRepository { .. } => "ephemeral_repository",
            Self::RelativeRoot { .. } => "relative_root",
            Self::Overlap { .. } => "overlaps_cleanup_root",
            Self::InsideGitWorktree { .. } => "inside_git_worktree",
            Self::InsecurePermissions { .. } => "insecure_permissions",
            Self::ForeignRoot { .. } => "foreign_root",
            Self::InvalidProjectKey { .. } => "invalid_project_key",
            Self::SchemaTooNew { .. } => "schema_too_new",
            Self::ProjectMismatch { .. } => "project_mismatch",
            Self::NotACollaborationDatabase { .. } => "not_a_collaboration_database",
            Self::Io { .. } => "io",
            Self::Sqlite { .. } => "sqlite",
        }
    }
}

fn io(path: &Path, source: std::io::Error) -> CollaborationStateError {
    CollaborationStateError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn sqlite(path: &Path, source: rusqlite::Error) -> CollaborationStateError {
    CollaborationStateError::Sqlite {
        path: path.to_path_buf(),
        source,
    }
}

/// Resolve symlinks through the longest existing ancestor, so `/var` and
/// `/private/var`, or a symlinked container, compare as the same place.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            return rest
                .iter()
                .rev()
                .fold(resolved, |path: PathBuf, name| path.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The resolved root, or why it may not hold collaboration state.
fn check_location(
    root: &Path,
    forbidden: &[ForbiddenRoot],
) -> Result<PathBuf, CollaborationStateError> {
    if !root.is_absolute() {
        return Err(CollaborationStateError::RelativeRoot {
            root: root.to_path_buf(),
        });
    }
    let resolved = resolve(root);
    for entry in forbidden {
        let other = resolve(&entry.path);
        let overlap = if resolved == other {
            Overlap::Equals
        } else if resolved.starts_with(&other) {
            Overlap::Inside
        } else if other.starts_with(&resolved) {
            Overlap::Contains
        } else {
            continue;
        };
        return Err(CollaborationStateError::Overlap {
            root: resolved,
            forbidden: other,
            kind: entry.kind,
            overlap,
        });
    }
    if let Some(worktree) = resolved
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
    {
        return Err(CollaborationStateError::InsideGitWorktree {
            worktree: worktree.to_path_buf(),
            root: resolved,
        });
    }
    Ok(resolved)
}

/// Create the root with its marker, or accept an existing marked root.
fn prepare_root(root: &Path) -> Result<(), CollaborationStateError> {
    let marker = root.join(ROOT_MARKER);
    if marker.is_file() {
        return check_private(root);
    }
    if root.exists() {
        let mut entries = std::fs::read_dir(root).map_err(|source| io(root, source))?;
        if entries.next().is_some() {
            return Err(CollaborationStateError::ForeignRoot {
                root: root.to_path_buf(),
            });
        }
    }
    ensure_private_directory(root)?;
    let body = serde_json::json!({
        "schema_version": 1,
        "kind": ROOT_MARKER_KIND,
        "note": "Retained collaboration state: source, records and receipts. Not a worktree \
                 root or a cache. Aethyme cleanup never removes it.",
    });
    let bytes = serde_json::to_vec_pretty(&body).expect("static marker serializes");
    crate::atomic_file::with_synced_temporary(&marker, &bytes, |temporary| {
        std::fs::rename(temporary, &marker)
    })
    .map_err(|source| io(&marker, source))?;
    sync_directory(root)
}

/// Create `path` (and its parents) and require it to be private.
fn ensure_private_directory(path: &Path) -> Result<(), CollaborationStateError> {
    if !path.is_dir() {
        let parent = path.parent().unwrap_or(path);
        std::fs::create_dir_all(path).map_err(|source| io(path, source))?;
        crate::host_state::protect_host_state_path(path, true)
            .map_err(|source| io(path, source))?;
        sync_directory(parent)?;
    }
    check_private(path)
}

#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), CollaborationStateError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if metadata.uid() != me || mode & 0o077 != 0 {
        return Err(CollaborationStateError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
            owner: metadata.uid(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path) -> Result<(), CollaborationStateError> {
    Ok(())
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<(), CollaborationStateError> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        // Another process created it first; its open applies the same mode.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(io(path, source)),
    }
}

#[cfg(not(unix))]
fn create_private_file(_path: &Path) -> Result<(), CollaborationStateError> {
    Ok(())
}

/// Make a directory's entries durable. On macOS `sync_all` issues
/// `F_FULLFSYNC`, which also flushes the device cache.
pub(crate) fn sync_directory(path: &Path) -> Result<(), CollaborationStateError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io(path, source))
}

fn configure(connection: &Connection, directory: &Path) -> rusqlite::Result<DurabilityProfile> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "wal")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "fullfsync", true)?;
    connection.pragma_update(None, "checkpoint_fullfsync", true)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    let full_fsync: bool = connection.pragma_query_value(None, "fullfsync", |row| row.get(0))?;
    let (filesystem, local) = filesystem_of(directory);
    let mut limitations = Vec::new();
    if !local {
        limitations.push(format!(
            "{filesystem} is not a local filesystem, so flush and lock semantics are not known"
        ));
    } else if !SUPPORTED_FILESYSTEMS.contains(&filesystem.as_str()) {
        limitations.push(format!(
            "{filesystem} is not a supported filesystem ({})",
            SUPPORTED_FILESYSTEMS.join(", ")
        ));
    }
    if !journal_mode.eq_ignore_ascii_case("wal") {
        limitations.push(format!("journal mode is {journal_mode}, not wal"));
    }
    if synchronous != 2 {
        limitations.push(format!("synchronous is {synchronous}, not FULL (2)"));
    }
    if cfg!(target_os = "macos") && !full_fsync {
        limitations
            .push("fullfsync is off, so commits are not flushed past the drive cache".into());
    }
    Ok(DurabilityProfile {
        filesystem,
        local,
        journal_mode,
        synchronous,
        full_fsync,
        supported: limitations.is_empty(),
        limitation: (!limitations.is_empty()).then(|| limitations.join("; ")),
    })
}

/// Filesystems whose flush behaviour the supported profile assumes.
#[cfg(target_os = "macos")]
const SUPPORTED_FILESYSTEMS: &[&str] = &["apfs", "hfs"];
#[cfg(not(target_os = "macos"))]
const SUPPORTED_FILESYSTEMS: &[&str] = &["ext4", "xfs", "btrfs"];

/// The filesystem type holding `path`, and whether it is local.
#[cfg(target_os = "macos")]
fn filesystem_of(path: &Path) -> (String, bool) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return ("unknown".into(), false);
    };
    // SAFETY: statfs is a plain struct of integers and byte arrays, for which
    // all-zero is valid; the call only writes into it.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is writable.
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return ("unknown".into(), false);
    }
    // SAFETY: the kernel NUL-terminates f_fstypename within its 16 bytes.
    let name = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) };
    let local = stat.f_flags & libc::MNT_LOCAL as u32 != 0;
    (name.to_string_lossy().into_owned(), local)
}

#[cfg(target_os = "linux")]
fn filesystem_of(path: &Path) -> (String, bool) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return ("unknown".into(), false);
    };
    // SAFETY: statfs is a plain struct of integers, for which all-zero is
    // valid; the call only writes into it.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is writable.
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return ("unknown".into(), false);
    }
    #[allow(clippy::unnecessary_cast)]
    let magic = stat.f_type as i64;
    let (name, local) = match magic {
        0xEF53 => ("ext4", true),
        0x5846_5342 => ("xfs", true),
        0x9123_683E => ("btrfs", true),
        0x0102_1994 => ("tmpfs", true),
        0x6969 => ("nfs", false),
        0x517B | 0xFF53_4D42 | 0xFE53_4D42 => ("smb", false),
        0x6573_5546 => ("fuse", false),
        _ => return (format!("0x{magic:x}"), true),
    };
    (name.to_string(), local)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn filesystem_of(_path: &Path) -> (String, bool) {
    ("unknown".into(), false)
}

/// Create the schema on first open; on later opens check that this binary
/// may read it and that it belongs to `project`. Returns the schema version.
fn initialise(
    connection: &mut Connection,
    project: &ProjectKey,
    path: &Path,
) -> Result<i64, CollaborationStateError> {
    let sqlite = |source| sqlite(path, source);
    let has_meta: bool = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |_| Ok(true),
        )
        .optional()
        .map_err(sqlite)?
        .unwrap_or(false);
    if !has_meta {
        let tables: i64 = connection
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .map_err(sqlite)?;
        if tables != 0 {
            return Err(CollaborationStateError::NotACollaborationDatabase {
                path: path.to_path_buf(),
            });
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite)?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL
                 ) STRICT;",
            )
            .map_err(sqlite)?;
        for (key, value) in [
            // The version 1 layout; MIGRATIONS bring it up to date below.
            ("schema_version", "1".to_string()),
            ("min_compatible_schema", MIN_COMPATIBLE_SCHEMA.to_string()),
            ("project_key", project.as_str().to_string()),
        ] {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
                    (key, value),
                )
                .map_err(sqlite)?;
        }
        transaction.commit().map_err(sqlite)?;
    }
    let meta = |key: &str| -> Result<Option<String>, CollaborationStateError> {
        connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(sqlite)
    };
    let number = |key: &str| -> Result<i64, CollaborationStateError> {
        meta(key)?
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| CollaborationStateError::NotACollaborationDatabase {
                path: path.to_path_buf(),
            })
    };
    let found = number("schema_version")?;
    let min_compatible = number("min_compatible_schema")?;
    if min_compatible > COLLABORATION_STATE_SCHEMA_VERSION {
        return Err(CollaborationStateError::SchemaTooNew {
            path: path.to_path_buf(),
            found,
            min_compatible,
        });
    }
    let owner =
        meta("project_key")?.ok_or_else(|| CollaborationStateError::NotACollaborationDatabase {
            path: path.to_path_buf(),
        })?;
    if owner != project.as_str() {
        return Err(CollaborationStateError::ProjectMismatch {
            path: path.to_path_buf(),
            expected: project.as_str().to_string(),
            found: owner,
        });
    }
    let mut found = found;
    for &(version, sql) in MIGRATIONS {
        if version <= found {
            continue;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite)?;
        // Another process may have migrated while this one waited for the lock.
        let current: i64 = transaction
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite)?;
        if current < version {
            transaction.execute_batch(sql).map_err(sqlite)?;
            transaction
                .execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [version.to_string()],
                )
                .map_err(sqlite)?;
        }
        transaction.commit().map_err(sqlite)?;
        found = found.max(version);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::fs::PermissionsExt;

    fn key(text: &str) -> ProjectKey {
        ProjectKey::parse(text).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn open(
        host: &Path,
        forbidden: &[ForbiddenRoot],
    ) -> Result<CollaborationStore, CollaborationStateError> {
        CollaborationStore::open(
            &CollaborationRoot::under_host_state(host),
            &key("proj-a"),
            forbidden,
        )
    }

    fn forbid(path: &Path, kind: ForbiddenKind) -> ForbiddenRoot {
        ForbiddenRoot {
            path: path.to_path_buf(),
            kind,
        }
    }

    #[test]
    fn project_keys_are_opaque_directory_names() {
        for good in ["a", "proj-7k2m", "0", &"x".repeat(64)] {
            assert!(ProjectKey::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-a",
            "A",
            "a/b",
            "..",
            "a.b",
            "proj:abc",
            &"x".repeat(65),
        ] {
            assert_eq!(
                ProjectKey::parse(bad).unwrap_err().code(),
                "invalid_project_key",
                "{bad}"
            );
        }
    }

    #[test]
    fn first_open_creates_a_private_marked_root_and_a_project_database() {
        let host = tempfile::tempdir().unwrap();
        let store = open(host.path(), &[]).unwrap();
        let root = host.path().join("collaboration");
        assert!(root.join(ROOT_MARKER).is_file());
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(store.project_dir()), 0o700);
        assert_eq!(mode(&store.project_dir().join("state.db")), 0o600);
        // SQLite gives its WAL the database's mode.
        assert_eq!(mode(&store.project_dir().join("state.db-wal")), 0o600);
        assert_eq!(store.schema_version(), COLLABORATION_STATE_SCHEMA_VERSION);

        let profile = store.durability();
        assert_eq!(profile.journal_mode, "wal");
        assert_eq!(profile.synchronous, 2);
        if cfg!(target_os = "macos") {
            assert!(profile.full_fsync);
        }
        assert_eq!(
            profile.supported,
            profile.limitation.is_none(),
            "{profile:?}"
        );
        assert_eq!(
            profile.receipt_label(),
            if profile.supported {
                "local_durable"
            } else {
                "local_unverified"
            }
        );

        // No other database is attached: there is nothing to share a
        // transaction with.
        let names: Vec<String> = store
            .connection
            .prepare("SELECT name FROM pragma_database_list")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(names, ["main"]);
    }

    #[test]
    fn reopening_is_idempotent() {
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path(), &[]).unwrap());
        let marker = std::fs::read(host.path().join("collaboration").join(ROOT_MARKER)).unwrap();
        let again = open(host.path(), &[]).unwrap();
        assert_eq!(again.schema_version(), COLLABORATION_STATE_SCHEMA_VERSION);
        assert_eq!(
            std::fs::read(host.path().join("collaboration").join(ROOT_MARKER)).unwrap(),
            marker
        );
    }

    /// The location rule is about where legacy cleanup can reach, so it is
    /// checked in every direction and through symlinks.
    #[test]
    fn a_root_overlapping_a_cleanup_root_is_refused() {
        let host = tempfile::tempdir().unwrap();
        let root = host.path().join("collaboration");
        let elsewhere = tempfile::tempdir().unwrap();
        let link = elsewhere.path().join("link-to-host");
        std::os::unix::fs::symlink(host.path(), &link).unwrap();
        let cases = [
            (
                forbid(&root, ForbiddenKind::WorktreeContainer),
                Overlap::Equals,
            ),
            (
                forbid(host.path(), ForbiddenKind::WorktreeContainer),
                Overlap::Inside,
            ),
            (forbid(&link, ForbiddenKind::HostCache), Overlap::Inside),
            (
                forbid(host.path(), ForbiddenKind::RepositoryCheckout),
                Overlap::Inside,
            ),
            (
                forbid(&root.join("proj-a/x"), ForbiddenKind::WorktreeContainer),
                Overlap::Contains,
            ),
        ];
        for (forbidden, expected) in cases {
            let error = open(host.path(), std::slice::from_ref(&forbidden)).unwrap_err();
            match &error {
                CollaborationStateError::Overlap { overlap, kind, .. } => {
                    assert_eq!((*overlap, *kind), (expected, forbidden.kind), "{error}");
                }
                other => panic!("{forbidden:?}: {other}"),
            }
            assert!(
                !root.exists(),
                "a refusal must create nothing: {forbidden:?}"
            );
        }
        // A sibling is not an overlap.
        open(
            host.path(),
            &[forbid(
                &host.path().join("worktrees"),
                ForbiddenKind::WorktreeContainer,
            )],
        )
        .unwrap();
    }

    #[test]
    fn a_root_inside_a_git_working_tree_is_refused() {
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir(host.path().join(".git")).unwrap();
        let error = open(&host.path().join("nested"), &[]).unwrap_err();
        assert_eq!(error.code(), "inside_git_worktree", "{error}");
    }

    #[test]
    fn loose_permissions_are_refused_not_repaired() {
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path(), &[]).unwrap());
        let root = host.path().join("collaboration");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = open(host.path(), &[]).unwrap_err();
        assert_eq!(error.code(), "insecure_permissions", "{error}");
        assert_eq!(mode(&root), 0o755);

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = root.join("proj-a");
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(
            open(host.path(), &[]).unwrap_err().code(),
            "insecure_permissions"
        );
    }

    #[test]
    fn an_unmarked_non_empty_root_is_foreign_and_left_alone() {
        let host = tempfile::tempdir().unwrap();
        let root = host.path().join("collaboration");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("someone-else.txt"), "keep\n").unwrap();
        assert_eq!(open(host.path(), &[]).unwrap_err().code(), "foreign_root");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn a_database_from_elsewhere_is_refused() {
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path(), &[]).unwrap());
        let root = host.path().join("collaboration");

        // Copied into another project's place.
        std::fs::rename(root.join("proj-a"), root.join("proj-b")).unwrap();
        let error = CollaborationStore::open(
            &CollaborationRoot::under_host_state(host.path()),
            &key("proj-b"),
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), "project_mismatch", "{error}");

        // Some other SQLite file.
        let project = root.join("proj-c");
        std::fs::create_dir(&project).unwrap();
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o700)).unwrap();
        Connection::open(project.join("state.db"))
            .unwrap()
            .execute_batch("CREATE TABLE unrelated (x)")
            .unwrap();
        let error = CollaborationStore::open(
            &CollaborationRoot::under_host_state(host.path()),
            &key("proj-c"),
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), "not_a_collaboration_database", "{error}");
    }

    /// A newer database stays readable until a release raises the floor; it
    /// never locks every older binary out the way an exact-version check does.
    #[test]
    fn a_newer_schema_opens_until_its_floor_passes_this_binary() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute(
                "UPDATE meta SET value = '7' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(store);
        assert_eq!(open(host.path(), &[]).unwrap().schema_version(), 7);

        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'min_compatible_schema'",
                [(COLLABORATION_STATE_SCHEMA_VERSION + 1).to_string()],
            )
            .unwrap();
        drop(store);
        let error = open(host.path(), &[]).unwrap_err();
        assert_eq!(error.code(), "schema_too_new", "{error}");
    }

    const CRASH_CHILD: &str = "AETHYME_COLLABORATION_CRASH_CHILD";

    /// Run only as the child of the crash test: commit numbered rows, one per
    /// transaction, and report each after its commit returned.
    #[test]
    #[ignore = "child process of a_killed_writer_loses_no_acknowledged_commit"]
    fn crash_child_commits_until_killed() {
        let Some(host) = std::env::var_os(CRASH_CHILD) else {
            return;
        };
        let mut store = open(Path::new(&host), &[]).unwrap();
        let connection = store.connection();
        connection
            .execute_batch("CREATE TABLE probe (n INTEGER PRIMARY KEY, pad BLOB NOT NULL) STRICT")
            .unwrap();
        let stdout = std::io::stdout();
        for n in 1_i64.. {
            connection
                .execute(
                    "INSERT INTO probe (n, pad) VALUES (?1, zeroblob(4096))",
                    [n],
                )
                .unwrap();
            let mut out = stdout.lock();
            std::io::Write::write_all(&mut out, format!("committed {n}\n").as_bytes()).unwrap();
            std::io::Write::flush(&mut out).unwrap();
        }
    }

    /// T08 at the state layer: a writer killed between and during commits
    /// leaves an intact database holding every commit it acknowledged, and
    /// nothing out of order. (Power loss is a documented assumption, not
    /// something a test can produce.)
    #[test]
    fn a_killed_writer_loses_no_acknowledged_commit() {
        let host = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "collaboration_state::tests::crash_child_commits_until_killed",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_CHILD, host.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut acknowledged = 0_i64;
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            if let Some(n) = line.unwrap().strip_prefix("committed ") {
                acknowledged = n.parse().unwrap();
                if acknowledged >= 300 {
                    break;
                }
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            acknowledged >= 300,
            "the child stopped early at {acknowledged}"
        );

        let store = open(host.path(), &[]).unwrap();
        let integrity: String = store
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let (count, max): (i64, i64) = store
            .connection
            .query_row("SELECT count(*), max(n) FROM probe", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!(
            max >= acknowledged,
            "lost acknowledged commits: {max} < {acknowledged}"
        );
        assert_eq!(count, max, "commits are contiguous");
    }
}
