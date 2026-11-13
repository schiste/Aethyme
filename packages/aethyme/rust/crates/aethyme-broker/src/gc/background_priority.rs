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

#[cfg(target_os = "macos")]
mod platform {
    use super::BackgroundPriority;

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
    use super::BackgroundPriority;

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
    use super::BackgroundPriority;

    /// No qualified call on this platform (Windows included): the worker runs
    /// at normal priority and relies on the rate limit and host lock.
    pub(super) fn lower_current_process() -> BackgroundPriority {
        BackgroundPriority::default()
    }
}
