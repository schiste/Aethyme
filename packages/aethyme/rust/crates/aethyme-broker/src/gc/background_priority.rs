//! Lowest scheduling priority for the detached cleanup worker.
//!
//! Unattended deletion should never compete with the work a person or an agent
//! is waiting on. The worker lowers its own CPU and disk I/O priority once,
//! before it opens the broker, and every child it later spawns (Git included)
//! inherits the lowered state. Each platform call is best effort: a refusal is
//! reported in telemetry and the worker continues at the rate limit alone.

/// Which priority reductions the platform accepted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackgroundPriority {
    /// CPU scheduling lowered (`PRIO_DARWIN_BG`, or `nice 19` plus
    /// `SCHED_IDLE` on Linux).
    pub cpu_lowered: bool,
    /// Disk I/O priority lowered (`IOPOL_THROTTLE`, or the Linux idle I/O
    /// class).
    pub io_lowered: bool,
}

impl BackgroundPriority {
    /// Both reductions took effect.
    pub fn lowered(self) -> bool {
        self.cpu_lowered && self.io_lowered
    }
}

/// Lower the calling process to background CPU and I/O priority. Call once,
/// from the worker's main thread, before any other thread or child exists.
pub(crate) fn lower_current_process() -> BackgroundPriority {
    platform::lower_current_process()
}

/// What the checkout trash deleter process set its priority to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleterPriority {
    /// The disk I/O policy now in effect for the process, as a label:
    /// `important` (macOS `IOPOL_IMPORTANT`, never throttled), `inherited`
    /// (left as the parent set it: Linux and other platforms) or `unchanged`
    /// (the macOS call was refused).
    pub io_policy: &'static str,
    /// macOS background CPU band (`PRIO_DARWIN_BG`) cleared; it would
    /// throttle disk I/O too.
    pub darwin_bg_cleared: bool,
    /// CPU niceness lowered instead (`nice 10`).
    pub cpu_niced: bool,
}

/// The macOS calls the trash deleter makes, as `(darwin background,
/// io policy scope, io policy, niceness)`: leave the background band, set
/// the process-scope disk policy to `IOPOL_IMPORTANT`, and lower CPU
/// niceness instead. Process scope because XNU computes a thread's
/// effective I/O tier as the most throttled of its own and its task's
/// policy: a thread cannot lift itself out of a throttled process, so the
/// deleter is a process of its own.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const TRASH_DELETER_DARWIN_BG: libc::c_int = 0;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const TRASH_DELETER_IOPOL_SCOPE_PROCESS: libc::c_int = 0;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const TRASH_DELETER_IOPOL_IMPORTANT: libc::c_int = 1;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const TRASH_DELETER_NICE: libc::c_int = 10;

/// Apply the trash deleter's priority through the three setters (the
/// platform calls, or recorders in tests); each returns whether it took.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn raise_for_trash_deleter_with(
    set_darwin_bg: impl FnOnce(libc::c_int) -> bool,
    set_io_policy: impl FnOnce(libc::c_int, libc::c_int) -> bool,
    set_nice: impl FnOnce(libc::c_int) -> bool,
) -> DeleterPriority {
    let darwin_bg_cleared = set_darwin_bg(TRASH_DELETER_DARWIN_BG);
    let io_raised = set_io_policy(
        TRASH_DELETER_IOPOL_SCOPE_PROCESS,
        TRASH_DELETER_IOPOL_IMPORTANT,
    );
    let cpu_niced = set_nice(TRASH_DELETER_NICE);
    DeleterPriority {
        io_policy: if io_raised { "important" } else { "unchanged" },
        darwin_bg_cleared,
        cpu_niced,
    }
}

/// Set the checkout trash deleter process (the worker's catch-up lane)
/// to unthrottled disk I/O on macOS. The worker it comes from runs at
/// `IOPOL_THROTTLE` and in the `PRIO_DARWIN_BG` band, under which XNU
/// delays every I/O while any normal-priority I/O hit the same disk
/// recently: measured on an external volume under concurrent installs,
/// the deleter got 2 to 274 unlinks a second while `rm -rf` got ~12,000
/// operations a second. The deleter is rate-paced, follows host pressure
/// and stops at critical memory pressure; the throttle is not what keeps
/// it gentle. Linux keeps the inherited idle I/O class.
pub(crate) fn raise_for_trash_deleter() -> DeleterPriority {
    platform::raise_for_trash_deleter()
}

/// The macOS process-scope disk policy the trash deleter drops to while
/// foreground work (a dependency install, a gate) writes to a volume it
/// deletes on: `IOPOL_THROTTLE`, under which XNU delays its I/O whenever
/// ordinary-priority I/O hit the same disk recently.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const TRASH_DELETER_IOPOL_THROTTLE: libc::c_int = 3;

/// Which disk policy the trash deleter should switch to, if any.
///
/// The deleter raises itself to `IOPOL_IMPORTANT` so it is not starved by a
/// busy disk, and that also lets it starve the foreground install beside it
/// (a 30-minute offline install used ~6 s of CPU while the catch-up removed
/// 17.7 GB on its volume). It yields back to the throttled tier while
/// foreground work runs on its volume and returns to `important` once that
/// work ends. A process whose policy was not `important` when the lap began
/// (the throttled worker, an operator's foreground drain on Linux) is never
/// switched: raising it would undo its own policy.
///
/// `Some(true)` means drop to the throttle, `Some(false)` means return to
/// `important`, `None` means leave the policy as it is.
pub(crate) fn trash_deleter_io_switch(
    base_policy: &str,
    yielding_now: bool,
    foreground_active: bool,
) -> Option<bool> {
    (base_policy == "important" && yielding_now != foreground_active).then_some(foreground_active)
}

/// Apply [`trash_deleter_io_switch`]'s answer: the throttle when `yielding`,
/// `important` otherwise. Whether the platform accepted it; always `false`
/// off macOS, where the deleter keeps its inherited idle class.
pub(crate) fn set_trash_deleter_io_yield(yielding: bool) -> bool {
    platform::set_process_io_policy(if yielding {
        TRASH_DELETER_IOPOL_THROTTLE
    } else {
        TRASH_DELETER_IOPOL_IMPORTANT
    })
}

/// Apply the retirement lane's priority through the three setters (the
/// platform calls, or recorders in tests): leave the background band, set
/// the process-scope disk policy to `IOPOL_IMPORTANT` (the policy an
/// ordinary process has), and lower CPU niceness to 10.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn raise_for_retirement_lane_with(
    set_darwin_bg: impl FnOnce(libc::c_int) -> bool,
    set_io_policy: impl FnOnce(libc::c_int, libc::c_int) -> bool,
    set_nice: impl FnOnce(libc::c_int) -> bool,
) -> DeleterPriority {
    let darwin_bg_cleared = set_darwin_bg(TRASH_DELETER_DARWIN_BG);
    let io_raised = set_io_policy(
        TRASH_DELETER_IOPOL_SCOPE_PROCESS,
        TRASH_DELETER_IOPOL_IMPORTANT,
    );
    let cpu_niced = set_nice(TRASH_DELETER_NICE);
    DeleterPriority {
        io_policy: if io_raised { "important" } else { "unchanged" },
        darwin_bg_cleared,
        cpu_niced,
    }
}

/// Set the checkout retirement lane process to an ordinary process's disk
/// priority (`IOPOL_IMPORTANT`) with reduced CPU priority on macOS. Its Git
/// children inherit the policy. Under `IOPOL_STANDARD` (tier 1) every one
/// of 16 eligibility `git status` runs hit its 60 s limit on the busiest
/// repository while agents' builds (tier 0) used the same volume, and the
/// same command in an ordinary shell finished in 0.14 s and 0.36 s on two
/// of those checkouts. One Git inspection at a time per volume and the
/// host pressure pacing bound it instead. Linux keeps the inherited class.
pub(crate) fn raise_for_retirement_lane() -> DeleterPriority {
    platform::raise_for_retirement_lane()
}

/// In a child process between `fork` and `exec` (a Git child of a
/// throttled worker): leave the macOS background band and set the child's
/// process-scope disk policy to `IOPOL_IMPORTANT`, an ordinary process's.
/// Two integer-only system calls on the child itself, safe after `fork`;
/// failures leave the inherited policy. Nothing elsewhere.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn raise_child_before_exec() {
    platform::raise_child_before_exec();
}

/// Whether a child Git process of this one should run at an ordinary disk
/// priority: only a read on behalf of a throttled process (macOS
/// `throttle`), never a Git deletion (`keeps_inherited`).
pub(crate) fn git_child_wants_ordinary_io(io_policy: &str, keeps_inherited: bool) -> bool {
    io_policy == "throttle" && !keeps_inherited
}

/// The disk I/O policy of this process, as a label for telemetry:
/// macOS `throttle`, `utility`, `standard`, `important`, `passive` or
/// `default`; Linux `idle`, `best_effort`, `realtime` or `none`;
/// `unknown` elsewhere or when it cannot be read.
pub(crate) fn current_io_policy() -> &'static str {
    platform::current_io_policy()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{BackgroundPriority, DeleterPriority};

    const IOPOL_SCOPE_PROCESS_READ: libc::c_int = 0;
    const IOPOL_TYPE_DISK_READ: libc::c_int = 0;

    unsafe extern "C" {
        fn getiopolicy_np(iotype: libc::c_int, scope: libc::c_int) -> libc::c_int;
    }

    pub(super) fn raise_for_trash_deleter() -> DeleterPriority {
        super::raise_for_trash_deleter_with(
            // SAFETY: integer-only syscall wrapper; `who = 0` is this process.
            |band| unsafe { libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, band) } == 0,
            // SAFETY: integer-only wrapper changing only this process's policy.
            |scope, policy| unsafe { setiopolicy_np(IOPOL_TYPE_DISK, scope, policy) } == 0,
            // SAFETY: integer-only syscall wrapper; `who = 0` is this process.
            |nice| unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } == 0,
        )
    }

    pub(super) fn raise_for_retirement_lane() -> DeleterPriority {
        super::raise_for_retirement_lane_with(
            // SAFETY: integer-only syscall wrapper; `who = 0` is this process.
            |band| unsafe { libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, band) } == 0,
            // SAFETY: integer-only wrapper changing only this process's policy.
            |scope, policy| unsafe { setiopolicy_np(IOPOL_TYPE_DISK, scope, policy) } == 0,
            // SAFETY: integer-only syscall wrapper; `who = 0` is this process.
            |nice| unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } == 0,
        )
    }

    pub(super) fn raise_child_before_exec() {
        // SAFETY: integer-only system calls on the calling (child) process,
        // async-signal-safe between fork and exec.
        unsafe {
            libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, super::TRASH_DELETER_DARWIN_BG);
            setiopolicy_np(
                IOPOL_TYPE_DISK,
                super::TRASH_DELETER_IOPOL_SCOPE_PROCESS,
                super::TRASH_DELETER_IOPOL_IMPORTANT,
            );
        }
    }

    pub(super) fn current_io_policy() -> &'static str {
        // SAFETY: integer-only wrapper reading this process's policy.
        match unsafe { getiopolicy_np(IOPOL_TYPE_DISK_READ, IOPOL_SCOPE_PROCESS_READ) } {
            0 => "default",
            1 => "important",
            2 => "passive",
            3 => "throttle",
            4 => "utility",
            5 => "standard",
            _ => "unknown",
        }
    }

    // `setiopolicy_np(3)` and its constants from <sys/resource.h>; the `libc`
    // crate does not declare them.
    const IOPOL_TYPE_DISK: libc::c_int = 0;
    const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
    const IOPOL_THROTTLE: libc::c_int = 3;

    unsafe extern "C" {
        fn setiopolicy_np(
            iotype: libc::c_int,
            scope: libc::c_int,
            policy: libc::c_int,
        ) -> libc::c_int;
    }

    pub(super) fn set_process_io_policy(policy: libc::c_int) -> bool {
        // SAFETY: integer-only wrapper changing only this process's policy.
        unsafe { setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, policy) == 0 }
    }

    pub(super) fn lower_current_process() -> BackgroundPriority {
        // SAFETY: plain syscall wrappers with integer arguments; `who = 0`
        // names the calling process and no memory is passed.
        let cpu_lowered =
            unsafe { libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG) } == 0;
        // SAFETY: as above; the call only changes this process's I/O policy.
        let io_lowered =
            unsafe { setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_THROTTLE) } == 0;
        BackgroundPriority {
            cpu_lowered,
            io_lowered,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{BackgroundPriority, DeleterPriority};

    const IOPRIO_CLASS_NONE: libc::c_long = 0;
    const IOPRIO_CLASS_RT: libc::c_long = 1;
    const IOPRIO_CLASS_BE: libc::c_long = 2;

    /// Linux keeps the idle I/O class it inherited: the measured
    /// starvation was macOS throttling.
    pub(super) fn raise_for_trash_deleter() -> DeleterPriority {
        DeleterPriority {
            io_policy: "inherited",
            darwin_bg_cleared: false,
            cpu_niced: false,
        }
    }

    /// As for the trash deleter: the inherited class.
    pub(super) fn raise_for_retirement_lane() -> DeleterPriority {
        raise_for_trash_deleter()
    }

    /// Linux keeps the inherited class (the throttle measured was macOS's).
    pub(super) fn raise_child_before_exec() {}

    /// Linux keeps the inherited idle class; nothing to switch.
    pub(super) fn set_process_io_policy(_policy: libc::c_int) -> bool {
        false
    }

    pub(super) fn current_io_policy() -> &'static str {
        // SAFETY: `ioprio_get` takes two integers and touches no memory.
        let value =
            unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, 0 as libc::c_long) };
        if value < 0 {
            return "unknown";
        }
        match value >> IOPRIO_CLASS_SHIFT {
            IOPRIO_CLASS_NONE => "none",
            IOPRIO_CLASS_RT => "realtime",
            IOPRIO_CLASS_BE => "best_effort",
            IOPRIO_CLASS_IDLE => "idle",
            _ => "unknown",
        }
    }

    // <linux/ioprio.h>: who = process, class = idle (data ignored).
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_IDLE: libc::c_long = 3;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;

    pub(super) fn lower_current_process() -> BackgroundPriority {
        // SAFETY: integer-only syscall wrapper; `who = 0` is the caller.
        let nice = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) } == 0;
        // SAFETY: `param` is a zero-initialised, properly aligned value that
        // outlives the call; SCHED_IDLE requires priority 0.
        let idle = unsafe {
            let param: libc::sched_param = std::mem::zeroed();
            libc::sched_setscheduler(0, libc::SCHED_IDLE, &param)
        } == 0;
        // SAFETY: `ioprio_set` takes three integers and touches no memory.
        let io_lowered = unsafe {
            libc::syscall(
                libc::SYS_ioprio_set,
                IOPRIO_WHO_PROCESS,
                0 as libc::c_long,
                IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
            )
        } == 0;
        BackgroundPriority {
            cpu_lowered: nice || idle,
            io_lowered,
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{BackgroundPriority, DeleterPriority};

    pub(super) fn raise_for_trash_deleter() -> DeleterPriority {
        DeleterPriority {
            io_policy: "inherited",
            darwin_bg_cleared: false,
            cpu_niced: false,
        }
    }

    pub(super) fn raise_for_retirement_lane() -> DeleterPriority {
        raise_for_trash_deleter()
    }

    pub(super) fn current_io_policy() -> &'static str {
        "unknown"
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(super) fn raise_child_before_exec() {}

    pub(super) fn set_process_io_policy(_policy: libc::c_int) -> bool {
        false
    }

    /// No qualified call on this platform (Windows included): the worker runs
    /// at normal priority and relies on the rate limit and host lock.
    pub(super) fn lower_current_process() -> BackgroundPriority {
        BackgroundPriority::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trash deleter leaves the macOS background band, sets the
    /// process-scope disk policy to `IOPOL_IMPORTANT` (never throttled) and
    /// lowers CPU niceness instead; a refused policy call is reported.
    #[test]
    fn the_trash_deleter_asks_for_unthrottled_process_io() {
        let calls = std::cell::RefCell::new(Vec::new());
        let priority = raise_for_trash_deleter_with(
            |band| {
                calls.borrow_mut().push(format!("darwin_bg {band}"));
                true
            },
            |scope, policy| {
                calls
                    .borrow_mut()
                    .push(format!("iopolicy scope {scope} policy {policy}"));
                true
            },
            |nice| {
                calls.borrow_mut().push(format!("nice {nice}"));
                true
            },
        );
        assert_eq!(
            calls.into_inner(),
            vec!["darwin_bg 0", "iopolicy scope 0 policy 1", "nice 10"]
        );
        assert_eq!(priority.io_policy, "important");
        assert!(priority.darwin_bg_cleared && priority.cpu_niced);
        let refused = raise_for_trash_deleter_with(|_| false, |_, _| false, |_| false);
        assert_eq!(refused.io_policy, "unchanged");
        assert!(!current_io_policy().is_empty());
    }

    /// The deleter drops to the throttle while foreground work runs on its
    /// volume and returns to `important` after; a process that was never
    /// raised is left alone in both directions.
    #[test]
    fn the_trash_deleter_yields_its_disk_only_from_important() {
        assert_eq!(
            trash_deleter_io_switch("important", false, true),
            Some(true)
        );
        assert_eq!(
            trash_deleter_io_switch("important", true, false),
            Some(false)
        );
        assert_eq!(trash_deleter_io_switch("important", true, true), None);
        assert_eq!(trash_deleter_io_switch("important", false, false), None);
        assert_eq!(trash_deleter_io_switch("throttle", false, true), None);
        assert_eq!(trash_deleter_io_switch("idle", false, true), None);
        assert_eq!(trash_deleter_io_switch("unknown", true, false), None);
    }

    /// Only a Git read is known read-only; a write, a deletion, an unknown
    /// subcommand and a bare option list keep the throttle.
    #[test]
    fn git_subcommands_are_classified_read_only_conservatively() {
        use crate::git::git_subcommand_is_read_only as read_only;
        assert!(read_only(&[
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "status",
            "-z"
        ]));
        assert!(read_only(&["-C", "/w", "rev-parse", "HEAD"]));
        assert!(read_only(&["diff", "--binary"]));
        assert!(read_only(&["worktree", "list", "--porcelain"]));
        assert!(read_only(&["config", "--get", "user.name"]));
        assert!(!read_only(&["worktree", "remove", "--force", "/w"]));
        assert!(!read_only(&["worktree", "prune"]));
        assert!(!read_only(&["commit", "-m", "x"]));
        assert!(!read_only(&["gc", "--auto"]));
        assert!(!read_only(&["config", "user.name", "x"]));
        assert!(!read_only(&["update-ref", "--stdin"]));
        assert!(!read_only(&["bundle", "create", "-", "HEAD"]));
        assert!(!read_only(&["some-new-command"]));
        assert!(!read_only(&["-c", "a=b"]));
    }

    /// A Git child runs at an ordinary disk priority only for a throttled
    /// process and never for a Git deletion.
    #[test]
    fn only_a_throttled_processes_git_reads_get_ordinary_io() {
        assert!(git_child_wants_ordinary_io("throttle", false));
        assert!(
            !git_child_wants_ordinary_io("throttle", true),
            "a deletion keeps its pace"
        );
        assert!(!git_child_wants_ordinary_io("important", false));
        assert!(!git_child_wants_ordinary_io("idle", false));
    }

    /// The retirement lane leaves the background band, asks for an ordinary
    /// process's disk policy (`IOPOL_IMPORTANT`) and lowers CPU niceness.
    #[test]
    fn the_retirement_lane_asks_for_ordinary_process_io() {
        let calls = std::cell::RefCell::new(Vec::new());
        let priority = raise_for_retirement_lane_with(
            |band| {
                calls.borrow_mut().push(format!("darwin_bg {band}"));
                true
            },
            |scope, policy| {
                calls
                    .borrow_mut()
                    .push(format!("iopolicy scope {scope} policy {policy}"));
                true
            },
            |nice| {
                calls.borrow_mut().push(format!("nice {nice}"));
                true
            },
        );
        assert_eq!(
            calls.into_inner(),
            vec!["darwin_bg 0", "iopolicy scope 0 policy 1", "nice 10"]
        );
        assert_eq!(priority.io_policy, "important");
        let refused = raise_for_retirement_lane_with(|_| false, |_, _| false, |_| false);
        assert_eq!(refused.io_policy, "unchanged");
    }
}
