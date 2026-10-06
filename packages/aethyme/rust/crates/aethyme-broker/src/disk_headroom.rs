//! Refusing to start a gate that cannot fit its build.
//!
//! A full disk does not surface as "disk full". It surfaces as
//! `ld: write() failed, errno=28`, then `cached cgu … should have an object
//! file, but doesn't`, then a poisoned test-binary cache, then ten unrelated
//! test failures. Worse, that outcome is recorded as a **gate verdict** and
//! returned from cache on the next attempt, so the failure outlives the
//! condition that caused it and a retry cannot clear it.
//!
//! Observed on this repository: a release submit failed with ten test failures
//! in `ai_ready_cli`; the same tree passed in 555s once space was reclaimed and
//! the cached verdict bypassed. Nothing in that chain named the disk.
//!
//! So the check is not an optimisation. It is the difference between a
//! diagnosable refusal and a plausible, cacheable lie.

/// Free bytes a gate should have before it is allowed to start.
///
/// A debug build of this workspace is several gigabytes, and cargo writes
/// incremental state before it links. Eight is chosen to refuse while there is
/// still room to *act* -- reclaiming needs the tooling to run.
pub const DEFAULT_GATE_HEADROOM_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Free inodes a gate should have before it is allowed to start.
///
/// A single dependency install can create tens of thousands of entries. Keep a
/// reserve large enough for the broker and recovery tools to keep working.
pub const MIN_GATE_HEADROOM_INODES: u64 = 100_000;

/// Free space on a filesystem, captured in one statvfs call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskHeadroom {
    pub bytes: u64,
    pub inodes: u64,
}

/// Filesystem objects and their logical byte sizes below a path.
///
/// Inodes count directory entries and files without following symlinks.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectoryUsage {
    pub bytes: u64,
    pub inodes: u64,
}

/// Measure one path without following symlinks. Directories and symlinks
/// consume inodes too; only regular-file lengths contribute bytes.
pub(crate) fn directory_usage_without_following_links(
    path: &std::path::Path,
) -> std::io::Result<DirectoryUsage> {
    fn visit(
        path: &std::path::Path,
        usage: &mut DirectoryUsage,
        seen: &mut std::collections::HashSet<(u64, u64)>,
        deadline: Option<std::time::Instant>,
    ) -> std::io::Result<()> {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "directory measurement budget expired",
            ));
        }
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        let first_link = seen.insert((metadata.dev(), metadata.ino()));
        if first_link {
            usage.inodes = usage.inodes.saturating_add(1);
        }
        // Preserve the established logical byte total for hard-linked paths;
        // inode accounting deduplicates the shared filesystem object.
        if metadata.is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
        }
        if metadata.is_dir() && first_link {
            for entry in std::fs::read_dir(path)? {
                visit(&entry?.path(), usage, seen, deadline)?;
            }
        }
        Ok(())
    }

    let mut usage = DirectoryUsage::default();
    visit(
        path,
        &mut usage,
        &mut std::collections::HashSet::new(),
        None,
    )?;
    Ok(usage)
}

/// Best-effort counterpart for status reports: inaccessible entries are
/// skipped while the rest of the tree remains useful.
pub(crate) fn directory_usage_best_effort(path: &std::path::Path) -> DirectoryUsage {
    fn visit(
        path: &std::path::Path,
        usage: &mut DirectoryUsage,
        seen: &mut std::collections::HashSet<(u64, u64)>,
    ) {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        let first_link = seen.insert((metadata.dev(), metadata.ino()));
        if first_link {
            usage.inodes = usage.inodes.saturating_add(1);
        }
        // Preserve the established logical byte total for hard-linked paths;
        // inode accounting deduplicates the shared filesystem object.
        if metadata.is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
        }
        if metadata.is_dir() && first_link {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                visit(&entry.path(), usage, seen);
            }
        }
    }

    let mut usage = DirectoryUsage::default();
    visit(path, &mut usage, &mut std::collections::HashSet::new());
    usage
}

/// Measure a tree, returning neither partial bytes nor partial inode counts
/// when the caller's budget expires.
pub(crate) fn directory_usage_bounded(
    path: &std::path::Path,
    deadline: std::time::Instant,
) -> Option<DirectoryUsage> {
    fn visit(
        path: &std::path::Path,
        usage: &mut DirectoryUsage,
        seen: &mut std::collections::HashSet<(u64, u64)>,
        deadline: std::time::Instant,
    ) -> std::io::Result<()> {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "directory measurement budget expired",
            ));
        }
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        let first_link = seen.insert((metadata.dev(), metadata.ino()));
        if first_link {
            usage.inodes = usage.inodes.saturating_add(1);
        }
        // Preserve the established logical byte total for hard-linked paths;
        // inode accounting deduplicates the shared filesystem object.
        if metadata.is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
        }
        if metadata.is_dir() && first_link {
            for entry in std::fs::read_dir(path)? {
                visit(&entry?.path(), usage, seen, deadline)?;
            }
        }
        Ok(())
    }

    let mut usage = DirectoryUsage::default();
    visit(
        path,
        &mut usage,
        &mut std::collections::HashSet::new(),
        deadline,
    )
    .ok()?;
    Some(usage)
}

/// Free space on the filesystem holding `path`, or `None` when it cannot be
/// determined.
///
/// Unknown is not treated as low: refusing every gate because `statvfs` failed
/// would be worse than the problem, and the build's own errors remain the
/// fallback.
pub(crate) fn available_headroom(path: &std::path::Path) -> Option<DiskHeadroom> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: c_path is a valid NUL-terminated string that outlives the call,
    // and stat is fully initialised by the callee on success.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        Some(DiskHeadroom {
            bytes: stat.f_bavail as u64 * stat.f_frsize as u64,
            inodes: stat.f_favail as u64,
        })
    }
}

pub fn available_bytes(path: &std::path::Path) -> Option<u64> {
    available_headroom(path).map(|headroom| headroom.bytes)
}

/// The nearest existing ancestor carries the same filesystem as a missing
/// path that will eventually be created beneath it.
pub(crate) fn available_headroom_at_or_above(path: &std::path::Path) -> Option<DiskHeadroom> {
    path.ancestors()
        .filter(|ancestor| ancestor.exists())
        .find_map(available_headroom)
}

/// Test-only: the free space, in bytes, every headroom decision for a
/// throwaway repository reads instead of the host's.
///
/// Without it the suite measures the machine it runs on: below 8 GiB free,
/// gates refuse, `status` grows a `host.gate-headroom` row and a configured
/// worktree root falls back, so some twenty unrelated tests fail on a full
/// disk and pass once space is freed. `.cargo/config.toml` sets a generous
/// value for every test; a test of the refusal itself sets a low one.
///
/// Honoured only for a repository under the system temporary directory, the
/// same narrowing as the gate-trust escape: `.cargo/config.toml` also reaches
/// `cargo run`, and an environment variable must not be able to admit a gate
/// on a real checkout whose disk is full.
pub const TEST_AVAILABLE_BYTES_ENV: &str = "AETHYME_TEST_AVAILABLE_BYTES";

pub const TEST_AVAILABLE_INODES_ENV: &str = "AETHYME_TEST_AVAILABLE_INODES";

/// Free space for a headroom decision about `path`, made on behalf of the
/// repository whose checkout is `repository`.
///
/// Every broker decision -- admitting a gate, the status headroom row, a
/// configured worktree root's floor, the sweep's urgency -- reads through
/// here, so a simulated reading reaches all of them or none.
pub(crate) fn available_bytes_for(
    repository: &std::path::Path,
    path: &std::path::Path,
) -> Option<u64> {
    simulated_available_bytes(repository).or_else(|| available_bytes(path))
}

/// [`available_bytes_at_or_above`] behind the same test seam as

pub(crate) fn available_headroom_for(
    repository: &std::path::Path,
    path: &std::path::Path,
) -> Option<DiskHeadroom> {
    let measured = available_headroom(path);
    let bytes =
        simulated_available_bytes(repository).or_else(|| measured.map(|value| value.bytes))?;
    let inodes =
        simulated_available_inodes(repository).or_else(|| measured.map(|value| value.inodes))?;
    Some(DiskHeadroom { bytes, inodes })
}

pub(crate) fn available_headroom_at_or_above_for(
    repository: &std::path::Path,
    path: &std::path::Path,
) -> Option<DiskHeadroom> {
    let measured = available_headroom_at_or_above(path);
    let bytes =
        simulated_available_bytes(repository).or_else(|| measured.map(|value| value.bytes))?;
    let inodes =
        simulated_available_inodes(repository).or_else(|| measured.map(|value| value.inodes))?;
    Some(DiskHeadroom { bytes, inodes })
}

fn simulated_available_bytes(repository: &std::path::Path) -> Option<u64> {
    simulated_available_bytes_from(
        std::env::var_os(TEST_AVAILABLE_BYTES_ENV).as_deref(),
        repository,
    )
}

fn simulated_available_inodes(repository: &std::path::Path) -> Option<u64> {
    simulated_available_inodes_from(
        std::env::var_os(TEST_AVAILABLE_INODES_ENV).as_deref(),
        repository,
    )
}

fn simulated_available_inodes_from(
    value: Option<&std::ffi::OsStr>,
    repository: &std::path::Path,
) -> Option<u64> {
    let inodes = value?.to_str()?.trim().parse().ok()?;
    crate::host_state::path_is_ephemeral(repository).then_some(inodes)
}

/// [`simulated_available_bytes`] with the environment value supplied, so the
/// narrowing is tested without mutating the process environment.
fn simulated_available_bytes_from(
    value: Option<&std::ffi::OsStr>,
    repository: &std::path::Path,
) -> Option<u64> {
    let bytes = value?.to_str()?.trim().parse().ok()?;
    crate::host_state::path_is_ephemeral(repository).then_some(bytes)
}

/// How hard the autonomous sweep should work right now.
///
/// Derived from the same fact the gate refuses on, so the two cannot disagree
/// about whether the disk is in trouble. A fixed budget spends the same five
/// seconds a day whether the volume is at 40% or about to refuse every gate,
/// which is the state that actually needs the work done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepUrgency {
    /// Free space is above the threshold a gate needs; keep the light touch.
    Routine,
    /// Free space is already below what a gate requires to start. Reclaiming
    /// is now the thing standing between this machine and refused work.
    Pressured,
}

impl SweepUrgency {
    /// Budget multiplier. Under pressure a minute of deletion is cheap next to
    /// a gate that will not run at all.
    pub fn budget_scale(self) -> u64 {
        match self {
            Self::Routine => 1,
            Self::Pressured => 12,
        }
    }

    /// Interval divisor. Hourly under pressure, daily otherwise.
    pub fn interval_divisor(self) -> i64 {
        match self {
            Self::Routine => 1,
            Self::Pressured => 24,
        }
    }
}

/// Classify current headroom for `path`.
///
/// Unknown headroom stays `Routine` for the same reason `refusal` fails open:
/// a failed `statvfs` is not evidence of a full disk, and reacting to it would
/// make an unreadable filesystem look like an emergency.
pub fn sweep_urgency(available: Option<u64>, required: u64) -> SweepUrgency {
    match available {
        Some(available) if available < required => SweepUrgency::Pressured,
        _ => SweepUrgency::Routine,
    }
}

/// Classify pressure against both limits a gate refuses at.
pub(crate) fn sweep_urgency_with_inodes(
    available_bytes: Option<u64>,
    required_bytes: u64,
    available_inodes: Option<u64>,
    required_inodes: u64,
) -> SweepUrgency {
    if sweep_urgency(available_bytes, required_bytes) == SweepUrgency::Pressured
        || sweep_urgency(available_inodes, required_inodes) == SweepUrgency::Pressured
    {
        SweepUrgency::Pressured
    } else {
        SweepUrgency::Routine
    }
}

/// Render bytes for operator-facing messages.
pub(crate) fn format_gibibytes(bytes: u64) -> String {
    gibibytes(bytes)
}

fn gibibytes(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// What a refusing gate knows about its repository's gate cache.
#[derive(Debug, Clone, Copy)]
pub struct GateCacheUsage<'a> {
    /// `<host cache>/gates/<repository key>`.
    pub root: &'a std::path::Path,
    /// Every entry under `root`.
    pub total_bytes: u64,
    pub total_inodes: u64,
    /// The cache key this gate uses: the entry gc plan keeps as active.
    pub active_key: &'a str,
    pub active_bytes: u64,
    pub active_inodes: u64,
}

/// The refusal message for a gate that cannot safely start, or `None`.
///
/// Separated from the syscall so the decision is testable without a full disk.
///
/// The recovery it names is a broker subcommand, not a shell pipeline. This
/// message used to suggest `du -sh "$(aethyme broker paths worktrees)"/*/`,
/// and there is no `paths` subcommand -- so the substitution produced nothing
/// and the suggestion expanded to `du -sh /*/`, telling an operator who had
/// just run out of disk to walk the entire root filesystem (#167). A command
/// substitution inside a diagnostic fails open: when it is wrong the command
/// still runs, against something else. `gc plan` cannot be wrong that way, and
/// it reports reclaimable bytes per worktree rather than raw sizes.
pub fn refusal(available: Option<u64>, required: u64) -> Option<String> {
    refusal_with_gate_cache(available, required, None)
}

/// [`refusal`], naming this repository's gate cache and its measured size.
///
/// The gate cache sits under the per-user cache directory rather than in any
/// worktree, and on the host behind #295 it was the largest reclaimable item
/// on the disk -- 7.7 GiB, almost exactly the headroom the gates were refusing
/// for -- while this message pointed only at worktree build artefacts. Naming
/// it, with bytes, is what stops an operator from concluding that `gc plan`'s
/// total is everything there is.
pub fn refusal_with_gate_cache(
    available: Option<u64>,
    required: u64,
    gate_cache: Option<GateCacheUsage<'_>>,
) -> Option<String> {
    refusal_with_headroom_and_gate_cache(available, None, required, 0, gate_cache)
}

/// Refuse when either byte or inode headroom is known to be below its limit.
pub(crate) fn refusal_with_headroom(
    available_bytes: Option<u64>,
    available_inodes: Option<u64>,
    required_bytes: u64,
    required_inodes: u64,
) -> Option<String> {
    refusal_with_headroom_and_gate_cache(
        available_bytes,
        available_inodes,
        required_bytes,
        required_inodes,
        None,
    )
}

/// The headroom refusal with measured repository gate-cache usage.
pub(crate) fn refusal_with_headroom_and_gate_cache(
    available_bytes: Option<u64>,
    available_inodes: Option<u64>,
    required_bytes: u64,
    required_inodes: u64,
    gate_cache: Option<GateCacheUsage<'_>>,
) -> Option<String> {
    let bytes_low = available_bytes.is_some_and(|available| available < required_bytes);
    let inodes_low = available_inodes.is_some_and(|available| available < required_inodes);
    if !bytes_low && !inodes_low {
        return None;
    }

    let mut limits = Vec::new();
    if bytes_low {
        limits.push(format!(
            "{} free, {} required",
            gibibytes(available_bytes?),
            gibibytes(required_bytes)
        ));
    }
    if inodes_low {
        limits.push(format!(
            "{} inodes free, {} inodes required",
            available_inodes?, required_inodes
        ));
    }
    let capacity = limits.join("; ");
    let gate_cache = match gate_cache {
        Some(usage) if usage.total_bytes > 0 || usage.total_inodes > 0 => {
            let older_bytes = usage.total_bytes.saturating_sub(usage.active_bytes);
            let older_inodes = usage.total_inodes.saturating_sub(usage.active_inodes);
            format!(
                "\nThis repository's gate cache holds {} and {} inodes at {}: {} in {} ({} inodes), the cache this gate reuses, which gc plan keeps active; {} in older entries ({} inodes), which gc plan proposes beyond its budget once no gate holds them. gc plan --include-active-gate-cache proposes the active cache too, and the next gate then rebuilds it from scratch.",
                gibibytes(usage.total_bytes),
                usage.total_inodes,
                usage.root.display(),
                gibibytes(usage.active_bytes),
                usage.active_key,
                usage.active_inodes,
                gibibytes(older_bytes),
                older_inodes,
            )
        }
        _ => String::new(),
    };
    let mut message = format!(
        "refusing to start: {capacity}. A build that runs out of space or inodes does not report a useful disk error; it reports link failures, a corrupt incremental cache and unrelated test failures, and that verdict is then cached against this tree. Reclaim space and retry.\nBuild artefacts in finished session worktrees and the gate cache are usually the largest reclaimable sets, and the broker measures both:\n  aethyme broker gc plan"
    );
    message.push_str(&gate_cache);
    Some(message)
}

#[cfg(test)]
mod tests {

    /// The sweep must react to the same fact the gate refuses on, or the two
    /// disagree about whether the disk is in trouble.
    #[test]
    fn pressure_tracks_the_threshold_a_gate_refuses_at() {
        let required = DEFAULT_GATE_HEADROOM_BYTES;
        assert_eq!(
            sweep_urgency(Some(required - 1), required),
            SweepUrgency::Pressured
        );
        assert_eq!(
            sweep_urgency(Some(required), required),
            SweepUrgency::Routine,
            "exactly enough headroom is not pressure"
        );
        // Same input that makes `refusal` fire must make the sweep hurry.
        assert!(refusal(Some(required - 1), required).is_some());
    }

    /// Unknown headroom is not an emergency. A failed statvfs would otherwise
    /// put every machine into the aggressive cadence permanently.
    #[test]
    fn unknown_headroom_stays_routine() {
        assert_eq!(
            sweep_urgency(None, DEFAULT_GATE_HEADROOM_BYTES),
            SweepUrgency::Routine
        );
        assert!(refusal(None, DEFAULT_GATE_HEADROOM_BYTES).is_none());
    }

    /// Pressure has to actually change the work done, not just the label.
    #[test]
    fn pressure_widens_the_budget_and_shortens_the_interval() {
        let routine = SweepUrgency::Routine;
        let pressured = SweepUrgency::Pressured;
        assert_eq!(routine.budget_scale(), 1);
        assert_eq!(routine.interval_divisor(), 1);
        assert!(pressured.budget_scale() > routine.budget_scale());
        assert!(pressured.interval_divisor() > routine.interval_divisor());

        // 5s/24h routine becomes a minute, hourly.
        assert_eq!(5_000_u64 * pressured.budget_scale(), 60_000);
        assert_eq!(24_i64 * 3_600_000 / pressured.interval_divisor(), 3_600_000);
    }
    use super::*;

    #[test]
    fn low_inode_headroom_refuses_even_when_byte_headroom_is_ample() {
        let message = refusal_with_headroom(
            Some(DEFAULT_GATE_HEADROOM_BYTES),
            Some(MIN_GATE_HEADROOM_INODES - 1),
            DEFAULT_GATE_HEADROOM_BYTES,
            MIN_GATE_HEADROOM_INODES,
        )
        .expect("low inode headroom must refuse");
        assert!(
            message.contains("99999 inodes free, 100000 inodes required"),
            "{message}"
        );
    }

    #[test]
    fn inode_threshold_is_inclusive_and_unknown_inode_count_fails_open() {
        assert!(
            refusal_with_headroom(
                Some(DEFAULT_GATE_HEADROOM_BYTES),
                Some(MIN_GATE_HEADROOM_INODES),
                DEFAULT_GATE_HEADROOM_BYTES,
                MIN_GATE_HEADROOM_INODES,
            )
            .is_none()
        );
        assert!(
            refusal_with_headroom(
                Some(DEFAULT_GATE_HEADROOM_BYTES),
                None,
                DEFAULT_GATE_HEADROOM_BYTES,
                MIN_GATE_HEADROOM_INODES,
            )
            .is_none()
        );
    }

    #[test]
    fn inode_pressure_also_accelerates_the_artifact_sweep() {
        assert_eq!(
            sweep_urgency_with_inodes(
                Some(DEFAULT_GATE_HEADROOM_BYTES),
                DEFAULT_GATE_HEADROOM_BYTES,
                Some(MIN_GATE_HEADROOM_INODES - 1),
                MIN_GATE_HEADROOM_INODES,
            ),
            SweepUrgency::Pressured
        );
        assert_eq!(
            sweep_urgency_with_inodes(
                Some(DEFAULT_GATE_HEADROOM_BYTES),
                DEFAULT_GATE_HEADROOM_BYTES,
                Some(MIN_GATE_HEADROOM_INODES),
                MIN_GATE_HEADROOM_INODES,
            ),
            SweepUrgency::Routine
        );
    }

    #[test]
    fn directory_usage_counts_nodes_and_hardlinks_without_following_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let tree = root.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::write(tree.join("nested/file"), b"abc").unwrap();
        std::fs::hard_link(tree.join("nested/file"), tree.join("hardlink")).unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, tree.join("link")).unwrap();

        let usage = directory_usage_without_following_links(&tree).unwrap();
        assert_eq!(
            usage.bytes, 6,
            "both hard-linked paths retain their logical byte size; the symlink target is outside the tree"
        );
        assert_eq!(
            usage.inodes, 4,
            "the tree root, nested directory, file, and symlink each use an inode; a hard link does not"
        );
    }

    #[test]
    fn bounded_directory_usage_does_not_return_partial_counts() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"content").unwrap();
        assert!(
            directory_usage_bounded(
                root.path(),
                std::time::Instant::now() - std::time::Duration::from_millis(1),
            )
            .is_none()
        );
    }

    #[test]
    fn ample_space_does_not_refuse() {
        assert!(refusal(Some(50 * 1024 * 1024 * 1024), DEFAULT_GATE_HEADROOM_BYTES).is_none());
    }

    #[test]
    fn exactly_the_requirement_is_enough() {
        assert!(
            refusal(
                Some(DEFAULT_GATE_HEADROOM_BYTES),
                DEFAULT_GATE_HEADROOM_BYTES
            )
            .is_none()
        );
    }

    /// The observed failure: 1.4 GiB free, and the build died claiming
    /// unrelated test failures.
    #[test]
    fn too_little_space_refuses_and_says_so_in_bytes_a_human_reads() {
        let message = refusal(Some(1_503_238_553), DEFAULT_GATE_HEADROOM_BYTES)
            .expect("1.4 GiB must refuse against an 8 GiB requirement");
        assert!(message.contains("1.4 GiB free"), "{message}");
        assert!(message.contains("8.0 GiB required"), "{message}");
    }

    /// The point of the message is that the *next* failure is diagnosable, so
    /// it has to say what a disk failure looks like and how to recover.
    #[test]
    fn the_refusal_explains_the_symptom_and_names_a_recovery() {
        let message = refusal(Some(0), DEFAULT_GATE_HEADROOM_BYTES).unwrap();
        assert!(message.contains("cached"), "{message}");
        assert!(message.contains("Reclaim space"), "{message}");
    }

    /// The regression that made #167 worth filing: the recovery command has to
    /// survive being pasted into a shell.
    ///
    /// Asserted as a property rather than against the literal text, because
    /// the failure was not "the wrong command" -- it was a command whose
    /// meaning depended on a substitution that silently produced nothing.
    /// Any future edit that reintroduces one fails here regardless of which
    /// subcommand it names.
    #[test]
    fn the_recovery_command_does_not_depend_on_a_command_substitution() {
        let message = refusal(Some(0), DEFAULT_GATE_HEADROOM_BYTES).unwrap();
        assert!(
            !message.contains("$("),
            "a substitution that resolves to nothing turns the suggestion into a \
             different command, and the operator reading this has no disk to spare \
             for finding that out: {message}"
        );
        assert!(
            !message.contains("/*/"),
            "an unanchored glob is what the empty substitution expanded against: {message}"
        );
    }

    /// Refusing every gate because the syscall failed would be worse than the
    /// problem it prevents.
    #[test]
    fn unknown_free_space_does_not_refuse() {
        assert!(refusal(None, DEFAULT_GATE_HEADROOM_BYTES).is_none());
    }

    /// #295: the gate cache was the largest reclaimable item and the message
    /// never mentioned it. With a measured size it must say where and how much.
    ///
    /// And it must say what `gc plan` will actually do with it: the cache
    /// this gate reuses is kept (active), so a message promising it would be
    /// proposed sends the operator to a plan that proposes nothing.
    #[test]
    fn a_measured_gate_cache_is_named_with_its_bytes() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let root = std::path::Path::new("/cache/gates/abc");
        let usage = GateCacheUsage {
            root,
            total_bytes: 7 * GIB + 800 * 1024 * 1024,
            total_inodes: 19_000,
            active_key: "rust-workspace-v3",
            active_bytes: 5 * GIB,
            active_inodes: 11_000,
        };
        let message =
            refusal_with_gate_cache(Some(0), DEFAULT_GATE_HEADROOM_BYTES, Some(usage)).unwrap();
        assert!(message.contains("gate cache holds 7.8 GiB"), "{message}");
        assert!(message.contains("/cache/gates/abc"), "{message}");
        assert!(
            message.contains("5.0 GiB in rust-workspace-v3"),
            "{message}"
        );
        assert!(message.contains("keeps active"), "{message}");
        assert!(message.contains("2.8 GiB in older entries"), "{message}");
        assert!(message.contains("--include-active-gate-cache"), "{message}");
        assert!(message.contains("rebuilds it from scratch"), "{message}");
        assert!(!message.contains("least recently used"), "{message}");
        assert!(message.contains("aethyme broker gc plan"), "{message}");
        // An empty cache is not worth a sentence.
        let empty = GateCacheUsage {
            total_bytes: 0,
            total_inodes: 0,
            active_bytes: 0,
            active_inodes: 0,
            ..usage
        };
        let quiet =
            refusal_with_gate_cache(Some(0), DEFAULT_GATE_HEADROOM_BYTES, Some(empty)).unwrap();
        assert!(!quiet.contains("gate cache holds"), "{quiet}");
    }

    /// The simulated reading is for throwaway repositories only: a real
    /// checkout with a full disk must still be refused, whatever the
    /// environment says.
    #[test]
    fn simulated_free_space_applies_only_to_a_throwaway_repository() {
        let value = Some(std::ffi::OsStr::new("4096"));
        let throwaway = tempfile::tempdir().unwrap();
        assert_eq!(
            simulated_available_bytes_from(value, throwaway.path()),
            Some(4096)
        );

        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !crate::host_state::path_is_ephemeral(checkout),
            "this test needs a checkout outside the temporary directory"
        );
        assert_eq!(simulated_available_bytes_from(value, checkout), None);
    }

    #[test]
    fn simulated_free_inodes_apply_only_to_a_throwaway_repository() {
        let value = Some(std::ffi::OsStr::new("512"));
        let throwaway = tempfile::tempdir().unwrap();
        assert_eq!(
            simulated_available_inodes_from(value, throwaway.path()),
            Some(512)
        );

        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !crate::host_state::path_is_ephemeral(checkout),
            "this test needs a checkout outside the temporary directory"
        );
        assert_eq!(simulated_available_inodes_from(value, checkout), None);
    }

    #[test]
    fn an_unset_or_unparsable_simulation_reads_the_real_disk() {
        let throwaway = tempfile::tempdir().unwrap();
        assert_eq!(simulated_available_bytes_from(None, throwaway.path()), None);
        assert_eq!(
            simulated_available_bytes_from(Some(std::ffi::OsStr::new("lots")), throwaway.path()),
            None
        );
    }

    #[test]
    fn the_real_filesystem_reports_something_plausible() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let headroom = available_headroom(here).expect("statvfs works on the checkout");
        assert!(headroom.bytes > 0, "a writable checkout has free bytes");
        assert!(available_bytes(here).is_some());
        assert!(available_headroom(here).is_some_and(|headroom| headroom.inodes > 0));
    }
}
