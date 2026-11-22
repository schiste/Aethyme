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
//! removed with one `rmdir`, together with its alias. A non-empty one stays
//! where it is, alias included, so a process the command left behind keeps
//! its `$TMPDIR`; the broker only writes a finished-run marker for it in the
//! broker-owned sibling container `<session scratch>/tmp-done/` (never
//! inside the run, where the command could forge or delete it). The checkout
//! retirement run moves marked runs into the pool's checkout trash after its
//! open-file observation, removes their alias then, and the paced host
//! deleter unlinks them later. Nothing here unlinks a file of a run. A run
//! the broker never got to finish (the broker itself was killed) stays
//! inside the session scratch, which goes with the session: that is the
//! backstop.
//!
//! Unix socket paths are limited to 104 bytes on macOS (108 on Linux), and a
//! session scratch path on the busiest machine is about 130 bytes. A tool
//! that creates a socket under `$TMPDIR` (SSH and GPG agents, Python
//! multiprocessing, tmux) would fail there. When the run directory's path is
//! longer than [`TMPDIR_PATH_MAX`], the command gets a short alias instead: a
//! symbolic link `/tmp/ae<uid>/<6 hex>` to the run directory, in a private
//! directory owned by this user (the inherited temp folder is the fallback
//! base when that one is unusable). The kernel follows the link when a socket
//! is bound, so only the alias's length counts, and every byte written still
//! lands on the scratch volume. Links left by a killed broker are pruned once
//! their target is gone. When no alias fits, the command keeps the inherited
//! temp folder (counter `command_temp_path_too_long`).
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
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The variables pointed at the run directory.
pub(crate) const TEMP_VARIABLES: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];
/// Names the run directory a command was given, so a command it starts
/// inherits the run instead of nesting one.
pub(crate) const COMMAND_TEMP_ENV: &str = "AETHYME_COMMAND_TEMP_DIR";
/// Live and finished runs, inside the session scratch directory.
pub(crate) const LIVE_CONTAINER: &str = "tmp";
/// Finished-run markers, inside the session scratch directory.
pub(crate) const DONE_CONTAINER: &str = "tmp-done";
/// The longest temp path handed to a command: 40 of the 104 bytes a macOS
/// socket path may hold stay free for the names tools add under it.
pub(crate) const TMPDIR_PATH_MAX: usize = 64;
/// The preferred alias base: short and the same on every Unix host.
const ALIAS_ROOT: &str = "/tmp";
/// Alias names are this many lowercase hex digits.
const ALIAS_NAME_LEN: usize = 6;
/// Dangling alias links examined per new alias, at most.
const ALIAS_PRUNE_SCAN_MAX: usize = 128;
/// Attempts at a fresh name when one is taken.
const CREATE_ATTEMPTS: u32 = 8;
/// Longest accepted run name.
const RUN_NAME_MAX: usize = 48;
/// Largest marker read back.
pub(crate) const MARKER_MAX_BYTES: usize = 4096;
/// Finished runs of one session at or above which the retirement run is
/// requested instead of waiting for its periodic pass.
pub(crate) const RETIREMENT_REQUEST_RUNS: usize = 16;
/// Bytes of finished runs of one session at or above which the retirement
/// run is requested.
pub(crate) const RETIREMENT_REQUEST_BYTES: u64 = 1024 * 1024 * 1024;
/// How long finishing a run may spend measuring it.
const FINISH_MEASURE_BUDGET: Duration = Duration::from_millis(100);

static RUN_NONCE: AtomicU64 = AtomicU64::new(0);
/// A run finished in this process past a threshold: the next broker step
/// that can, requests the retirement run.
static RETIREMENT_WANTED: AtomicBool = AtomicBool::new(false);

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

/// Whether `name` is an alias link name the broker writes.
fn is_alias_name(name: &str) -> bool {
    name.len() == ALIAS_NAME_LEN
        && name
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether a run finished past a threshold since the last call.
pub(crate) fn take_retirement_wanted() -> bool {
    RETIREMENT_WANTED.swap(false, Ordering::AcqRel)
}

/// Whether `headroom` (free bytes and inodes on the scratch's volume) is
/// below what a gate needs to start, by the classifier the storage sweeps
/// use. Unknown headroom is not pressure.
pub(crate) fn pressured(headroom: Option<crate::disk_headroom::DiskHeadroom>) -> bool {
    pressured_counts(
        headroom.map(|headroom| headroom.bytes),
        headroom.and_then(|headroom| headroom.inodes),
    )
}

/// [`pressured`] from counts already read (a gate's own reading).
pub(crate) fn pressured_counts(bytes: Option<u64>, inodes: Option<u64>) -> bool {
    crate::disk_headroom::sweep_urgency_with_inodes(
        bytes,
        crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES,
        inodes,
        crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
    ) == crate::disk_headroom::SweepUrgency::Pressured
}

fn uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() }
}

/// Create `path` as a private directory when absent, then require it to be
/// a real directory (not a link) owned by this user and closed to others.
/// A refusal is its cause ([`crate::checkout_trash::errno_cause`], or
/// `not_private`). Two commands creating it at once both succeed: the one
/// that loses the `mkdir` sees `EEXIST` and checks the winner's directory.
fn private_directory(path: &Path) -> Result<(), &'static str> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(crate::checkout_trash::errno_cause(&error)),
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| crate::checkout_trash::errno_cause(&error))?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
        return Err("not_private");
    }
    Ok(())
}

/// What the broker writes about a finished, non-empty run, beside it in the
/// done container: the run's identity, its alias, and what finishing
/// measured.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FinishedMarker {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) alias: Option<PathBuf>,
    /// Bytes measured at finish (a lower bound when not `complete`).
    pub(crate) bytes: u64,
    pub(crate) complete: bool,
}

impl FinishedMarker {
    /// Whether this run alone is past the request threshold.
    pub(crate) fn large(&self) -> bool {
        !self.complete || self.bytes >= RETIREMENT_REQUEST_BYTES
    }
}

/// Remove `alias` when it is still a symbolic link whose target is exactly
/// `run`; anything else at that path is left alone.
pub(crate) fn remove_alias(alias: &Path, run: &Path) -> bool {
    let still_ours = std::fs::symlink_metadata(alias)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        && std::fs::read_link(alias).is_ok_and(|target| target == run);
    let removed = still_ours && std::fs::remove_file(alias).is_ok();
    if removed {
        crate::phase_telemetry::add("command_temp_alias_removed", Duration::ZERO);
    }
    removed
}

/// Why a command keeps the inherited temp folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// It runs inside another run, whose directory it inherits.
    Inherited,
    /// Neither the run directory nor an alias fits [`TMPDIR_PATH_MAX`].
    PathTooLong,
    /// A directory or link could not be made: which, and why.
    Failed(CreateFailure),
}

/// Where creating a run failed and why, counted under `<step>.<cause>` in
/// the `command_temp_create_failures` attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CreateFailure {
    /// `live_container`, `run_dir`, `inspect` or `alias`.
    pub(crate) step: &'static str,
    /// [`crate::checkout_trash::errno_cause`], `not_private` or `no_name`.
    pub(crate) cause: &'static str,
}

impl CreateFailure {
    fn io(step: &'static str, error: &std::io::Error) -> Self {
        Self {
            step,
            cause: crate::checkout_trash::errno_cause(error),
        }
    }

    pub(crate) fn key(self) -> String {
        format!("{}.{}", self.step, self.cause)
    }
}

impl Unavailable {
    fn counter(&self) -> &'static str {
        match self {
            Self::Inherited => "command_temp_inherited",
            Self::PathTooLong => "command_temp_path_too_long",
            Self::Failed(_) => "command_temp_create_failed",
        }
    }

    /// Count it, with the cause of a failure.
    fn record(&self) {
        crate::phase_telemetry::add(self.counter(), Duration::ZERO);
        if let Self::Failed(failure) = self {
            crate::phase_telemetry::add_keyed(
                "command_temp_create_failures",
                &failure.key(),
                Duration::ZERO,
            );
        }
    }
}

/// The alias bases, in order: `/tmp`, then the inherited temp folder.
pub(crate) fn alias_bases() -> Vec<PathBuf> {
    let mut bases = vec![PathBuf::from(ALIAS_ROOT)];
    let inherited = std::env::temp_dir();
    if inherited != bases[0] {
        bases.push(inherited);
    }
    bases
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
        Self::create_in(scratch, &alias_bases(), inherited)
            .inspect_err(Unavailable::record)
            .ok()
    }

    /// [`Self::create`] with the alias bases and the inheritance named.
    pub(crate) fn create_in(
        scratch: &Path,
        alias_bases: &[PathBuf],
        inherited: bool,
    ) -> Result<Self, Unavailable> {
        let _span = crate::phase_telemetry::span("command_temp_create");
        if inherited {
            return Err(Unavailable::Inherited);
        }
        let live = scratch.join(LIVE_CONTAINER);
        private_directory(&live).map_err(|cause| {
            Unavailable::Failed(CreateFailure {
                step: "live_container",
                cause,
            })
        })?;
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
                Err(error) => {
                    return Err(Unavailable::Failed(CreateFailure::io("run_dir", &error)));
                }
            }
        }
        let name = made.ok_or(Unavailable::Failed(CreateFailure {
            step: "run_dir",
            cause: "no_name",
        }))?;
        let run = live.join(&name);
        let metadata = std::fs::symlink_metadata(&run)
            .map_err(|error| Unavailable::Failed(CreateFailure::io("inspect", &error)))?;
        let mut temp = Self {
            scratch: scratch.to_path_buf(),
            name,
            device: metadata.dev(),
            inode: metadata.ino(),
            alias: None,
        };
        if run.as_os_str().len() > TMPDIR_PATH_MAX {
            // On failure `temp` is dropped, which removes the empty run.
            temp.alias = Some(make_alias(alias_bases, &run)?);
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

    /// End the run. Only the directory this guard created (same device and
    /// inode) is considered: when it is empty it goes with one `rmdir`, its
    /// alias too; otherwise it stays, alias included, and a marker in the
    /// done container hands it to the retirement run.
    fn finish(&mut self) {
        let _span = crate::phase_telemetry::span("command_temp_finish");
        let run = self.run_dir();
        let same = std::fs::symlink_metadata(&run).is_ok_and(|metadata| {
            metadata.is_dir() && (metadata.dev(), metadata.ino()) == (self.device, self.inode)
        });
        if !same {
            crate::phase_telemetry::add("command_temp_kept_changed", Duration::ZERO);
            return;
        }
        // `rmdir` removes only an empty directory: nothing is unlinked.
        if std::fs::remove_dir(&run).is_ok() {
            if let Some(alias) = self.alias.take() {
                remove_alias(&alias, &run);
            }
            crate::phase_telemetry::add("command_temp_removed_empty", Duration::ZERO);
            return;
        }
        let (bytes, complete) =
            crate::session_scratch::measure(&run, Instant::now() + FINISH_MEASURE_BUDGET);
        let marker = FinishedMarker {
            device: self.device,
            inode: self.inode,
            alias: self.alias.take(),
            bytes,
            complete,
        };
        match write_marker(&self.scratch, &self.name, &marker) {
            Ok(()) => {
                crate::phase_telemetry::add("command_temp_finished", Duration::ZERO);
                // After the marker, never before: the retirement pass clears
                // a signal before it lists the done container.
                match crate::session_scratch::signal_finished_run(&self.scratch) {
                    Some(true) => {
                        crate::phase_telemetry::add("command_temp_finish_signaled", Duration::ZERO);
                    }
                    Some(false) => crate::phase_telemetry::add(
                        "command_temp_finish_signal_failed",
                        Duration::ZERO,
                    ),
                    None => {}
                }
                if marker.large() {
                    RETIREMENT_WANTED.store(true, Ordering::Release);
                }
            }
            // Left unmarked: the session's own retirement removes it.
            Err(_) => crate::phase_telemetry::add("command_temp_finish_failed", Duration::ZERO),
        }
    }
}

impl Drop for CommandTemp {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Write `marker` as `<scratch>/tmp-done/<name>`, a new regular file in a
/// private directory, never over an existing entry.
fn write_marker(scratch: &Path, name: &str, marker: &FinishedMarker) -> Result<(), String> {
    let done = scratch.join(DONE_CONTAINER);
    private_directory(&done).map_err(|cause| format!("the done container: {cause}"))?;
    let text = serde_json::to_vec(marker).map_err(|error| error.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(done.join(name))
        .map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut file, &text).map_err(|error| error.to_string())
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

/// A fresh alias name: six hex digits, spread by process so concurrent
/// brokers rarely collide (a collision is retried).
fn alias_name() -> String {
    let nonce = RUN_NONCE.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let ms = u64::try_from(crate::clock::epoch_ms().max(0)).unwrap_or(0);
    let mixed = pid
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(ms.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(nonce.wrapping_mul(0x94D0_49BB_1331_11EB));
    format!("{:06x}", (mixed >> 40) & 0x00ff_ffff)
}

/// A short link to `run` in `<base>/ae<uid>/` for the first base whose
/// private directory is usable and whose actual alias path fits
/// [`TMPDIR_PATH_MAX`], pruning dangling links of the broker's naming first.
fn make_alias(bases: &[PathBuf], run: &Path) -> Result<PathBuf, Unavailable> {
    let mut too_long = false;
    // Why the last usable base failed, for the count.
    let mut cause = "no_base";
    for base in bases {
        let container = base.join(format!("ae{}", uid()));
        let name = alias_name();
        if container.join(&name).as_os_str().len() > TMPDIR_PATH_MAX {
            too_long = true;
            continue;
        }
        if let Err(refused) = private_directory(&container) {
            crate::phase_telemetry::add("command_temp_alias_base_refused", Duration::ZERO);
            cause = refused;
            continue;
        }
        prune_dangling_aliases(&container);
        let mut name = name;
        cause = "no_name";
        for _ in 0..CREATE_ATTEMPTS {
            let alias = container.join(&name);
            match std::os::unix::fs::symlink(run, &alias) {
                Ok(()) => return Ok(alias),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    name = alias_name();
                }
                Err(error) => {
                    cause = crate::checkout_trash::errno_cause(&error);
                    break;
                }
            }
        }
    }
    Err(if too_long {
        Unavailable::PathTooLong
    } else {
        Unavailable::Failed(CreateFailure {
            step: "alias",
            cause,
        })
    })
}

/// Remove links of the broker's naming in the private alias container whose
/// target no longer exists (their run was retired, or ended in a broker that
/// was killed). Never a directory or a regular file, never a link that still
/// resolves.
fn prune_dangling_aliases(container: &Path) {
    let Ok(listing) = std::fs::read_dir(container) else {
        return;
    };
    for entry in listing.flatten().take(ALIAS_PRUNE_SCAN_MAX) {
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

    /// A scratch directory whose run paths are always over the limit.
    fn long_scratch() -> (tempfile::TempDir, PathBuf) {
        let (owner, scratch) = scratch();
        let deep = scratch.join("a-long-directory-name-that-pushes-the-path-past-the-limit");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&deep)
            .unwrap();
        (owner, deep)
    }

    /// A short alias base, as `/tmp` is.
    fn short_base() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ct")
            .tempdir_in("/tmp")
            .unwrap()
    }

    /// A base exactly `length` bytes long, like the macOS per-user temp
    /// folder (49 bytes).
    fn base_of_length(owner: &Path, length: usize) -> PathBuf {
        let prefix = owner.as_os_str().len() + 1;
        assert!(length > prefix, "{owner:?} is too long for {length}");
        let base = owner.join("b".repeat(length - prefix));
        std::fs::create_dir(&base).unwrap();
        assert_eq!(base.as_os_str().len(), length);
        base
    }

    fn env_of(temp: &CommandTemp) -> BTreeMap<String, String> {
        temp.environment().into_iter().collect()
    }

    fn done_marker(scratch: &Path, run: &Path) -> PathBuf {
        scratch.join(DONE_CONTAINER).join(run.file_name().unwrap())
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
        assert!(is_alias_name("04f2a1"));
        assert!(is_alias_name(&alias_name()));
        assert!(!is_alias_name("ssh-agent"));
        assert!(!is_alias_name("4f2a1"));
    }

    #[test]
    fn storage_pressure_is_the_gate_headroom() {
        let gib = 1024 * 1024 * 1024;
        assert!(pressured_counts(Some(gib), None));
        assert!(pressured_counts(Some(100 * gib), Some(10)));
        assert!(!pressured_counts(Some(100 * gib), Some(10_000_000)));
        assert!(!pressured_counts(None, None), "unknown is not pressure");
    }

    #[test]
    fn alias_bases_start_with_tmp() {
        assert_eq!(alias_bases()[0], PathBuf::from("/tmp"));
    }

    /// The child sees the run directory (or its alias) in every temp
    /// variable, and what it writes there is on the scratch's volume.
    #[test]
    fn a_child_gets_the_run_directory_as_its_temp_folder() {
        let (_owner, scratch) = scratch();
        let base = short_base();
        let temp = CommandTemp::create_in(&scratch, &[base.path().to_path_buf()], false).unwrap();
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
    /// through a short link, removed with an empty run.
    #[test]
    fn a_long_run_path_gets_a_short_alias() {
        let (_owner, deep) = long_scratch();
        let base = short_base();
        let temp = CommandTemp::create_in(&deep, &[base.path().to_path_buf()], false).unwrap();
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

    /// The macOS per-user temp folder is 49 bytes: as a base it still fits
    /// an alias, sized on the actual alias path.
    #[test]
    fn a_49_byte_temp_base_still_yields_an_alias() {
        let (_owner, deep) = long_scratch();
        let owner = short_base();
        let base = base_of_length(owner.path(), 49);
        let temp = CommandTemp::create_in(&deep, std::slice::from_ref(&base), false).unwrap();
        let alias = temp.tmpdir();
        assert!(alias.starts_with(&base), "{alias:?}");
        assert_eq!(
            alias.as_os_str().len(),
            49 + 1 + format!("ae{}", uid()).len() + 1 + ALIAS_NAME_LEN
        );
        assert!(alias.as_os_str().len() <= TMPDIR_PATH_MAX, "{alias:?}");
    }

    /// `ae<uid>` replaced by a symbolic link is refused, and the next base
    /// is used; with no other base the command keeps the inherited folder.
    #[test]
    fn a_symlinked_alias_container_is_refused() {
        let (_owner, deep) = long_scratch();
        let base = short_base();
        let elsewhere = base.path().join("elsewhere");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&elsewhere)
            .unwrap();
        let linked = base.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(&elsewhere, linked.join(format!("ae{}", uid()))).unwrap();
        assert_eq!(
            CommandTemp::create_in(&deep, std::slice::from_ref(&linked), false).err(),
            Some(Unavailable::Failed(CreateFailure {
                step: "alias",
                cause: "not_private"
            }))
        );
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        let fallback = base.path().join("fallback");
        std::fs::create_dir(&fallback).unwrap();
        let temp = CommandTemp::create_in(&deep, &[linked, fallback.clone()], false).unwrap();
        assert!(temp.tmpdir().starts_with(&fallback));
    }

    #[test]
    fn a_foreign_alias_container_is_refused() {
        let (_owner, deep) = long_scratch();
        let base = short_base();
        let container = base.path().join(format!("ae{}", uid()));
        std::fs::create_dir(&container).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            CommandTemp::create_in(&deep, &[base.path().to_path_buf()], false).err(),
            Some(Unavailable::Failed(CreateFailure {
                step: "alias",
                cause: "not_private"
            }))
        );
    }

    /// When no alias fits, the command keeps the inherited temp folder and
    /// nothing is left behind.
    #[test]
    fn a_too_long_alias_base_falls_back_to_the_inherited_folder() {
        let (_owner, deep) = long_scratch();
        assert_eq!(
            CommandTemp::create_in(&deep, std::slice::from_ref(&deep), false).err(),
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
        let outer = CommandTemp::create_in(&scratch, &[base.path().to_path_buf()], false).unwrap();
        let tmpdir = outer.tmpdir();
        let given = Some(tmpdir.as_os_str());
        assert!(runs_inside(&scratch, given, given));
        assert!(!runs_inside(&other, given, given), "another session's run");
        assert!(!runs_inside(&scratch, given, Some("/tmp".as_ref())));
        assert!(!runs_inside(&scratch, None, given));
    }

    /// Through a real child: a process started with a run's environment
    /// sees it as its own run (no nesting); one without it does not.
    #[test]
    fn a_real_child_inside_a_run_inherits_it() {
        let (_owner, deep) = long_scratch();
        let base = short_base();
        let outer = CommandTemp::create_in(&deep, &[base.path().to_path_buf()], false).unwrap();
        let script = "printf '%s|%s' \"$AETHYME_COMMAND_TEMP_DIR\" \"$TMPDIR\"";
        let mut child = std::process::Command::new("sh");
        child.arg("-c").arg(script);
        outer.apply(&mut child);
        let output = String::from_utf8(child.output().unwrap().stdout).unwrap();
        let (marker, tmpdir) = output.split_once('|').unwrap();
        assert!(runs_inside(
            &deep,
            Some(marker.as_ref()),
            Some(tmpdir.as_ref())
        ));
        let mut plain = std::process::Command::new("sh");
        plain
            .arg("-c")
            .arg(script)
            .env_remove(COMMAND_TEMP_ENV)
            .env("TMPDIR", "/tmp");
        let output = String::from_utf8(plain.output().unwrap().stdout).unwrap();
        let (marker, tmpdir) = output.split_once('|').unwrap();
        assert!(!runs_inside(
            &deep,
            Some(marker.as_ref()).filter(|marker: &&std::ffi::OsStr| !marker.is_empty()),
            Some(tmpdir.as_ref())
        ));
    }

    /// A live container that is not private refuses, and the failure names
    /// its step and cause instead of a bare "failed".
    #[test]
    fn a_failed_create_names_its_step_and_cause() {
        let (_owner, scratch) = scratch();
        let live = scratch.join(LIVE_CONTAINER);
        std::fs::create_dir(&live).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&live, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = CommandTemp::create_in(&scratch, &[PathBuf::from("/tmp")], false).err();
        assert_eq!(
            refused,
            Some(Unavailable::Failed(CreateFailure {
                step: "live_container",
                cause: "not_private"
            }))
        );
        let Some(Unavailable::Failed(failure)) = refused else {
            unreachable!()
        };
        assert_eq!(failure.key(), "live_container.not_private");
        // A missing scratch directory fails at the same step, by errno.
        let gone = scratch.join("gone");
        assert_eq!(
            CommandTemp::create_in(&gone, &[PathBuf::from("/tmp")], false).err(),
            Some(Unavailable::Failed(CreateFailure {
                step: "live_container",
                cause: "not_found"
            }))
        );
    }

    /// Concurrent commands of one session creating the containers at once
    /// never fail: the `mkdir` that loses sees `EEXIST` and checks the
    /// winner's directory.
    #[test]
    fn concurrent_creates_share_the_containers() {
        let (_owner, deep) = long_scratch();
        let base = short_base();
        let bases = vec![base.path().to_path_buf()];
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let deep = deep.clone();
                let bases = bases.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    CommandTemp::create_in(&deep, &bases, false).map(|temp| temp.run_dir())
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let run = handle.join().unwrap();
            assert!(run.is_ok(), "{run:?}");
        }
    }

    #[test]
    fn a_command_inside_a_run_inherits_it() {
        let (_owner, scratch) = scratch();
        assert_eq!(
            CommandTemp::create_in(&scratch, &[PathBuf::from("/tmp")], true).err(),
            Some(Unavailable::Inherited)
        );
        assert!(!scratch.join(LIVE_CONTAINER).exists());
    }

    /// Success, failure and timeout all end the same way: the guard drops
    /// after the command, an empty run goes with one rmdir, and a non-empty
    /// one stays in place, alias included, with a marker beside it.
    #[test]
    fn a_finished_run_is_removed_when_empty_and_marked_otherwise() {
        let (_owner, deep) = long_scratch();
        let base = short_base();
        let bases = [base.path().to_path_buf()];
        for script in ["exit 0", "exit 3", "sleep 5"] {
            let empty = CommandTemp::create_in(&deep, &bases, false).unwrap();
            let empty_run = empty.run_dir();
            let full = CommandTemp::create_in(&deep, &bases, false).unwrap();
            let full_run = full.run_dir();
            let full_alias = full.tmpdir();
            let mut child = std::process::Command::new("sh");
            child.arg("-c").arg(format!(
                "mktemp -d \"$TMPDIR/build.XXXXXX\" >/dev/null; {script}"
            ));
            full.apply(&mut child);
            let mut child = child.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
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
            assert!(full_run.is_dir(), "{script}: the run stays in place");
            assert_eq!(
                std::fs::read_link(&full_alias).unwrap(),
                full_run,
                "{script}: a process left behind keeps its TMPDIR"
            );
            let marker: FinishedMarker =
                serde_json::from_slice(&std::fs::read(done_marker(&deep, &full_run)).unwrap())
                    .unwrap();
            assert_eq!(marker.alias.as_deref(), Some(full_alias.as_path()));
            let metadata = std::fs::metadata(&full_run).unwrap();
            assert_eq!(
                (marker.device, marker.inode),
                (metadata.dev(), metadata.ino())
            );
            assert!(!marker.large(), "{script}: {marker:?}");
        }
    }

    /// A directory that is not the one the guard created is never touched.
    #[test]
    fn a_swapped_run_directory_is_left_alone() {
        let (_owner, scratch) = scratch();
        let temp = CommandTemp::create_in(&scratch, &[PathBuf::from("/tmp")], false).unwrap();
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
        std::os::unix::fs::symlink(base.path().join("gone"), container.join("00000a")).unwrap();
        std::os::unix::fs::symlink(&live_target, container.join("00000b")).unwrap();
        std::os::unix::fs::symlink(base.path().join("gone"), container.join("other")).unwrap();
        std::fs::write(container.join("00000c"), b"a file").unwrap();
        prune_dangling_aliases(&container);
        assert!(std::fs::symlink_metadata(container.join("00000a")).is_err());
        assert!(std::fs::symlink_metadata(container.join("00000b")).is_ok());
        assert!(std::fs::symlink_metadata(container.join("other")).is_ok());
        assert!(container.join("00000c").is_file());
    }

    #[test]
    fn an_alias_is_removed_only_when_it_still_names_its_run() {
        let base = short_base();
        let run = base.path().join("run");
        let other = base.path().join("other");
        let alias = base.path().join("alias");
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        assert!(!remove_alias(&alias, &run));
        assert!(std::fs::symlink_metadata(&alias).is_ok());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&run, &alias).unwrap();
        assert!(remove_alias(&alias, &run));
        std::fs::create_dir(&alias).unwrap();
        assert!(!remove_alias(&alias, &run), "a directory is never removed");
    }
}
