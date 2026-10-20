//! Atomic publication of a new repository-local broker database.
//!
//! Existing databases stay on the ordinary migration path. A new database is
//! built at a private sibling name with its compatibility floor and
//! `fresh_unbound` identity already committed, then published with a
//! no-replace hard link. This module does not create a worktree root or run
//! startup reconciliation; the broker lifecycle gates root-dependent writes
//! until an explicit worktree-start reservation is durably completed.
//!
//! Safe fresh-database publication and identity-bound root writes are currently
//! supported on Unix platforms, including the macOS/Linux target family. A
//! non-Unix build refuses an absent database and refuses identity-bound v53
//! rows; compatible legacy-unbound databases retain their existing migration
//! path. Equivalent safe filesystem identity and no-replace publication
//! primitives are required before enabling fresh bindings elsewhere.

use super::*;

#[cfg(unix)]
const FRESH_DATABASE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const FRESH_DATABASE_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(unix)]
const FRESH_DATABASE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(unix)]
const FRESH_DATABASE_MAX_INTEGRITY_ROWS: usize = 64;

#[cfg(not(unix))]
use super::repository_binding::RepositoryWorktreeBindingState;

#[cfg(unix)]
pub(super) fn open_in_repo(
    repo_root: &Path,
    requested_db_path: &Path,
) -> Result<BrokerStore, BrokerError> {
    unix::open_in_repo(repo_root, requested_db_path)
}

#[cfg(unix)]
pub(super) fn verify_legacy_database_file_for_binding(path: &Path) -> Result<(), BrokerError> {
    unix::verify_legacy_database_file_for_binding(path)
}

#[cfg(not(unix))]
pub(super) fn verify_legacy_database_file_for_binding(_path: &Path) -> Result<(), BrokerError> {
    Err(BrokerError::InvalidRepositoryWorktreeBinding(
        "legacy repository binding writes require Unix no-follow file checks".to_owned(),
    ))
}

/// Serialize root reservation/publication with the fresh-database bootstrap
/// protocol. The caller holds the returned file for the whole state change.
#[cfg(unix)]
pub(crate) fn acquire_repository_binding_lock(
    repo_root: &Path,
    requested_db_path: &Path,
) -> Result<std::fs::File, BrokerError> {
    unix::acquire_repository_binding_lock(repo_root, requested_db_path)
}

#[cfg(not(unix))]
pub(crate) fn acquire_repository_binding_lock(
    _repo_root: &Path,
    _requested_db_path: &Path,
) -> Result<std::fs::File, BrokerError> {
    Err(BrokerError::InvalidRepositoryWorktreeBinding(
        "repository worktree binding writes require Unix no-follow identity checks".to_owned(),
    ))
}

#[cfg(not(unix))]
pub(super) fn open_in_repo(
    _repo_root: &Path,
    requested_db_path: &Path,
) -> Result<BrokerStore, BrokerError> {
    let metadata = match std::fs::symlink_metadata(requested_db_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(BrokerError::InvalidRepositoryWorktreeBinding(
                "safe fresh repository database publication is currently supported only on Unix"
                    .to_owned(),
            ));
        }
        Err(source) => {
            return Err(BrokerError::Io {
                path: requested_db_path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BrokerError::InvalidRepositoryWorktreeBinding(
            "existing broker database is not a regular non-symlink file".to_owned(),
        ));
    }
    let connection = Connection::open_with_flags(
        requested_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(FRESH_DATABASE_BUSY_TIMEOUT)?;
    let version = schema::current_version(&connection)?;
    if version > crate::SCHEMA_VERSION {
        return Err(BrokerError::SchemaTooNew {
            found: version,
            supported: crate::SCHEMA_VERSION,
        });
    }
    if version == 53 {
        let store = BrokerStore {
            conn: connection,
            path: requested_db_path.to_path_buf(),
            _snapshot_dir: None,
        };
        let binding = store.repository_worktree_binding()?;
        if binding.state != RepositoryWorktreeBindingState::LegacyUnbound {
            return Err(BrokerError::InvalidRepositoryWorktreeBinding(
                "identity-bound repository databases require safe Unix no-follow verification"
                    .to_owned(),
            ));
        }
    } else {
        drop(connection);
    }
    BrokerStore::open(requested_db_path)
}

#[cfg(all(test, not(unix)))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn fresh_database_refuses_without_creating_the_database_or_parent() {
        let owner = tempfile::tempdir().unwrap();
        let repo_root = owner.path().join("repository");
        std::fs::create_dir(&repo_root).unwrap();
        let database = repo_root.join(".aethyme/broker.db");

        let error = match open_in_repo(&repo_root, &database) {
            Ok(_) => panic!("non-Unix fresh database publication must refuse"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            BrokerError::InvalidRepositoryWorktreeBinding(message)
                if message.contains("safe fresh repository database publication is currently supported only on Unix")
        ));
        assert!(!database.exists());
        assert!(!repo_root.join(".aethyme").exists());
    }
}

#[cfg(unix)]
mod unix {
    use super::super::repository_binding::{
        RepositoryRootIdentity, RepositoryWorktreeBinding, RepositoryWorktreeBindingOrigin,
        RepositoryWorktreeBindingState,
    };
    use super::*;
    use std::ffi::OsString;
    use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
    use std::io::Read;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct DirectoryIdentity {
        device: u64,
        inode: u64,
        uid: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
        uid: u32,
        mode: u32,
    }

    struct DbPaths {
        logical_path: PathBuf,
        final_path: PathBuf,
        stage_path: PathBuf,
        lock_path: PathBuf,
        parent_path: PathBuf,
        parent_identity: DirectoryIdentity,
    }

    struct IdentityBase {
        repository_root: PathBuf,
        git_common_dir: PathBuf,
        repository_key: String,
        broker_db_path: PathBuf,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct VerifiedRepositoryIdentity {
        origin: RepositoryWorktreeBindingOrigin,
        state: RepositoryWorktreeBindingState,
        repository_root: PathBuf,
        git_common_dir: PathBuf,
        repository_key: String,
        broker_db_path: PathBuf,
        bootstrap_nonce: Option<String>,
    }

    impl VerifiedRepositoryIdentity {
        fn from_fresh(identity: &RepositoryRootIdentity) -> Self {
            Self {
                origin: RepositoryWorktreeBindingOrigin::Fresh,
                state: RepositoryWorktreeBindingState::FreshUnbound,
                repository_root: identity.repository_root.clone(),
                git_common_dir: identity.git_common_dir.clone(),
                repository_key: identity.repository_key.clone(),
                broker_db_path: identity.broker_db_path.clone(),
                bootstrap_nonce: Some(identity.bootstrap_nonce.clone()),
            }
        }

        fn matches_fresh(&self, identity: &RepositoryRootIdentity) -> bool {
            self == &Self::from_fresh(identity)
        }
    }

    struct Header {
        floor: Option<i64>,
        binding: Option<RepositoryWorktreeBinding>,
    }

    /// Resolve and open the fixed parent before touching any sibling name.
    /// The only missing parent this publisher creates is the repository's
    /// direct `.aethyme` directory; an absent override parent is refused.
    pub(super) fn open_in_repo(
        repo_root: &Path,
        requested_db_path: &Path,
    ) -> Result<BrokerStore, BrokerError> {
        let paths = prepare_paths(repo_root, requested_db_path)?;
        let final_present = lstat_optional(&paths.final_path)?.is_some();
        let stage_present = lstat_optional(&paths.stage_path)?.is_some();

        if final_present && !stage_present {
            return open_existing(repo_root, &paths);
        }

        let _bootstrap_lock = acquire_bootstrap_lock(&paths)?;
        verify_parent(&paths)?;
        let final_present = lstat_optional(&paths.final_path)?.is_some();
        let stage_present = lstat_optional(&paths.stage_path)?.is_some();

        match (final_present, stage_present) {
            (true, true) => recover_published_stage_link(repo_root, &paths),
            (true, false) => open_existing_locked(repo_root, &paths, false),
            (false, true) => {
                let base = capture_identity_base(repo_root, &paths.final_path)?;
                let verified = verify_database(
                    &paths,
                    &paths.stage_path,
                    &base,
                    DatabaseProof::UnpublishedStage,
                )?;
                publish_verified_stage(&paths, &base, &verified)
            }
            (false, false) => {
                let base = capture_identity_base(repo_root, &paths.final_path)?;
                let identity = RepositoryRootIdentity {
                    repository_root: base.repository_root.clone(),
                    git_common_dir: base.git_common_dir.clone(),
                    repository_key: base.repository_key.clone(),
                    broker_db_path: base.broker_db_path.clone(),
                    bootstrap_nonce: fresh_nonce(&paths.final_path)?,
                };
                let stage_identity = create_and_initialize_stage(&paths, &identity)?;
                let verified = verify_database(
                    &paths,
                    &paths.stage_path,
                    &base,
                    DatabaseProof::UnpublishedStage,
                )?;
                if !verified.identity.matches_fresh(&identity) || verified.inode != stage_identity {
                    return Err(refusal("new stage identity changed during verification"));
                }
                // The stage remains as inspectable evidence on every uncertain
                // path, including collisions with a readable legacy database.
                publish_verified_stage(&paths, &base, &verified)
            }
        }
    }

    fn prepare_paths(repo_root: &Path, requested_db_path: &Path) -> Result<DbPaths, BrokerError> {
        let logical_path = absolute_path(requested_db_path)?;
        let filename = logical_path
            .file_name()
            .ok_or_else(|| refusal("broker database path has no filename"))?;
        let requested_parent = logical_path
            .parent()
            .ok_or_else(|| refusal("broker database path has no parent"))?;

        match fs::symlink_metadata(requested_parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(refusal("broker database parent is a symlink"));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(refusal("broker database parent is not a directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_default_parent(repo_root, requested_parent)?;
            }
            Err(source) => return Err(io_error(requested_parent, source)),
        }

        let parent_path = fs::canonicalize(requested_parent)
            .map_err(|source| io_error(requested_parent, source))?;
        let (parent_file, parent_identity) = open_verified_parent(&parent_path, None)?;
        drop(parent_file);
        let final_path = parent_path.join(filename);
        let stage_path = sibling_path(&parent_path, filename, ".fresh-stage");
        let lock_path = sibling_path(&parent_path, filename, ".bootstrap.lock");
        Ok(DbPaths {
            logical_path,
            final_path,
            stage_path,
            lock_path,
            parent_path,
            parent_identity,
        })
    }

    fn create_default_parent(repo_root: &Path, requested_parent: &Path) -> Result<(), BrokerError> {
        let canonical_root =
            fs::canonicalize(repo_root).map_err(|source| io_error(repo_root, source))?;
        if requested_parent
            .file_name()
            .is_none_or(|name| name != ".aethyme")
        {
            return Err(refusal(
                "an absent broker database override parent is not created automatically",
            ));
        }
        let parent_of_parent = requested_parent
            .parent()
            .ok_or_else(|| refusal("broker database parent has no repository parent"))?;
        let canonical_parent = fs::canonicalize(parent_of_parent)
            .map_err(|source| io_error(parent_of_parent, source))?;
        if canonical_parent != canonical_root {
            return Err(refusal(
                "only the repository's direct .aethyme directory may be created",
            ));
        }
        let (root_file, root_identity) = open_verified_parent(&canonical_root, None)?;
        drop(root_file);

        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(requested_parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_error(requested_parent, source)),
        }
        let created = fs::symlink_metadata(requested_parent)
            .map_err(|source| io_error(requested_parent, source))?;
        if created.file_type().is_symlink() || !created.is_dir() {
            return Err(refusal(
                "repository .aethyme parent changed during creation",
            ));
        }
        sync_directory(&canonical_root, root_identity)?;
        Ok(())
    }

    fn absolute_path(path: &Path) -> Result<PathBuf, BrokerError> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .map_err(|source| io_error(path, source))
        }
    }

    fn sibling_path(parent: &Path, filename: &std::ffi::OsStr, suffix: &str) -> PathBuf {
        let mut name = OsString::from(".");
        name.push(filename);
        name.push(suffix);
        parent.join(name)
    }

    fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        PathBuf::from(value)
    }

    fn open_verified_parent(
        path: &Path,
        expected: Option<DirectoryIdentity>,
    ) -> Result<(File, DirectoryIdentity), BrokerError> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        let file = options
            .open(path)
            .map_err(|source| io_error(path, source))?;
        let metadata = file.metadata().map_err(|source| io_error(path, source))?;
        let identity = directory_identity(&metadata);
        let uid = effective_uid();
        if !metadata.is_dir()
            || identity.uid != uid
            || metadata.mode() & 0o022 != 0
            || expected.is_some_and(|expected| expected != identity)
        {
            return Err(refusal(format!(
                "broker database parent has unsafe type, owner, permissions, or identity: {}",
                path.display()
            )));
        }
        let named = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        if named.file_type().is_symlink()
            || !named.is_dir()
            || directory_identity(&named) != identity
        {
            return Err(refusal("broker database parent changed while opening"));
        }
        Ok((file, identity))
    }

    fn verify_parent(paths: &DbPaths) -> Result<(), BrokerError> {
        let (file, _identity) =
            open_verified_parent(&paths.parent_path, Some(paths.parent_identity))?;
        drop(file);
        Ok(())
    }

    fn directory_identity(metadata: &Metadata) -> DirectoryIdentity {
        DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
        }
    }

    fn effective_uid() -> u32 {
        // SAFETY: geteuid reads the effective UID and has no pointer arguments.
        unsafe { libc::geteuid() }
    }

    fn sync_directory(path: &Path, expected: DirectoryIdentity) -> Result<(), BrokerError> {
        verify_parent_identity(path, expected)?;
        let (directory, _) = open_verified_parent(path, Some(expected))?;
        directory
            .sync_all()
            .map_err(|source| io_error(path, source))
    }

    fn verify_parent_identity(path: &Path, expected: DirectoryIdentity) -> Result<(), BrokerError> {
        let (directory, _) = open_verified_parent(path, Some(expected))?;
        drop(directory);
        Ok(())
    }

    fn acquire_bootstrap_lock(paths: &DbPaths) -> Result<File, BrokerError> {
        verify_parent(paths)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options
            .open(&paths.lock_path)
            .map_err(|source| io_error(&paths.lock_path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| io_error(&paths.lock_path, source))?;
        let identity = checked_private_file(&metadata, 1, "bootstrap lock")?;
        if metadata.len() != 0 {
            return Err(refusal("bootstrap lock file contains unexpected bytes"));
        }
        verify_named_file(&paths.lock_path, identity, 1, "bootstrap lock")?;
        let deadline = Instant::now() + FRESH_DATABASE_LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(refusal(
                        "fresh repository database bootstrap lock was busy for five seconds",
                    ));
                }
                Err(std::fs::TryLockError::Error(source)) => {
                    return Err(io_error(&paths.lock_path, source));
                }
            }
        }
        verify_parent(paths)?;
        verify_named_file(&paths.lock_path, identity, 1, "bootstrap lock")?;
        Ok(file)
    }

    pub(super) fn acquire_repository_binding_lock(
        repo_root: &Path,
        requested_db_path: &Path,
    ) -> Result<File, BrokerError> {
        let paths = prepare_paths(repo_root, requested_db_path)?;
        acquire_bootstrap_lock(&paths)
    }

    fn lstat_optional(path: &Path) -> Result<Option<Metadata>, BrokerError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error(path, source)),
        }
    }

    fn check_sidecars(db_path: &Path, allow_present: bool) -> Result<(), BrokerError> {
        for suffix in ["-journal", "-wal", "-shm"] {
            let sidecar = append_suffix(db_path, suffix);
            if lstat_optional(&sidecar)?.is_some() && !allow_present {
                return Err(refusal(format!(
                    "fresh database has an unexpected SQLite sidecar: {}",
                    sidecar.display()
                )));
            }
        }
        Ok(())
    }

    /// Existing databases may legitimately have SQLite journal sidecars, but
    /// SQLite must not be allowed to follow an unsafe name before we inspect
    /// the database header. These files are mutable while another connection
    /// is live, so this checks only their named/opened identity and permissions
    /// rather than reading their contents.
    fn check_existing_sidecars(paths: &DbPaths, db_path: &Path) -> Result<(), BrokerError> {
        verify_parent(paths)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let sidecar = append_suffix(db_path, suffix);
            let Some(before) = lstat_optional(&sidecar)? else {
                continue;
            };
            if !before.file_type().is_file()
                || before.uid() != effective_uid()
                || before.nlink() != 1
                || before.mode() & 0o022 != 0
            {
                return Err(refusal(format!(
                    "SQLite sidecar is not an owned, non-group/world-writable regular file with one link: {}",
                    sidecar.display()
                )));
            }
            let expected = file_identity(&before);
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            let file = options
                .open(&sidecar)
                .map_err(|source| io_error(&sidecar, source))?;
            let opened = file
                .metadata()
                .map_err(|source| io_error(&sidecar, source))?;
            if !opened.file_type().is_file()
                || opened.uid() != effective_uid()
                || opened.nlink() != 1
                || opened.mode() & 0o022 != 0
                || file_identity(&opened) != expected
            {
                return Err(refusal(format!(
                    "SQLite sidecar changed while opening: {}",
                    sidecar.display()
                )));
            }
            let named =
                fs::symlink_metadata(&sidecar).map_err(|source| io_error(&sidecar, source))?;
            if !named.file_type().is_file()
                || named.uid() != effective_uid()
                || named.nlink() != 1
                || named.mode() & 0o022 != 0
                || file_identity(&named) != expected
            {
                return Err(refusal(format!(
                    "SQLite sidecar name changed while opening: {}",
                    sidecar.display()
                )));
            }
        }
        verify_parent(paths)
    }

    pub(super) fn verify_legacy_database_file_for_binding(path: &Path) -> Result<(), BrokerError> {
        let (file, identity) = open_legacy_regular(path, 1, "legacy repository database")?;
        drop(file);
        verify_named_legacy_file(path, identity, 1, "legacy repository database")?;
        Ok(())
    }

    fn checked_legacy_file(
        metadata: &Metadata,
        expected_links: u64,
        label: &str,
    ) -> Result<FileIdentity, BrokerError> {
        if !metadata.is_file()
            || metadata.uid() != effective_uid()
            || metadata.mode() & 0o022 != 0
            || metadata.nlink() != expected_links
        {
            return Err(refusal(format!(
                "{label} must be an owned, non-group/world-writable regular file with {expected_links} link(s)"
            )));
        }
        Ok(file_identity(metadata))
    }

    fn verify_named_legacy_file(
        path: &Path,
        expected: FileIdentity,
        expected_links: u64,
        label: &str,
    ) -> Result<Metadata, BrokerError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        let actual = checked_legacy_file(&metadata, expected_links, label)?;
        if actual != expected {
            return Err(refusal(format!("{label} identity changed")));
        }
        Ok(metadata)
    }

    fn open_legacy_regular(
        path: &Path,
        expected_links: u64,
        label: &str,
    ) -> Result<(File, FileIdentity), BrokerError> {
        let before = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        let expected = checked_legacy_file(&before, expected_links, label)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options
            .open(path)
            .map_err(|source| io_error(path, source))?;
        let opened = file.metadata().map_err(|source| io_error(path, source))?;
        if checked_legacy_file(&opened, expected_links, label)? != expected {
            return Err(refusal(format!("{label} changed while opening")));
        }
        verify_named_legacy_file(path, expected, expected_links, label)?;
        Ok((file, expected))
    }

    fn checked_private_file(
        metadata: &Metadata,
        expected_links: u64,
        label: &str,
    ) -> Result<FileIdentity, BrokerError> {
        if !metadata.is_file()
            || metadata.uid() != effective_uid()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != expected_links
        {
            return Err(refusal(format!(
                "{label} must be an owned private regular file with {expected_links} link(s)"
            )));
        }
        Ok(file_identity(metadata))
    }

    fn file_identity(metadata: &Metadata) -> FileIdentity {
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            mode: metadata.mode() & 0o7777,
        }
    }

    fn verify_named_file(
        path: &Path,
        expected: FileIdentity,
        expected_links: u64,
        label: &str,
    ) -> Result<Metadata, BrokerError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        let actual = checked_private_file(&metadata, expected_links, label)?;
        if actual != expected {
            return Err(refusal(format!("{label} identity changed")));
        }
        Ok(metadata)
    }

    fn open_private_regular(
        path: &Path,
        expected_links: u64,
        label: &str,
    ) -> Result<(File, FileIdentity), BrokerError> {
        let before = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        let expected = checked_private_file(&before, expected_links, label)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options
            .open(path)
            .map_err(|source| io_error(path, source))?;
        let opened = file.metadata().map_err(|source| io_error(path, source))?;
        if checked_private_file(&opened, expected_links, label)? != expected {
            return Err(refusal(format!("{label} changed while opening")));
        }
        verify_named_file(path, expected, expected_links, label)?;
        Ok((file, expected))
    }

    fn capture_identity_base(
        repo_root: &Path,
        physical_db_path: &Path,
    ) -> Result<IdentityBase, BrokerError> {
        let repository_root =
            fs::canonicalize(repo_root).map_err(|source| io_error(repo_root, source))?;
        let repo = crate::git::GitRepo::discover(&repository_root).map_err(|error| {
            refusal(format!(
                "could not capture repository Git identity: {error}"
            ))
        })?;
        let discovered_root =
            fs::canonicalize(repo.root()).map_err(|source| io_error(repo.root(), source))?;
        if discovered_root != repository_root {
            return Err(refusal(
                "open_in_repo requires the top-level checkout root, not a nested path",
            ));
        }
        let git_common_dir = fs::canonicalize(repo.git_common_dir().map_err(|error| {
            refusal(format!("could not capture Git common directory: {error}"))
        })?)
        .map_err(|source| io_error(&repository_root, source))?;
        let repository_key =
            crate::host_state::repository_key(&repository_root, Some(&git_common_dir));
        Ok(IdentityBase {
            repository_root,
            git_common_dir,
            repository_key,
            broker_db_path: physical_db_path.to_path_buf(),
        })
    }

    fn fresh_nonce(path: &Path) -> Result<String, BrokerError> {
        let mut bytes = [0u8; 32];
        let mut random = File::open("/dev/urandom").map_err(|source| io_error(path, source))?;
        random
            .read_exact(&mut bytes)
            .map_err(|source| io_error(path, source))?;
        let mut value = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut value, "{byte:02x}")
                .map_err(|_| refusal("could not format bootstrap nonce"))?;
        }
        Ok(value)
    }

    fn create_and_initialize_stage(
        paths: &DbPaths,
        identity: &RepositoryRootIdentity,
    ) -> Result<FileIdentity, BrokerError> {
        verify_parent(paths)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let stage_file = options
            .open(&paths.stage_path)
            .map_err(|source| io_error(&paths.stage_path, source))?;
        let stage_metadata = stage_file
            .metadata()
            .map_err(|source| io_error(&paths.stage_path, source))?;
        let stage_identity = checked_private_file(&stage_metadata, 1, "fresh database stage")?;
        verify_named_file(&paths.stage_path, stage_identity, 1, "fresh database stage")?;
        sync_directory(&paths.parent_path, paths.parent_identity)?;

        let connection = Connection::open_with_flags(
            &paths.stage_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(FRESH_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        if page_size <= 0 {
            return Err(refusal("fresh database has an invalid SQLite page size"));
        }
        let max_pages = i64::try_from(FRESH_DATABASE_MAX_BYTES / page_size as u64)
            .map_err(|_| refusal("fresh database page limit is invalid"))?;
        connection.pragma_update(None, "max_page_count", max_pages)?;
        let configured_max: i64 =
            connection.query_row("PRAGMA max_page_count", [], |row| row.get(0))?;
        if configured_max > max_pages {
            return Err(refusal("SQLite did not retain the fresh database page cap"));
        }

        schema::migrate(&connection)?;
        ensure_page_bound(&connection)?;
        ensure_file_size(&paths.stage_path)?;

        let mut store = BrokerStore {
            conn: connection,
            path: paths.stage_path.clone(),
            _snapshot_dir: None,
        };
        let permit = super::super::open::fresh_database_stage_permit();
        store.initialize_fresh_repository_worktree_binding(&permit, identity)?;
        ensure_page_bound(&store.conn)?;
        ensure_file_size(&paths.stage_path)?;
        drop(store);

        stage_file
            .sync_all()
            .map_err(|source| io_error(&paths.stage_path, source))?;
        verify_parent(paths)?;
        verify_named_file(&paths.stage_path, stage_identity, 1, "fresh database stage")?;
        check_sidecars(&paths.stage_path, false)?;
        Ok(stage_identity)
    }

    fn ensure_file_size(path: &Path) -> Result<(), BrokerError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(refusal("fresh database stage is no longer a regular file"));
        }
        if metadata.len() > FRESH_DATABASE_MAX_BYTES {
            return Err(refusal(format!(
                "fresh database exceeds the {} byte image cap",
                FRESH_DATABASE_MAX_BYTES
            )));
        }
        Ok(())
    }

    fn ensure_page_bound(connection: &Connection) -> Result<(), BrokerError> {
        let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        if page_size <= 0 || page_count < 0 {
            return Err(refusal("fresh database has invalid SQLite page accounting"));
        }
        let bytes = (page_size as u64)
            .checked_mul(page_count as u64)
            .ok_or_else(|| refusal("fresh database page accounting overflowed"))?;
        if bytes > FRESH_DATABASE_MAX_BYTES {
            return Err(refusal("fresh database exceeds its SQLite page cap"));
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum DatabaseProof {
        UnpublishedStage,
        PublishedStageLink { expected_links: u64 },
        PublishedUnbound { expected_links: u64 },
        PublishedIdentityOnly { expected_links: u64 },
        PublishedLegacyBound { expected_links: u64 },
    }

    struct VerifiedDatabase {
        identity: VerifiedRepositoryIdentity,
        inode: FileIdentity,
    }

    fn verify_database(
        paths: &DbPaths,
        db_path: &Path,
        base: &IdentityBase,
        proof: DatabaseProof,
    ) -> Result<VerifiedDatabase, BrokerError> {
        verify_parent(paths)?;
        let (expected_links, is_stage, integrity, cap, existing_sidecars, legacy_bound) =
            match proof {
                DatabaseProof::UnpublishedStage => (1, true, true, true, false, false),
                DatabaseProof::PublishedStageLink { expected_links } => {
                    (expected_links, true, true, true, false, false)
                }
                DatabaseProof::PublishedUnbound { expected_links } => {
                    let stage_linked = expected_links == 2;
                    (
                        expected_links,
                        stage_linked,
                        true,
                        stage_linked,
                        !stage_linked,
                        false,
                    )
                }
                DatabaseProof::PublishedIdentityOnly { expected_links } => {
                    (expected_links, false, false, false, true, false)
                }
                DatabaseProof::PublishedLegacyBound { expected_links } => {
                    (expected_links, false, false, false, true, true)
                }
            };
        if existing_sidecars {
            check_existing_sidecars(paths, db_path)?;
        } else if is_stage {
            check_sidecars(db_path, false)?;
        }
        let (file, inode) = if legacy_bound {
            open_legacy_regular(db_path, expected_links, "legacy repository database")?
        } else {
            open_private_regular(db_path, expected_links, "fresh repository database")?
        };
        let before = file
            .metadata()
            .map_err(|source| io_error(db_path, source))?;
        if cap && before.len() > FRESH_DATABASE_MAX_BYTES {
            return Err(refusal("fresh database stage exceeds the 64 MiB cap"));
        }

        let connection = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(FRESH_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "query_only", true)?;
        let version = strict_schema_version(&connection)?;
        if version != crate::SCHEMA_VERSION {
            return Err(refusal(format!(
                "fresh database schema is {version}, expected {}",
                crate::SCHEMA_VERSION
            )));
        }
        let floor = strict_compatibility_floor(&connection)?;
        if floor != 53 {
            return Err(refusal(format!(
                "fresh database compatibility floor is {floor}, expected 53"
            )));
        }
        let journal_mode: String =
            connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        if is_stage && !journal_mode.eq_ignore_ascii_case("delete") {
            return Err(refusal("fresh stage is not in SQLite DELETE journal mode"));
        }
        if cap {
            ensure_page_bound(&connection)?;
        }
        let store = BrokerStore {
            conn: connection,
            path: db_path.to_path_buf(),
            _snapshot_dir: None,
        };
        let binding = store.repository_worktree_binding()?;
        let identity = validate_binding_identity(&binding, base)?;
        match proof {
            DatabaseProof::PublishedLegacyBound { .. }
                if identity.origin != RepositoryWorktreeBindingOrigin::Legacy =>
            {
                return Err(refusal(
                    "legacy database proof does not match the persisted repository origin",
                ));
            }
            DatabaseProof::PublishedIdentityOnly { .. }
            | DatabaseProof::PublishedStageLink { .. }
                if identity.origin != RepositoryWorktreeBindingOrigin::Fresh =>
            {
                return Err(refusal(
                    "fresh database proof does not match the persisted repository origin",
                ));
            }
            _ => {}
        }
        match proof {
            DatabaseProof::UnpublishedStage
                if binding.state != RepositoryWorktreeBindingState::FreshUnbound =>
            {
                return Err(refusal("unpublished stage is not fresh_unbound"));
            }
            DatabaseProof::PublishedUnbound { .. }
                if binding.state != RepositoryWorktreeBindingState::FreshUnbound =>
            {
                return Err(refusal(
                    "published fresh database is no longer fresh_unbound",
                ));
            }
            _ => {}
        }
        if integrity {
            verify_integrity(&store.conn)?;
        }
        drop(store);

        if cap {
            ensure_file_size(db_path)?;
        }
        let after = file
            .metadata()
            .map_err(|source| io_error(db_path, source))?;
        let named = if legacy_bound {
            verify_named_legacy_file(db_path, inode, expected_links, "legacy repository database")?
        } else {
            verify_named_file(db_path, inode, expected_links, "fresh repository database")?
        };
        if before.len() != after.len() || after.len() != named.len() {
            return Err(refusal(
                "fresh database size changed during read-only verification",
            ));
        }
        verify_parent(paths)?;
        if existing_sidecars {
            check_existing_sidecars(paths, db_path)?;
        } else if is_stage {
            check_sidecars(db_path, false)?;
        }
        Ok(VerifiedDatabase { identity, inode })
    }

    fn strict_schema_version(connection: &Connection) -> Result<i64, BrokerError> {
        let value: String = connection.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        value
            .parse::<i64>()
            .map_err(|_| refusal("broker schema version is not an integer"))
    }

    fn strict_compatibility_floor(connection: &Connection) -> Result<i64, BrokerError> {
        let value: String = connection.query_row(
            "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
            [],
            |row| row.get(0),
        )?;
        value
            .parse::<i64>()
            .map_err(|_| refusal("broker compatibility floor is not an integer"))
    }

    fn read_compatibility_floor(connection: &Connection) -> Result<Option<i64>, BrokerError> {
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| {
                value
                    .parse::<i64>()
                    .map_err(|_| refusal("broker compatibility floor is not an integer"))
            })
            .transpose()
    }

    fn verify_integrity(connection: &Connection) -> Result<(), BrokerError> {
        // The 64 MiB page cap bounds the input. This build does not enable
        // rusqlite's progress-handler feature, so the scan has a practical
        // size bound but no hard execution deadline.
        let mut statement = connection.prepare("PRAGMA integrity_check")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut messages = Vec::new();
        for row in rows {
            if messages.len() >= FRESH_DATABASE_MAX_INTEGRITY_ROWS {
                return Err(refusal("SQLite integrity result exceeded its row bound"));
            }
            let message = row?;
            if message.len() > 4096 {
                return Err(refusal("SQLite integrity result exceeded its byte bound"));
            }
            messages.push(message);
        }
        if messages.len() != 1 || messages.first().map(String::as_str) != Some("ok") {
            return Err(refusal(
                "fresh database SQLite integrity check did not return exactly ok",
            ));
        }
        Ok(())
    }

    fn validate_binding_identity(
        binding: &RepositoryWorktreeBinding,
        base: &IdentityBase,
    ) -> Result<VerifiedRepositoryIdentity, BrokerError> {
        if binding.repository_root.as_deref() != Some(base.repository_root.as_path())
            || binding.git_common_dir.as_deref() != Some(base.git_common_dir.as_path())
            || binding.repository_key.as_deref() != Some(base.repository_key.as_str())
            || binding.broker_db_path.as_deref() != Some(base.broker_db_path.as_path())
        {
            return Err(refusal(
                "database repository identity does not match this checkout",
            ));
        }
        match (
            binding.origin,
            binding.state,
            binding.bootstrap_nonce.as_deref(),
        ) {
            (
                Some(RepositoryWorktreeBindingOrigin::Fresh),
                RepositoryWorktreeBindingState::FreshUnbound
                | RepositoryWorktreeBindingState::FreshReserved
                | RepositoryWorktreeBindingState::Bound
                | RepositoryWorktreeBindingState::RecoveryRequired,
                Some(nonce),
            ) if nonce.len() == 64
                && nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) =>
            {
                Ok(VerifiedRepositoryIdentity {
                    origin: RepositoryWorktreeBindingOrigin::Fresh,
                    state: binding.state,
                    repository_root: base.repository_root.clone(),
                    git_common_dir: base.git_common_dir.clone(),
                    repository_key: base.repository_key.clone(),
                    broker_db_path: base.broker_db_path.clone(),
                    bootstrap_nonce: Some(nonce.to_owned()),
                })
            }
            (
                Some(RepositoryWorktreeBindingOrigin::Legacy),
                RepositoryWorktreeBindingState::Bound,
                None,
            ) => Ok(VerifiedRepositoryIdentity {
                origin: RepositoryWorktreeBindingOrigin::Legacy,
                state: RepositoryWorktreeBindingState::Bound,
                repository_root: base.repository_root.clone(),
                git_common_dir: base.git_common_dir.clone(),
                repository_key: base.repository_key.clone(),
                broker_db_path: base.broker_db_path.clone(),
                bootstrap_nonce: None,
            }),
            (Some(RepositoryWorktreeBindingOrigin::Fresh), _, _) => Err(refusal(
                "fresh repository binding has an invalid state or bootstrap nonce",
            )),
            (Some(RepositoryWorktreeBindingOrigin::Legacy), _, _) => Err(refusal(
                "legacy repository identity is valid only after an explicit bound transition",
            )),
            (None, _, _) => Err(refusal("database repository identity has no origin")),
        }
    }

    fn read_header(paths: &DbPaths) -> Result<Header, BrokerError> {
        verify_parent(paths)?;
        check_existing_sidecars(paths, &paths.final_path)?;
        let metadata = fs::symlink_metadata(&paths.final_path)
            .map_err(|source| io_error(&paths.final_path, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(refusal(
                "existing broker database is not a regular non-symlink file",
            ));
        }
        let connection = Connection::open_with_flags(
            &paths.final_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(FRESH_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "query_only", true)?;
        let version = read_version_if_present(&connection)?;
        if version != crate::SCHEMA_VERSION {
            return Ok(Header {
                floor: None,
                binding: None,
            });
        }
        let floor = read_compatibility_floor(&connection)?;
        let store = BrokerStore {
            conn: connection,
            path: paths.final_path.clone(),
            _snapshot_dir: None,
        };
        let binding = store.repository_worktree_binding()?;
        Ok(Header {
            floor,
            binding: Some(binding),
        })
    }

    fn read_version_if_present(connection: &Connection) -> Result<i64, BrokerError> {
        let meta_exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if meta_exists.is_none() {
            return Ok(0);
        }
        strict_schema_version(connection)
    }

    fn open_existing(repo_root: &Path, paths: &DbPaths) -> Result<BrokerStore, BrokerError> {
        let header = read_header(paths)?;
        if let Some(binding) = header.binding.as_ref()
            && binding.state != RepositoryWorktreeBindingState::LegacyUnbound
        {
            let _bootstrap_lock = acquire_bootstrap_lock(paths)?;
            verify_parent(paths)?;
            if lstat_optional(&paths.final_path)?.is_none()
                || lstat_optional(&paths.stage_path)?.is_some()
            {
                return Err(refusal(
                    "fresh database names changed while waiting for the bootstrap lock",
                ));
            }
            return open_existing_locked(repo_root, paths, true);
        }
        BrokerStore::open(&paths.logical_path)
    }

    /// Open an existing database while the caller holds the bootstrap lock.
    /// For the fast path, `require_nonlegacy` records that a read-only probe
    /// already found a v53 identity-bound row; if that row changed while the
    /// lock was acquired, refuse instead of falling back to legacy migration.
    fn open_existing_locked(
        repo_root: &Path,
        paths: &DbPaths,
        require_nonlegacy: bool,
    ) -> Result<BrokerStore, BrokerError> {
        check_existing_sidecars(paths, &paths.final_path)?;
        let header = read_header(paths)?;
        if let Some(binding) = header.binding.as_ref()
            && binding.state != RepositoryWorktreeBindingState::LegacyUnbound
        {
            let base = capture_identity_base(repo_root, &paths.final_path)?;
            let identity = validate_binding_identity(binding, &base)?;
            if header.floor.unwrap_or_default() < 53 {
                return Err(refusal(
                    "nonlegacy repository binding exists below compatibility floor 53",
                ));
            }
            let proof = match identity.origin {
                RepositoryWorktreeBindingOrigin::Fresh
                    if binding.state == RepositoryWorktreeBindingState::FreshUnbound =>
                {
                    DatabaseProof::PublishedUnbound { expected_links: 1 }
                }
                RepositoryWorktreeBindingOrigin::Fresh => {
                    DatabaseProof::PublishedIdentityOnly { expected_links: 1 }
                }
                RepositoryWorktreeBindingOrigin::Legacy => {
                    DatabaseProof::PublishedLegacyBound { expected_links: 1 }
                }
            };
            let verified = verify_database(paths, &paths.final_path, &base, proof)?;
            if verified.identity != identity {
                return Err(refusal(
                    "repository database identity changed between read-only checks",
                ));
            }
            check_existing_sidecars(paths, &paths.final_path)?;
            return open_verified_current(&paths.final_path, &paths.logical_path, &identity);
        }
        if require_nonlegacy {
            return Err(refusal(
                "fresh database binding changed while waiting for the bootstrap lock",
            ));
        }
        BrokerStore::open(&paths.logical_path)
    }

    fn recover_published_stage_link(
        repo_root: &Path,
        paths: &DbPaths,
    ) -> Result<BrokerStore, BrokerError> {
        let header = read_header(paths)?;
        let binding = header
            .binding
            .as_ref()
            .ok_or_else(|| refusal("a prior stage collides with an existing legacy database"))?;
        if binding.state == RepositoryWorktreeBindingState::LegacyUnbound {
            return Err(refusal(
                "a prior stage collides with an existing legacy database",
            ));
        }
        let base = capture_identity_base(repo_root, &paths.final_path)?;
        let identity = validate_binding_identity(binding, &base)?;
        if header.floor.unwrap_or_default() < 53 {
            return Err(refusal(
                "published fresh database is below compatibility floor 53",
            ));
        }
        let verified = verify_database(
            paths,
            &paths.final_path,
            &base,
            DatabaseProof::PublishedStageLink { expected_links: 2 },
        )?;
        if verified.identity != identity {
            return Err(refusal(
                "published database identity changed during recovery",
            ));
        }
        check_sidecars(&paths.stage_path, false)?;
        check_sidecars(&paths.final_path, false)?;
        let stage =
            verify_named_file(&paths.stage_path, verified.inode, 2, "published stage link")?;
        let final_meta = verify_named_file(
            &paths.final_path,
            verified.inode,
            2,
            "published final database",
        )?;
        if !same_inode(&stage, &final_meta) {
            return Err(refusal(
                "final database and prior stage are different files",
            ));
        }
        verify_parent(paths)?;
        fs::remove_file(&paths.stage_path).map_err(|source| io_error(&paths.stage_path, source))?;
        sync_directory(&paths.parent_path, paths.parent_identity)?;
        verify_named_file(
            &paths.final_path,
            verified.inode,
            1,
            "published final database",
        )?;
        check_sidecars(&paths.final_path, false)?;
        open_verified_current(&paths.final_path, &paths.logical_path, &identity)
    }

    fn publish_verified_stage(
        paths: &DbPaths,
        base: &IdentityBase,
        verified: &VerifiedDatabase,
    ) -> Result<BrokerStore, BrokerError> {
        verify_parent(paths)?;
        let stage =
            verify_named_file(&paths.stage_path, verified.inode, 1, "fresh database stage")?;
        if stage.len() > FRESH_DATABASE_MAX_BYTES {
            return Err(refusal("fresh database stage exceeds the 64 MiB cap"));
        }
        if lstat_optional(&paths.final_path)?.is_some() {
            return refuse_final_name_collision();
        }
        match fs::hard_link(&paths.stage_path, &paths.final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return refuse_final_name_collision();
            }
            Err(source) => return Err(io_error(&paths.final_path, source)),
        }

        let stage_after =
            verify_named_file(&paths.stage_path, verified.inode, 2, "fresh database stage")?;
        let final_after = verify_named_file(
            &paths.final_path,
            verified.inode,
            2,
            "published final database",
        )?;
        if !same_inode(&stage_after, &final_after) {
            return Err(refusal(
                "published database does not match the verified stage inode",
            ));
        }
        sync_directory(&paths.parent_path, paths.parent_identity)?;

        let published = verify_database(
            paths,
            &paths.final_path,
            base,
            DatabaseProof::PublishedUnbound { expected_links: 2 },
        )?;
        if published.identity != verified.identity {
            return Err(refusal(
                "published database identity differs from the verified stage",
            ));
        }
        let stage_again = verify_named_file(
            &paths.stage_path,
            published.inode,
            2,
            "fresh database stage",
        )?;
        if !same_inode(&stage_again, &final_after) {
            return Err(refusal(
                "stage changed before its final link could be removed",
            ));
        }
        check_sidecars(&paths.stage_path, false)?;
        fs::remove_file(&paths.stage_path).map_err(|source| io_error(&paths.stage_path, source))?;
        sync_directory(&paths.parent_path, paths.parent_identity)?;
        verify_named_file(
            &paths.final_path,
            published.inode,
            1,
            "published final database",
        )?;
        check_sidecars(&paths.final_path, false)?;
        open_verified_current(&paths.final_path, &paths.logical_path, &published.identity)
    }

    fn refuse_final_name_collision<T>() -> Result<T, BrokerError> {
        Err(refusal(
            "a database already occupies the final name; the verified stage is retained for explicit recovery",
        ))
    }

    fn open_verified_current(
        db_path: &Path,
        logical_path: &Path,
        identity: &VerifiedRepositoryIdentity,
    ) -> Result<BrokerStore, BrokerError> {
        let connection = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(FRESH_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "journal_mode", "wal")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&connection)?;
        let store = BrokerStore {
            conn: connection,
            path: logical_path.to_path_buf(),
            _snapshot_dir: None,
        };
        let binding = store.repository_worktree_binding()?;
        if binding.origin != Some(identity.origin)
            || binding.state != identity.state
            || binding.repository_root.as_deref() != Some(identity.repository_root.as_path())
            || binding.git_common_dir.as_deref() != Some(identity.git_common_dir.as_path())
            || binding.repository_key.as_deref() != Some(identity.repository_key.as_str())
            || binding.broker_db_path.as_deref() != Some(identity.broker_db_path.as_path())
            || binding.bootstrap_nonce.as_deref() != identity.bootstrap_nonce.as_deref()
        {
            return Err(refusal(
                "repository identity changed while opening the published database",
            ));
        }
        Ok(store)
    }

    fn same_inode(left: &Metadata, right: &Metadata) -> bool {
        left.dev() == right.dev() && left.ino() == right.ino()
    }

    fn refusal(message: impl Into<String>) -> BrokerError {
        BrokerError::InvalidRepositoryWorktreeBinding(message.into())
    }

    fn io_error(path: &Path, source: std::io::Error) -> BrokerError {
        BrokerError::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::store::repository_binding::{
            LegacyRepositoryRootConfirmation, RepositoryWorktreeRootLayout,
        };
        use std::os::unix::fs::PermissionsExt;

        fn init_repository(repo_root: &Path) {
            let output = std::process::Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(repo_root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git is installed for repository-backed broker tests");
            assert!(
                output.status.success(),
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn create_verified_stage(repo_root: &Path) -> (DbPaths, IdentityBase, VerifiedDatabase) {
            init_repository(repo_root);
            let requested_db_path = repo_root.join(".aethyme/broker.db");
            let paths = prepare_paths(repo_root, &requested_db_path).unwrap();
            let base = capture_identity_base(repo_root, &paths.final_path).unwrap();
            let identity = RepositoryRootIdentity {
                repository_root: base.repository_root.clone(),
                git_common_dir: base.git_common_dir.clone(),
                repository_key: base.repository_key.clone(),
                broker_db_path: base.broker_db_path.clone(),
                bootstrap_nonce: fresh_nonce(&paths.final_path).unwrap(),
            };
            create_and_initialize_stage(&paths, &identity).unwrap();
            let verified = verify_database(
                &paths,
                &paths.stage_path,
                &base,
                DatabaseProof::UnpublishedStage,
            )
            .unwrap();
            assert!(verified.identity.matches_fresh(&identity));
            (paths, base, verified)
        }

        fn prepare_legacy_database(
            repo_root: &Path,
        ) -> (DbPaths, BrokerStore, LegacyRepositoryRootConfirmation) {
            init_repository(repo_root);
            let requested_db_path = repo_root.join(".aethyme/broker.db");
            fs::create_dir_all(requested_db_path.parent().unwrap()).unwrap();
            let paths = prepare_paths(repo_root, &requested_db_path).unwrap();
            let store = BrokerStore::open(&paths.final_path).unwrap();
            let base = capture_identity_base(repo_root, &paths.final_path).unwrap();
            let digest = "c3".repeat(32);
            let confirmation = LegacyRepositoryRootConfirmation {
                repository_root: base.repository_root,
                git_common_dir: base.git_common_dir,
                repository_key: base.repository_key,
                broker_db_path: base.broker_db_path,
                worktree_root: repo_root.join(".worktrees"),
                layout: RepositoryWorktreeRootLayout::DirectRoot,
                source: crate::broker::WorktreeRootSource::LibraryOverride,
                plan_digest: digest.clone(),
                confirmed_digest: digest,
                parent_identity_json: r#"{"device":1,"inode":2}"#.to_owned(),
                worktree_identity_json: r#"{"device":1,"inode":3}"#.to_owned(),
            };
            (paths, store, confirmation)
        }

        fn bind_legacy_database(repo_root: &Path) -> (DbPaths, BrokerStore) {
            let (paths, mut store, confirmation) = prepare_legacy_database(repo_root);
            fs::set_permissions(&paths.final_path, fs::Permissions::from_mode(0o644)).unwrap();
            store
                .bind_legacy_repository_worktree(&confirmation)
                .unwrap();
            (paths, store)
        }

        #[test]
        fn explicitly_bound_legacy_database_reopens_without_a_fresh_nonce() {
            let repo = tempfile::tempdir().unwrap();
            let (paths, store) = bind_legacy_database(repo.path());
            let binding = store.repository_worktree_binding().unwrap();
            assert_eq!(binding.state, RepositoryWorktreeBindingState::Bound);
            assert_eq!(
                binding.origin,
                Some(RepositoryWorktreeBindingOrigin::Legacy)
            );
            assert_eq!(binding.bootstrap_nonce, None);
            drop(store);

            let reopened = open_in_repo(repo.path(), &paths.logical_path)
                .expect("a confirmed legacy binding must reopen with its durable identity");
            let binding = reopened.repository_worktree_binding().unwrap();
            assert_eq!(binding.state, RepositoryWorktreeBindingState::Bound);
            assert_eq!(
                binding.origin,
                Some(RepositoryWorktreeBindingOrigin::Legacy)
            );
            assert_eq!(binding.bootstrap_nonce, None);
        }

        #[test]
        fn legacy_binding_refuses_group_world_writable_database_before_transition() {
            let repo = tempfile::tempdir().unwrap();
            let (paths, mut store, confirmation) = prepare_legacy_database(repo.path());
            fs::set_permissions(&paths.final_path, fs::Permissions::from_mode(0o666)).unwrap();

            let error = store
                .bind_legacy_repository_worktree(&confirmation)
                .expect_err("a writable legacy database must not become bound");
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(message)
                    if message.contains("non-group/world-writable regular file")
            ));
            let binding = store.repository_worktree_binding().unwrap();
            assert_eq!(binding.state, RepositoryWorktreeBindingState::LegacyUnbound);
            assert_eq!(
                binding.origin,
                Some(RepositoryWorktreeBindingOrigin::Legacy)
            );
            assert_eq!(binding.repository_root, None);
            assert_eq!(binding.bootstrap_nonce, None);
        }

        #[test]
        fn bound_legacy_database_refuses_group_world_writable_mode_on_reopen() {
            let repo = tempfile::tempdir().unwrap();
            let (paths, store) = bind_legacy_database(repo.path());
            drop(store);
            fs::set_permissions(&paths.final_path, fs::Permissions::from_mode(0o666)).unwrap();

            let error = match open_in_repo(repo.path(), &paths.logical_path) {
                Ok(_) => panic!("a group/world-writable bound legacy database must refuse"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(message)
                    if message.contains("non-group/world-writable regular file")
            ));
        }

        #[test]
        fn fresh_database_with_legacy_readable_mode_still_refuses() {
            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let database = repo.path().join(".aethyme/broker.db");
            let store = open_in_repo(repo.path(), &database).unwrap();
            let binding = store.repository_worktree_binding().unwrap();
            assert_eq!(binding.origin, Some(RepositoryWorktreeBindingOrigin::Fresh));
            assert_eq!(binding.state, RepositoryWorktreeBindingState::FreshUnbound);
            drop(store);
            fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();

            let error = match open_in_repo(repo.path(), &database) {
                Ok(_) => panic!("a Fresh database must retain its private-file requirement"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(message)
                    if message.contains("owned private regular file")
            ));
        }

        #[test]
        fn explicitly_bound_legacy_database_refuses_a_different_checkout() {
            let repo = tempfile::tempdir().unwrap();
            let other_repo = tempfile::tempdir().unwrap();
            let (paths, store) = bind_legacy_database(repo.path());
            drop(store);
            init_repository(other_repo.path());
            let database_before = fs::read(&paths.final_path).unwrap();

            let error = match open_in_repo(other_repo.path(), &paths.logical_path) {
                Ok(_) => panic!("a legacy binding must not open from a different checkout"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(_)
            ));
            assert_eq!(fs::read(&paths.final_path).unwrap(), database_before);
            assert!(
                !other_repo.path().join(".aethyme").exists(),
                "identity refusal must not create state in the other checkout"
            );
        }

        #[test]
        fn crash_after_durable_final_link_replays_verified_publication() {
            let repo = tempfile::tempdir().unwrap();
            let (paths, _base, verified) = create_verified_stage(repo.path());

            fs::hard_link(&paths.stage_path, &paths.final_path).unwrap();
            sync_directory(&paths.parent_path, paths.parent_identity).unwrap();
            let stage = fs::symlink_metadata(&paths.stage_path).unwrap();
            let final_file = fs::symlink_metadata(&paths.final_path).unwrap();
            assert!(same_inode(&stage, &final_file));
            assert_eq!(stage.nlink(), 2);

            let store = open_in_repo(repo.path(), &paths.logical_path).unwrap();
            assert!(
                !paths.stage_path.exists(),
                "verified replay clears its stage link"
            );
            let binding = store.repository_worktree_binding().unwrap();
            assert_eq!(binding.bootstrap_nonce, verified.identity.bootstrap_nonce);
            assert_eq!(binding.state, RepositoryWorktreeBindingState::FreshUnbound);
            let final_meta = fs::symlink_metadata(&paths.final_path).unwrap();
            assert_eq!(final_meta.nlink(), 1);
        }

        #[test]
        fn crash_before_final_link_replays_the_verified_private_stage() {
            let repo = tempfile::tempdir().unwrap();
            let (paths, _base, verified) = create_verified_stage(repo.path());
            assert!(!paths.final_path.exists());
            assert!(paths.stage_path.exists());

            let store = open_in_repo(repo.path(), &paths.logical_path).unwrap();
            let binding = store.repository_worktree_binding().unwrap();
            assert_eq!(binding.bootstrap_nonce, verified.identity.bootstrap_nonce);
            assert_eq!(binding.state, RepositoryWorktreeBindingState::FreshUnbound);
            assert!(!paths.stage_path.exists());
            assert_eq!(fs::symlink_metadata(&paths.final_path).unwrap().nlink(), 1);
        }

        #[test]
        fn default_parent_is_created_but_absent_override_parent_is_refused() {
            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let requested_db_path = repo.path().join(".aethyme/broker.db");
            assert!(!requested_db_path.parent().unwrap().exists());

            let store = open_in_repo(repo.path(), &requested_db_path).unwrap();
            assert!(requested_db_path.parent().unwrap().is_dir());
            assert_eq!(
                store.repository_worktree_binding().unwrap().state,
                RepositoryWorktreeBindingState::FreshUnbound
            );
            drop(store);

            let missing_override_parent = repo.path().join("external-broker-state");
            let error = match open_in_repo(repo.path(), &missing_override_parent.join("broker.db"))
            {
                Ok(_) => panic!("an absent override parent must be refused"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(_)
            ));
            assert!(!missing_override_parent.exists());
        }

        #[test]
        fn existing_fresh_open_waits_for_the_bootstrap_lock() {
            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let requested_db_path = repo.path().join(".aethyme/broker.db");
            let store = open_in_repo(repo.path(), &requested_db_path).unwrap();
            assert_eq!(
                store.repository_worktree_binding().unwrap().state,
                RepositoryWorktreeBindingState::FreshUnbound
            );
            drop(store);

            let paths = prepare_paths(repo.path(), &requested_db_path).unwrap();
            let lock = acquire_bootstrap_lock(&paths).unwrap();
            let repo_root = repo.path().to_path_buf();
            let db_path = paths.logical_path.clone();
            let (started_sender, started_receiver) = std::sync::mpsc::channel();
            let (sender, receiver) = std::sync::mpsc::channel();
            let opener = thread::spawn(move || {
                started_sender.send(()).unwrap();
                let result = open_in_repo(&repo_root, &db_path)
                    .and_then(|store| store.repository_worktree_binding())
                    .map(|binding| binding.state)
                    .map_err(|error| error.to_string());
                sender.send(result).unwrap();
            });

            started_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            assert!(matches!(
                receiver.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            drop(lock);
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap(),
                RepositoryWorktreeBindingState::FreshUnbound
            );
            opener.join().unwrap();
        }

        #[test]
        fn existing_fresh_open_rereads_identity_after_acquiring_the_lock() {
            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let requested_db_path = repo.path().join(".aethyme/broker.db");
            let store = open_in_repo(repo.path(), &requested_db_path).unwrap();
            drop(store);

            let paths = prepare_paths(repo.path(), &requested_db_path).unwrap();
            let header = read_header(&paths).unwrap();
            assert!(header.binding.as_ref().is_some_and(|binding| {
                binding.state == RepositoryWorktreeBindingState::FreshUnbound
            }));
            let lock = acquire_bootstrap_lock(&paths).unwrap();
            let connection = Connection::open(&paths.final_path).unwrap();
            connection
                .execute(
                    "UPDATE repository_worktree_binding SET origin = 'legacy' WHERE singleton = 1",
                    [],
                )
                .unwrap();
            drop(connection);

            let error = match open_existing_locked(repo.path(), &paths, true) {
                Ok(_) => panic!("a changed fresh identity must be refused"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(_)
            ));
            drop(lock);
        }

        #[test]
        fn regular_existing_wal_sidecars_are_accepted() {
            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let db_path = repo.path().join(".aethyme/broker.db");
            let store = open_in_repo(repo.path(), &db_path).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO meta (key, value) VALUES ('test.wal_sidecar', 'present')",
                    [],
                )
                .unwrap();
            let paths = prepare_paths(repo.path(), &db_path).unwrap();
            let wal_path = append_suffix(&paths.final_path, "-wal");
            assert!(wal_path.is_file(), "the open connection retains its WAL");
            check_existing_sidecars(&paths, &paths.final_path).unwrap();

            let reopened = open_in_repo(repo.path(), &db_path).unwrap();
            assert_eq!(
                reopened.repository_worktree_binding().unwrap().state,
                RepositoryWorktreeBindingState::FreshUnbound
            );
            assert!(wal_path.is_file());
            drop(reopened);
            drop(store);
        }

        #[test]
        fn symlink_sidecar_is_refused_before_sqlite_opens_the_database() {
            use std::os::unix::fs::symlink;

            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let db_path = repo.path().join(".aethyme/broker.db");
            drop(open_in_repo(repo.path(), &db_path).unwrap());
            let paths = prepare_paths(repo.path(), &db_path).unwrap();
            let sentinel = paths.parent_path.join("sidecar-sentinel");
            fs::write(&sentinel, b"preserve this file").unwrap();
            let sentinel_before = fs::read(&sentinel).unwrap();
            let database_before = fs::read(&paths.final_path).unwrap();
            let wal_path = append_suffix(&paths.final_path, "-wal");
            symlink(&sentinel, &wal_path).unwrap();

            let error = match open_in_repo(repo.path(), &db_path) {
                Ok(_) => panic!("a symlink SQLite sidecar must be refused"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(_)
            ));
            assert_eq!(fs::read(&sentinel).unwrap(), sentinel_before);
            assert_eq!(fs::read(&paths.final_path).unwrap(), database_before);
            assert!(
                fs::symlink_metadata(&wal_path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        #[test]
        fn multiply_linked_sidecar_is_refused_before_database_open() {
            let repo = tempfile::tempdir().unwrap();
            init_repository(repo.path());
            let db_path = repo.path().join(".aethyme/broker.db");
            drop(open_in_repo(repo.path(), &db_path).unwrap());
            let paths = prepare_paths(repo.path(), &db_path).unwrap();
            let sentinel = paths.parent_path.join("sidecar-sentinel");
            fs::write(&sentinel, b"preserve this file").unwrap();
            let sentinel_before = fs::read(&sentinel).unwrap();
            let database_before = fs::read(&paths.final_path).unwrap();
            let wal_path = append_suffix(&paths.final_path, "-wal");
            fs::hard_link(&sentinel, &wal_path).unwrap();
            assert_eq!(fs::symlink_metadata(&wal_path).unwrap().nlink(), 2);

            let error = match open_in_repo(repo.path(), &db_path) {
                Ok(_) => panic!("a multiply-linked SQLite sidecar must be refused"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BrokerError::InvalidRepositoryWorktreeBinding(_)
            ));
            assert_eq!(fs::read(&sentinel).unwrap(), sentinel_before);
            assert_eq!(fs::read(&paths.final_path).unwrap(), database_before);
            assert_eq!(fs::symlink_metadata(&wal_path).unwrap().nlink(), 2);
        }

        #[test]
        fn legacy_final_collision_refuses_and_preserves_both_database_files() {
            let repo = tempfile::tempdir().unwrap();
            let (paths, base, verified) = create_verified_stage(repo.path());

            let legacy = BrokerStore::open(&paths.final_path).unwrap();
            assert_eq!(
                legacy.repository_worktree_binding().unwrap().state,
                RepositoryWorktreeBindingState::LegacyUnbound
            );
            drop(legacy);
            let final_before = fs::read(&paths.final_path).unwrap();
            let stage_before = fs::read(&paths.stage_path).unwrap();
            assert!(read_header(&paths).unwrap().binding.is_some_and(|binding| {
                binding.state == RepositoryWorktreeBindingState::LegacyUnbound
            }));

            let publish_error = match publish_verified_stage(&paths, &base, &verified) {
                Ok(_) => panic!("a legacy final-name collision must be refused"),
                Err(error) => error,
            };
            let replay_error = match open_in_repo(repo.path(), &paths.logical_path) {
                Ok(_) => panic!("a legacy final-name collision must be refused on retry"),
                Err(error) => error,
            };
            for error in [publish_error, replay_error] {
                assert!(matches!(
                    error,
                    BrokerError::InvalidRepositoryWorktreeBinding(_)
                ));
                assert_eq!(fs::read(&paths.final_path).unwrap(), final_before);
                assert_eq!(fs::read(&paths.stage_path).unwrap(), stage_before);
            }
        }
    }
}
