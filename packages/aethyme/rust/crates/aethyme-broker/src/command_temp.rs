//! A temp directory per command the broker runs for a session.
//!
//! A repository's pre-push hook, gate, install or exec command makes its
//! scratch with `mktemp` under `$TMPDIR`. Inherited unchanged, that is the
//! system temp folder, and whatever an interrupted (timed out, killed) or
//! careless command leaves there belongs to nothing and stays forever. So
//! every such command gets `TMPDIR`, `TMP` and `TEMP` pointing at a fresh
//! directory the broker created for that one run:
//!
//! `<session scratch>/tmp/r<ms>-<pid>-<n>`
//!
//! inside the session scratch directory ([`crate::session_scratch`]), which
//! is on the same volume as the session's checkout (so a tool that makes a
//! file under `$TMPDIR` and renames it into the checkout still renames, never
//! copies).
//!
//! When the command ends, whatever way it ends (the guard is dropped after
//! the supervision that reaps the command), an empty run directory is
//! removed with one `rmdir`, and a non-empty one is renamed, O(1) and on the
//! same volume, into `<session scratch>/tmp-done/`. The checkout retirement
//! run moves finished runs from there into the pool's checkout trash, after
//! its open-file observation, and the paced host deleter unlinks them later.
//! Nothing here unlinks a file. A run the broker never got to finish (the
//! broker itself was killed) stays inside the session scratch, which goes
//! with the session: that is the backstop.
//!
//! Unix socket paths are limited to 104 bytes on macOS (108 on Linux), and a
//! session scratch path on the busiest machine is about 130 bytes. A tool
//! that creates a socket under `$TMPDIR` (SSH and GPG agents, Python
//! multiprocessing, tmux) would fail there. When the run directory's path is
//! longer than [`TMPDIR_PATH_MAX`], the command gets a short alias instead: a
//! symbolic link `<inherited temp dir>/ae<uid>/<pid><n>` to the run
//! directory, in a private directory owned by this user. The kernel follows
//! the link when a socket is bound, so only the alias's length counts, and
//! every byte written still lands on the scratch volume. The link is removed
//! with its run (only after checking it is still a link to that run); links
//! left by a killed broker are pruned once their target is gone. When even
//! the alias would be too long, the command keeps the inherited temp folder
//! (counter `command_temp_path_too_long`).
//!
//! A command started inside such a run (a gate in the pre-push hook a push
//! started) inherits the run's directory rather than nesting another one.
//!
//! A repository whose tools need the system temp folder opts out in
//! `.aethyme/config.toml`:
//!
//! ```toml
//! [command_temp]
//! enabled = false
//! ```

use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The variables pointed at the run directory.
pub(crate) const TEMP_VARIABLES: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];
/// Names the run directory a command was given, so a command it starts
/// inherits the run instead of nesting one.
pub(crate) const COMMAND_TEMP_ENV: &str = "AETHYME_COMMAND_TEMP_DIR";
/// Live runs, inside the session scratch directory.
pub(crate) const LIVE_CONTAINER: &str = "tmp";
/// Finished runs waiting for the retirement run, inside the session scratch.
pub(crate) const DONE_CONTAINER: &str = "tmp-done";
/// The longest temp path handed to a command: 40 of the 104 bytes a macOS
/// socket path may hold stay free for the names tools add under it.
pub(crate) const TMPDIR_PATH_MAX: usize = 64;
/// Dangling alias links pruned per new alias, at most.
const ALIAS_PRUNE_MAX: usize = 32;
/// Attempts at a fresh name when one is taken.
const CREATE_ATTEMPTS: u32 = 8;
/// Longest accepted run name.
const RUN_NAME_MAX: usize = 48;

static RUN_NONCE: AtomicU64 = AtomicU64::new(0);

/// Whether the repository of each session resolved in this process opted
/// out, for commands spawned where only the session id is at hand (gates).
static ENABLED_FOR_COMMANDS: Mutex<BTreeMap<i64, bool>> = Mutex::new(BTreeMap::new());

/// `[command_temp]` in `.aethyme/config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CommandTempPolicy {
    /// Give commands the broker runs for a session their own temp directory
    /// (default true).
    pub(crate) enabled: bool,
}

impl Default for CommandTempPolicy {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl CommandTempPolicy {
    /// The repository's policy, under the usual configuration trust rule.
    /// An unreadable or invalid file keeps the default (counted).
    pub(crate) fn load(main_root: &Path) -> Self {
        let _span = crate::phase_telemetry::span("command_temp_policy");
        let loaded = crate::merge::repository_config_checked(main_root)
            .map_err(|error| error.to_string())
            .and_then(|config| match config {
                Some((text, _)) => Self::from_config_text(&text),
                None => Ok(Self::default()),
            });
        loaded.unwrap_or_else(|_| {
            crate::phase_telemetry::add("command_temp_policy_invalid", Duration::ZERO);
            Self::default()
        })
    }

    fn from_config_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(section) = value.get("command_temp") else {
            return Ok(Self::default());
        };
        section
            .clone()
            .try_into()
            .map_err(|error| format!("[command_temp] is invalid: {error}"))
    }
}

/// Remember whether `session_id`'s repository wants command temp
/// directories, for commands this process spawns.
pub(crate) fn remember_enabled(session_id: i64, enabled: bool) {
    ENABLED_FOR_COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id, enabled);
}

/// Whether this process resolved the policy for `session_id` and it is on.
pub(crate) fn remembered_enabled(session_id: i64) -> bool {
    ENABLED_FOR_COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session_id)
        .copied()
        .unwrap_or(false)
}

/// Whether `name` is a run directory name the broker writes:
/// `r<hex>-<hex>-<hex>`.
pub(crate) fn is_run_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('r') else {
        return false;
    };
    let parts = rest.split('-').collect::<Vec<_>>();
    name.len() <= RUN_NAME_MAX
        && parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Whether `name` is an alias link name the broker writes: `<hex>x<hex>`.
fn is_alias_name(name: &str) -> bool {
    name.len() <= RUN_NAME_MAX
        && name.split_once('x').is_some_and(|(pid, nonce)| {
            [pid, nonce].iter().all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
        })
}

fn uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() }
}

/// Create `path` as a private directory when absent, then require it to be
/// a real directory (not a link) owned by this user and closed to others.
fn private_directory(path: &Path) -> Result<(), String> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot create {}: {error}", path.display())),
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is not a private directory owned by this user",
            path.display()
        ));
    }
    Ok(())
}

/// Why a command keeps the inherited temp folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// It runs inside another run, whose directory it inherits.
    Inherited,
    /// Neither the run directory nor an alias fits [`TMPDIR_PATH_MAX`].
    PathTooLong,
    /// A directory or link could not be made.
    Failed,
}

impl Unavailable {
    fn counter(&self) -> &'static str {
        match self {
            Self::Inherited => "command_temp_inherited",
            Self::PathTooLong => "command_temp_path_too_long",
            Self::Failed => "command_temp_create_failed",
        }
    }
}

/// One command's temp directory. Dropping it finishes the run.
#[derive(Debug)]
pub(crate) struct CommandTemp {
    scratch: PathBuf,
    name: String,
    device: u64,
    inode: u64,
    alias: Option<PathBuf>,
}

impl CommandTemp {
    /// A fresh run directory under the session scratch `scratch`, or `None`
    /// (counted) when the command keeps the inherited temp folder.
    pub(crate) fn create(scratch: &Path) -> Option<Self> {
        let inherited = runs_inside(
            scratch,
            std::env::var_os(COMMAND_TEMP_ENV).as_deref(),
            std::env::var_os("TMPDIR").as_deref(),
        );
        Self::create_in(scratch, &std::env::temp_dir(), inherited)
            .inspect_err(|unavailable| {
                crate::phase_telemetry::add(unavailable.counter(), Duration::ZERO);
            })
            .ok()
    }

    /// [`Self::create`] with the alias base and the inheritance named.
    pub(crate) fn create_in(
        scratch: &Path,
        alias_base: &Path,
        inherited: bool,
    ) -> Result<Self, Unavailable> {
        let _span = crate::phase_telemetry::span("command_temp_create");
        if inherited {
            return Err(Unavailable::Inherited);
        }
        let live = scratch.join(LIVE_CONTAINER);
        private_directory(&live).map_err(|_| Unavailable::Failed)?;
        let pid = std::process::id();
        let created_ms = u64::try_from(crate::clock::epoch_ms().max(0)).unwrap_or(0);
        let mut made = None;
        for _ in 0..CREATE_ATTEMPTS {
            let nonce = RUN_NONCE.fetch_add(1, Ordering::Relaxed);
            let name = format!("r{created_ms:x}-{pid:x}-{nonce:x}");
            match std::fs::DirBuilder::new()
                .mode(0o700)
                .create(live.join(&name))
            {
                Ok(()) => {
                    made = Some(name);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(Unavailable::Failed),
            }
        }
        let name = made.ok_or(Unavailable::Failed)?;
        let run = live.join(&name);
        let metadata = std::fs::symlink_metadata(&run).map_err(|_| Unavailable::Failed)?;
        let mut temp = Self {
            scratch: scratch.to_path_buf(),
            name,
            device: metadata.dev(),
            inode: metadata.ino(),
            alias: None,
        };
        if run.as_os_str().len() > TMPDIR_PATH_MAX {
            // On failure `temp` is dropped, which removes the empty run.
            temp.alias = Some(make_alias(alias_base, &run)?);
            crate::phase_telemetry::add("command_temp_alias_created", Duration::ZERO);
        }
        crate::phase_telemetry::add("command_temp_created", Duration::ZERO);
        Ok(temp)
    }

    /// The run directory itself.
    pub(crate) fn run_dir(&self) -> PathBuf {
        self.scratch.join(LIVE_CONTAINER).join(&self.name)
    }

    /// The path the command's temp variables name: the alias when the run
    /// directory's own path is too long for sockets.
    pub(crate) fn tmpdir(&self) -> PathBuf {
        self.alias.clone().unwrap_or_else(|| self.run_dir())
    }

    /// The variables to set on the command.
    pub(crate) fn environment(&self) -> Vec<(String, String)> {
        let tmpdir = self.tmpdir().to_string_lossy().into_owned();
        TEMP_VARIABLES
            .iter()
            .chain(std::iter::once(&COMMAND_TEMP_ENV))
            .map(|key| ((*key).to_owned(), tmpdir.clone()))
            .collect()
    }

    /// Set the variables on `command`.
    pub(crate) fn apply(&self, command: &mut std::process::Command) {
        for (key, value) in self.environment() {
            command.env(key, value);
        }
    }

    /// End the run: remove the alias link (when it is still a link to this
    /// run), then `rmdir` an empty run directory, or rename a non-empty one
    /// into the done container for the retirement run. Only the directory
    /// this guard created (same device and inode) is touched.
    fn finish(&mut self) {
        let _span = crate::phase_telemetry::span("command_temp_finish");
        let run = self.run_dir();
        if let Some(alias) = self.alias.take() {
            let still_ours = std::fs::symlink_metadata(&alias)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                && std::fs::read_link(&alias).is_ok_and(|target| target == run);
            if still_ours && std::fs::remove_file(&alias).is_ok() {
                crate::phase_telemetry::add("command_temp_alias_removed", Duration::ZERO);
            }
        }
        let same = std::fs::symlink_metadata(&run).is_ok_and(|metadata| {
            metadata.is_dir() && (metadata.dev(), metadata.ino()) == (self.device, self.inode)
        });
        if !same {
            crate::phase_telemetry::add("command_temp_kept_changed", Duration::ZERO);
            return;
        }
        // `rmdir` removes only an empty directory: nothing is unlinked.
        if std::fs::remove_dir(&run).is_ok() {
            crate::phase_telemetry::add("command_temp_removed_empty", Duration::ZERO);
            return;
        }
        let done = self.scratch.join(DONE_CONTAINER);
        let moved = private_directory(&done)
            .and_then(|()| std::fs::rename(&run, done.join(&self.name)).map_err(|e| e.to_string()));
        match moved {
            Ok(()) => crate::phase_telemetry::add("command_temp_finished", Duration::ZERO),
            // Left in place: the session's own retirement removes it.
            Err(_) => crate::phase_telemetry::add("command_temp_finish_failed", Duration::ZERO),
        }
    }
}

impl Drop for CommandTemp {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Whether this process runs inside a run of the session scratch
/// `scratch`: it was given a command temp directory (`marker`), it is still
/// its `TMPDIR`, and it resolves into this scratch's live container. A run
/// of another session, or a stale marker, does not count.
fn runs_inside(
    scratch: &Path,
    marker: Option<&std::ffi::OsStr>,
    tmpdir: Option<&std::ffi::OsStr>,
) -> bool {
    let (Some(marker), Some(tmpdir)) = (marker, tmpdir) else {
        return false;
    };
    if marker != tmpdir {
        return false;
    }
    let live = std::fs::canonicalize(scratch.join(LIVE_CONTAINER));
    let run = std::fs::canonicalize(tmpdir);
    matches!((live, run), (Ok(live), Ok(run)) if run.parent() == Some(live.as_path()))
}

/// A short link to `run` in `<alias_base>/ae<uid>/`, pruning dangling links
/// of the broker's naming first.
fn make_alias(alias_base: &Path, run: &Path) -> Result<PathBuf, Unavailable> {
    let container = alias_base.join(format!("ae{}", uid()));
    let longest = container
        .join(format!("{:x}x{:x}", u32::MAX, u64::from(u32::MAX)))
        .as_os_str()
        .len();
    if longest > TMPDIR_PATH_MAX {
        return Err(Unavailable::PathTooLong);
    }
    private_directory(&container).map_err(|_| Unavailable::Failed)?;
    prune_dangling_aliases(&container);
    let pid = std::process::id();
    for _ in 0..CREATE_ATTEMPTS {
        let nonce = RUN_NONCE.fetch_add(1, Ordering::Relaxed) & u64::from(u32::MAX);
        let alias = container.join(format!("{pid:x}x{nonce:x}"));
        match std::os::unix::fs::symlink(run, &alias) {
            Ok(()) => return Ok(alias),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(Unavailable::Failed),
        }
    }
    Err(Unavailable::Failed)
}

/// Remove links of the broker's naming in the private alias container whose
/// target no longer exists (their run ended in a broker that was killed).
/// Never a directory or a regular file, never a link that still resolves.
fn prune_dangling_aliases(container: &Path) {
    let Ok(listing) = std::fs::read_dir(container) else {
        return;
    };
    for entry in listing.flatten().take(ALIAS_PRUNE_MAX * 4) {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let path = entry.path();
        let dangling = is_alias_name(&name)
            && entry.file_type().is_ok_and(|kind| kind.is_symlink())
            && std::fs::metadata(&path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        if dangling && std::fs::remove_file(&path).is_ok() {
            crate::phase_telemetry::add("command_temp_alias_pruned", Duration::ZERO);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let owner = tempfile::tempdir().unwrap();
        let scratch = std::fs::canonicalize(owner.path()).unwrap().join("scratch");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .unwrap();
        (owner, scratch)
    }

    /// A short alias base, as the system temp folder is.
    fn short_base() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ct")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn env_of(temp: &CommandTemp) -> BTreeMap<String, String> {
        temp.environment().into_iter().collect()
    }

    #[test]
    fn policy_defaults_on_and_refuses_unknown_keys() {
        assert!(CommandTempPolicy::from_config_text("").unwrap().enabled);
        assert!(
            !CommandTempPolicy::from_config_text("[command_temp]\nenabled = false\n")
                .unwrap()
                .enabled
        );
        assert!(CommandTempPolicy::from_config_text("[command_temp]\nenable = false\n").is_err());
    }

    #[test]
    fn run_and_alias_names_are_the_brokers_only() {
        assert!(is_run_name("r19a2b-4f2-0"));
        assert!(!is_run_name("r19a2b-4f2"));
        assert!(!is_run_name("r19a2b-4f2-0/.."));
        assert!(!is_run_name("tmp.XYZ"));
        assert!(is_alias_name("4f2x1a"));
        assert!(!is_alias_name("ssh-agent"));
        assert!(!is_alias_name("x1"));
    }

    /// The child sees the run directory (or its alias) in every temp
    /// variable, and what it writes there is on the scratch's volume.
    #[test]
    fn a_child_gets_the_run_directory_as_its_temp_folder() {
        let (_owner, scratch) = scratch();
        let base = short_base();
        let temp = CommandTemp::create_in(&scratch, base.path(), false).unwrap();
        let environment = env_of(&temp);
        let tmpdir = temp.tmpdir().to_string_lossy().into_owned();
        for key in TEMP_VARIABLES {
            assert_eq!(environment.get(key), Some(&tmpdir), "{key}");
        }
        assert_eq!(environment.get(COMMAND_TEMP_ENV), Some(&tmpdir));
        assert!(tmpdir.len() <= TMPDIR_PATH_MAX, "{tmpdir}");
        let mut child = std::process::Command::new("sh");
        child
            .arg("-c")
            .arg("f=$(mktemp \"$TMPDIR/leftover.XXXXXX\") && printf %s \"$f\"");
        temp.apply(&mut child);
        let output = child.output().unwrap();
        assert!(output.status.success());
        let made = PathBuf::from(String::from_utf8(output.stdout).unwrap());
        let name = made.file_name().unwrap();
        assert!(temp.run_dir().join(name).is_file(), "written in the run");
    }

    /// A run directory whose path is too long for sockets is handed out
    /// through a short link, removed with the run.
    #[test]
    fn a_long_run_path_gets_a_short_alias() {
        let (_owner, scratch) = scratch();
        let deep = scratch.join("a-long-directory-name-that-pushes-the-path-past-the-limit");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&deep)
            .unwrap();
        let base = short_base();
        let temp = CommandTemp::create_in(&deep, base.path(), false).unwrap();
        assert!(temp.run_dir().as_os_str().len() > TMPDIR_PATH_MAX);
        let alias = temp.tmpdir();
        assert!(alias.starts_with(base.path()));
        assert!(alias.as_os_str().len() <= TMPDIR_PATH_MAX);
        assert_eq!(std::fs::read_link(&alias).unwrap(), temp.run_dir());
        let run = temp.run_dir();
        drop(temp);
        assert!(std::fs::symlink_metadata(&alias).is_err(), "alias removed");
        assert!(!run.exists(), "empty run removed");
    }

    /// When even the alias would be too long, the command keeps the
    /// inherited temp folder and nothing is left behind.
    #[test]
    fn a_too_long_alias_base_falls_back_to_the_inherited_folder() {
        let (_owner, scratch) = scratch();
        let deep = scratch.join("a-long-directory-name-that-pushes-the-path-past-the-limit");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&deep)
            .unwrap();
        let long_base = deep.clone();
        assert_eq!(
            CommandTemp::create_in(&deep, &long_base, false).err(),
            Some(Unavailable::PathTooLong)
        );
        let live = deep.join(LIVE_CONTAINER);
        assert_eq!(std::fs::read_dir(&live).unwrap().count(), 0, "run removed");
    }

    /// Only a marker that is still `TMPDIR` and names a live run of this
    /// same scratch makes a command inherit.
    #[test]
    fn only_a_run_of_the_same_scratch_is_inherited() {
        let (_owner, scratch) = scratch();
        let (_other_owner, other) = self::scratch();
        let base = short_base();
        let outer = CommandTemp::create_in(&scratch, base.path(), false).unwrap();
        let tmpdir = outer.tmpdir();
        let given = Some(tmpdir.as_os_str());
        assert!(runs_inside(&scratch, given, given));
        assert!(!runs_inside(&other, given, given), "another session's run");
        assert!(!runs_inside(&scratch, given, Some("/tmp".as_ref())));
        assert!(!runs_inside(&scratch, None, given));
    }

    #[test]
    fn a_command_inside_a_run_inherits_it() {
        let (_owner, scratch) = scratch();
        assert_eq!(
            CommandTemp::create_in(&scratch, Path::new("/tmp"), true).err(),
            Some(Unavailable::Inherited)
        );
        assert!(!scratch.join(LIVE_CONTAINER).exists());
    }

    /// Success, failure and timeout all end the same way: the guard drops
    /// after the command, an empty run goes with one rmdir, and a non-empty
    /// one is moved, whole, to the done container for the paced removal.
    #[test]
    fn a_finished_run_is_removed_when_empty_and_handed_on_otherwise() {
        let (_owner, scratch) = scratch();
        let base = short_base();
        for script in ["exit 0", "exit 3", "sleep 5"] {
            let empty = CommandTemp::create_in(&scratch, base.path(), false).unwrap();
            let empty_run = empty.run_dir();
            let full = CommandTemp::create_in(&scratch, base.path(), false).unwrap();
            let full_run = full.run_dir();
            let full_name = full_run.file_name().unwrap().to_owned();
            let mut child = std::process::Command::new("sh");
            child.arg("-c").arg(format!(
                "mktemp -d \"$TMPDIR/build.XXXXXX\" >/dev/null; {script}"
            ));
            full.apply(&mut child);
            let mut child = child.spawn().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    // The timeout path: kill and reap, then drop.
                    child.kill().unwrap();
                    child.wait().unwrap();
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(empty);
            drop(full);
            assert!(!empty_run.exists(), "{script}: empty run removed");
            assert!(!full_run.exists(), "{script}: run left the live container");
            let done = scratch.join(DONE_CONTAINER).join(&full_name);
            assert!(done.is_dir(), "{script}: handed to the done container");
            assert_eq!(std::fs::read_dir(&done).unwrap().count(), 1, "{script}");
        }
    }

    /// A directory that is not the one the guard created is never touched.
    #[test]
    fn a_swapped_run_directory_is_left_alone() {
        let (_owner, scratch) = scratch();
        let temp = CommandTemp::create_in(&scratch, Path::new("/tmp"), false).unwrap();
        let run = temp.run_dir();
        let replacement = scratch.join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("keep.txt"), b"not the broker's").unwrap();
        std::fs::remove_dir(&run).unwrap();
        std::fs::rename(&replacement, &run).unwrap();
        drop(temp);
        assert!(run.join("keep.txt").is_file());
        assert!(!scratch.join(DONE_CONTAINER).exists());
    }

    /// Only dangling links of the broker's naming are pruned.
    #[test]
    fn only_dangling_broker_aliases_are_pruned() {
        let base = short_base();
        let container = base.path().join(format!("ae{}", uid()));
        private_directory(&container).unwrap();
        let live_target = base.path().join("live");
        std::fs::create_dir(&live_target).unwrap();
        std::os::unix::fs::symlink(base.path().join("gone"), container.join("1x1")).unwrap();
        std::os::unix::fs::symlink(&live_target, container.join("1x2")).unwrap();
        std::os::unix::fs::symlink(base.path().join("gone"), container.join("other")).unwrap();
        std::fs::write(container.join("2x3"), b"a file").unwrap();
        prune_dangling_aliases(&container);
        assert!(std::fs::symlink_metadata(container.join("1x1")).is_err());
        assert!(std::fs::symlink_metadata(container.join("1x2")).is_ok());
        assert!(std::fs::symlink_metadata(container.join("other")).is_ok());
        assert!(container.join("2x3").is_file());
    }

    #[test]
    fn a_foreign_alias_container_is_refused() {
        let base = short_base();
        let container = base.path().join(format!("ae{}", uid()));
        std::fs::create_dir(&container).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            make_alias(base.path(), Path::new("/nonexistent")),
            Err(Unavailable::Failed)
        ));
    }
}
