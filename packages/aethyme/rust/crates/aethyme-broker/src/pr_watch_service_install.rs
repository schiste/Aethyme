//! Install exact reviewed service files without loading or replacing a service.
use crate::pr_watch_service::{Plan, script_contents};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

pub(crate) mod lifecycle;

const MAX_FILE_BYTES: u64 = 64 * 1024;
const SCRATCH: &str = ".install.pending";

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

fn private_file(metadata: &Metadata, uid: u32, links: u64) -> io::Result<()> {
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || !(1..=links).contains(&metadata.nlink())
        || metadata.len() > MAX_FILE_BYTES
    {
        return Err(invalid(
            "service file is not a bounded private owned regular file",
        ));
    }
    Ok(())
}

pub(crate) fn existing(path: &Path, uid: u32) -> io::Result<Option<(Vec<u8>, Metadata)>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    private_file(&metadata, uid, 2)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("service file grew beyond the read budget"));
    }
    Ok(Some((bytes, metadata)))
}

fn directory(path: &Path, uid: u32, private: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let forbidden = if private { 0o077 } else { 0o022 };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & forbidden != 0 {
        return Err(invalid(
            "service directory is linked, unowned or has unsafe permissions",
        ));
    }
    Ok(())
}

fn ensure_directories(
    home: &Path,
    relative: &Path,
    uid: u32,
    private: bool,
) -> io::Result<PathBuf> {
    directory(home, uid, false)?;
    let mut path = home.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid(
                "service directory must stay under the reviewed home",
            ));
        };
        path.push(name);
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        directory(&path, uid, false)?;
    }
    directory(&path, uid, private)?;
    Ok(path)
}

/// Publish without replacing an existing name. The fixed scratch file is
/// serialized by the persistent installer lock. A crash after hard-linking but
/// before unlinking scratch is adopted only with exact bytes and inode proof.
fn publish(path: &Path, bytes: &[u8], root: &Path, uid: u32) -> io::Result<bool> {
    publish_with_hook(path, bytes, root, uid, || Ok(()))
}

fn publish_with_hook(
    path: &Path,
    bytes: &[u8],
    root: &Path,
    uid: u32,
    after_link: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("service content exceeds the file budget"));
    }
    let scratch = root.join(SCRATCH);
    if let Some((old, metadata)) = existing(path, uid)? {
        if old != bytes {
            return Err(invalid("service file differs; refusing to overwrite it"));
        }
        if metadata.nlink() == 2 {
            let (pending, proof) = existing(&scratch, uid)?
                .ok_or_else(|| invalid("service hard link has no scratch recovery proof"))?;
            if proof.nlink() != 2
                || proof.dev() != metadata.dev()
                || proof.ino() != metadata.ino()
                || pending != bytes
            {
                return Err(invalid(
                    "service hard link does not match the exact scratch proof",
                ));
            }
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            fs::remove_file(&scratch)?;
            File::open(root)?.sync_all()?;
        }
        return Ok(false);
    }
    let mut pending = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&scratch)?;
    private_file(&pending.metadata()?, uid, 1)?;
    pending.set_len(0)?;
    pending.rewind()?;
    pending.write_all(bytes)?;
    pending.sync_all()?;
    // hard_link refuses an existing destination, including a symlink. Never
    // replace a differing unit even if another actor created it after our read.
    fs::hard_link(&scratch, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    after_link()?;
    fs::remove_file(&scratch)?;
    File::open(root)?.sync_all()?;
    Ok(true)
}

pub(crate) fn claim_installer(
    root: &Path,
    uid: u32,
) -> io::Result<crate::file_lock::ExclusiveFileLock> {
    let lock_path = root.join(".install.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&lock_path)?;
    let identity = file.metadata()?;
    private_file(&identity, uid, 1)?;
    if identity.len() != 0 {
        return Err(invalid("service installer lock must be empty"));
    }
    let claim = crate::file_lock::ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "another service installer is active",
        )
    })?;
    let named = fs::symlink_metadata(&lock_path)?;
    if named.dev() != identity.dev() || named.ino() != identity.ino() {
        return Err(invalid("service installer lock identity changed"));
    }
    Ok(claim)
}

/// Publish an exact list of bounded service files, then the unit last. The
/// caller owns the purpose-specific plan validation; this shared path performs
/// only secure, no-replace installation and never activates the service.
pub(crate) fn install_exact_files(
    home: &Path,
    directory: &Path,
    unit_path: &Path,
    uid: u32,
    files: &[(&str, &[u8])],
    unit_bytes: &[u8],
) -> io::Result<Vec<PathBuf>> {
    let relative = directory
        .strip_prefix(home)
        .map_err(|_| invalid("service helpers are outside the reviewed home"))?;
    for (name, _) in files {
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(invalid("service file names must be single path components"));
        }
    }
    let root = ensure_directories(home, relative, uid, true)?;
    let _claim = claim_installer(&root, uid)?;
    let unit_parent = unit_path
        .parent()
        .ok_or_else(|| invalid("service unit has no parent"))?;
    ensure_directories(
        home,
        unit_parent
            .strip_prefix(home)
            .map_err(|_| invalid("service unit is outside the reviewed home"))?,
        uid,
        false,
    )?;
    let mut created = Vec::new();
    for (name, content) in files {
        let path = root.join(name);
        if publish(&path, content, &root, uid)? {
            created.push(path);
        }
    }
    // Unit publication is last. Installation never invokes launchctl.
    if publish(unit_path, unit_bytes, &root, uid)? {
        created.push(unit_path.to_path_buf());
    }
    Ok(created)
}

pub(crate) fn install(plan: &Plan, confirmation: &str) -> io::Result<Vec<PathBuf>> {
    if confirmation != plan.digest {
        return Err(invalid(
            "service installation requires the exact plan digest",
        ));
    }
    // SAFETY: geteuid has no pointer arguments and only reads this process uid.
    if plan.uid != unsafe { libc::geteuid() } {
        return Err(invalid("service plan must belong to the current user"));
    }
    let expected = Plan::new(
        &plan.repository,
        &plan.repository_uuid,
        &plan.home,
        &plan.broker,
        &plan.python,
        plan.interval_seconds,
        plan.uid,
    )?;
    let encoded = serde_json::to_vec(plan).map_err(io::Error::other)?;
    if encoded != serde_json::to_vec(&expected).map_err(io::Error::other)? {
        return Err(invalid(
            "service plan changed; recompute and review it before installing",
        ));
    }
    let mut files = script_contents()
        .into_iter()
        .map(|(name, content)| (name, content.as_bytes()))
        .collect::<Vec<_>>();
    files.push(("plan.json", encoded.as_slice()));
    install_exact_files(
        &plan.home,
        &plan.directory,
        &plan.unit_path,
        plan.uid,
        &files,
        plan.unit.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    pub(super) fn fixture() -> (tempfile::TempDir, Plan) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("private home");
        let repo = root.path().join("repo '& < >");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&repo).unwrap();
        let binary = root.path().join("synthetic broker");
        fs::write(&binary, b"fixture executable").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        // SAFETY: geteuid takes no pointers and only observes this process uid.
        let uid = unsafe { libc::geteuid() };
        let plan = Plan::new(
            &repo,
            "12345678-1234-4123-8123-123456789abc",
            &home,
            &binary,
            &binary,
            300,
            uid,
        )
        .unwrap();
        (root, plan)
    }

    #[test]
    fn confirmation_and_recomputed_identity_precede_any_filesystem_write() {
        let (_root, mut plan) = fixture();
        assert!(install(&plan, "wrong").is_err());
        assert!(!plan.directory.exists());
        plan.unit.push_str("forged unit");
        assert!(install(&plan, &plan.digest).is_err());
        assert!(!plan.directory.exists());
    }

    #[test]
    fn install_is_idempotent_and_never_executes_the_fixture_binary() {
        let (_root, plan) = fixture();
        let created = install(&plan, &plan.digest).unwrap();
        assert_eq!(created.len(), 4);
        assert_eq!(fs::read_to_string(&plan.unit_path).unwrap(), plan.unit);
        assert!(plan.unit.contains("<key>RunAtLoad</key><false/>"));
        for (name, content) in script_contents() {
            let path = plan.directory.join(name);
            assert_eq!(fs::read_to_string(&path).unwrap(), content);
            assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o600);
        }
        assert!(install(&plan, &plan.digest).unwrap().is_empty());
        assert!(!plan.directory.join(SCRATCH).exists());
        assert_eq!(plan.directory.read_dir().unwrap().count(), 4);
    }

    #[test]
    fn differing_unit_and_helper_files_are_preserved() {
        let (_root, plan) = fixture();
        install(&plan, &plan.digest).unwrap();
        fs::write(&plan.unit_path, b"operator unit").unwrap();
        assert!(install(&plan, &plan.digest).is_err());
        assert_eq!(fs::read(&plan.unit_path).unwrap(), b"operator unit");
        let helper = plan.directory.join("aethyme-pr-monitor.py");
        fs::write(&helper, b"operator helper").unwrap();
        assert!(install(&plan, &plan.digest).is_err());
        assert_eq!(fs::read(helper).unwrap(), b"operator helper");
    }

    #[test]
    fn linked_parent_directory_is_refused_without_touching_its_target() {
        let (root, plan) = fixture();
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, plan.home.join("Library")).unwrap();
        assert!(install(&plan, &plan.digest).is_err());
        assert_eq!(elsewhere.read_dir().unwrap().count(), 0);
    }

    #[test]
    fn exact_two_link_publication_is_recovered_and_unknown_links_are_refused() {
        let (_root, plan) = fixture();
        let root = ensure_directories(
            &plan.home,
            plan.directory.strip_prefix(&plan.home).unwrap(),
            plan.uid,
            true,
        )
        .unwrap();
        let scratch = root.join(SCRATCH);
        let target = root.join("fixture");
        let error = publish_with_hook(&target, b"expected", &root, plan.uid, || {
            Err(invalid("simulated interruption after durable link"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("simulated interruption"));
        assert_eq!(target.metadata().unwrap().nlink(), 2);
        assert_eq!(fs::read(&scratch).unwrap(), b"expected");
        assert!(!publish(&target, b"expected", &root, plan.uid).unwrap());
        assert!(!scratch.exists());
        assert_eq!(target.metadata().unwrap().nlink(), 1);
        fs::hard_link(&target, root.join("foreign-link")).unwrap();
        assert!(publish(&target, b"expected", &root, plan.uid).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"expected");
    }

    #[test]
    fn scratch_links_and_active_installers_are_refused_without_truncation() {
        let (_root, plan) = fixture();
        install(&plan, &plan.digest).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(plan.directory.join(".install.lock"))
            .unwrap();
        let claim = crate::file_lock::ExclusiveFileLock::try_acquire(lock)
            .unwrap()
            .unwrap();
        assert_eq!(
            install(&plan, &plan.digest).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(claim);
        let sentinel = plan.directory.join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600)).unwrap();
        let scratch = plan.directory.join(SCRATCH);
        symlink(&sentinel, &scratch).unwrap();
        assert!(
            publish(
                &plan.directory.join("missing"),
                b"replacement",
                &plan.directory,
                plan.uid
            )
            .is_err()
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        fs::remove_file(scratch).unwrap();
        fs::hard_link(&sentinel, plan.directory.join(SCRATCH)).unwrap();
        assert!(
            publish(
                &plan.directory.join("missing"),
                b"replacement",
                &plan.directory,
                plan.uid
            )
            .is_err()
        );
        assert_eq!(fs::read(sentinel).unwrap(), b"untouched");
    }
}
