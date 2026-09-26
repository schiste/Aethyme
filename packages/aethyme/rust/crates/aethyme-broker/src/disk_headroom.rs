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

/// Free space on the filesystem holding `path`, or `None` when it cannot be
/// determined.
///
/// Unknown is not treated as low: refusing every gate because `statvfs` failed
/// would be worse than the problem, and the build's own errors remain the
/// fallback.
pub fn available_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call,
    // and `stat` is fully initialised by the callee on success.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        Some(stat.f_bavail as u64 * stat.f_frsize as u64)
    }
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

/// Render bytes for operator-facing messages.
pub(crate) fn format_gibibytes(bytes: u64) -> String {
    gibibytes(bytes)
}

fn gibibytes(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
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
    gate_cache: Option<(&std::path::Path, u64)>,
) -> Option<String> {
    let available = available?;
    if available >= required {
        return None;
    }
    let gate_cache = match gate_cache {
        Some((root, bytes)) if bytes > 0 => format!(
            "\nThis repository's gate cache holds {} at {}. Once this gate exits, \
             `gc plan` proposes its least recently used entries beyond the \
             retention budget, and never one a running gate holds.",
            gibibytes(bytes),
            root.display()
        ),
        _ => String::new(),
    };
    Some(format!(
        "refusing to start: {} free, {} required. A build that runs out of space \
         does not report a disk error -- it reports link failures, a corrupt \
         incremental cache and unrelated test failures, and that verdict is then \
         cached against this tree. Reclaim space and retry.\n\
         Build artefacts in finished session worktrees and the gate cache are \
         usually the largest reclaimable sets, and the broker measures both:\n  \
         aethyme broker gc plan{gate_cache}",
        gibibytes(available),
        gibibytes(required)
    ))
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
    #[test]
    fn a_measured_gate_cache_is_named_with_its_bytes() {
        let root = std::path::Path::new("/cache/gates/abc");
        let message = refusal_with_gate_cache(
            Some(0),
            DEFAULT_GATE_HEADROOM_BYTES,
            Some((root, 7 * 1024 * 1024 * 1024 + 800 * 1024 * 1024)),
        )
        .unwrap();
        assert!(message.contains("gate cache holds 7.8 GiB"), "{message}");
        assert!(message.contains("/cache/gates/abc"), "{message}");
        assert!(message.contains("aethyme broker gc plan"), "{message}");
        // An empty cache is not worth a sentence.
        let quiet =
            refusal_with_gate_cache(Some(0), DEFAULT_GATE_HEADROOM_BYTES, Some((root, 0))).unwrap();
        assert!(!quiet.contains("gate cache holds"), "{quiet}");
    }

    #[test]
    fn the_real_filesystem_reports_something_plausible() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let available = available_bytes(here).expect("statvfs works on the checkout");
        assert!(available > 0, "a writable checkout has some free space");
    }
}
