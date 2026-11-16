//! The host's boot time: the one piece of evidence that every process which
//! last wrote before it has exited, whatever its record says.
//!
//! A restart kills every process, but nothing the broker persisted learns of
//! it. A durable row written before the boot by a process that is supposed to
//! move it on later is therefore provably orphaned, and a holder whose
//! recorded start predates the boot has been gone since the boot.

/// A recorded time must predate the boot by at least this much to count as
/// pre-boot. The boot time is wall-clock arithmetic: a clock step after boot
/// (NTP, a resumed virtual machine) moves it, so only a wide margin makes a
/// timestamp comparison conservative. A boot identifier, where one was
/// recorded, is the exact proof and needs no margin.
pub(crate) const PRE_BOOT_MARGIN_MS: i64 = 10 * 60_000;

#[cfg(test)]
thread_local! {
    /// The boot time tests on this thread observe. Unit tests never read the
    /// real host boot time: unset means unknown.
    static TEST_BOOT_TIME_MS: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
    /// The boot identifier tests on this thread observe; unset means unknown.
    static TEST_BOOT_ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Make this test thread observe `boot_id` as the current boot identifier;
/// `None` restores "unknown".
#[cfg(test)]
pub(crate) fn set_boot_id_for_tests(boot_id: Option<&str>) {
    TEST_BOOT_ID.with(|cell| *cell.borrow_mut() = boot_id.map(str::to_owned));
}

/// An identifier unique to the current boot (`kern.bootsessionuuid` on
/// macOS, `/proc/sys/kernel/random/boot_id` on Linux), or `None` when the
/// platform cannot say. Unlike the boot time it does not move with the
/// clock. Read once per process.
pub(crate) fn boot_id() -> Option<String> {
    #[cfg(test)]
    {
        let _native_reader: fn() -> Option<String> = read_boot_id;
        TEST_BOOT_ID.with(|cell| cell.borrow().clone())
    }
    #[cfg(not(test))]
    {
        static BOOT_ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        BOOT_ID.get_or_init(read_boot_id).clone()
    }
}

#[cfg(target_os = "macos")]
fn read_boot_id() -> Option<String> {
    let name = c"kern.bootsessionuuid";
    let mut buffer = [0u8; 128];
    let mut length = buffer.len();
    // SAFETY: the name is NUL-terminated; `buffer` and `length` describe
    // writable storage of `length` bytes; no new value is supplied.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    let text = buffer.get(..length)?;
    let text = text.split(|byte| *byte == 0).next()?;
    let id = std::str::from_utf8(text).ok()?.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(target_os = "linux")]
fn read_boot_id() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_boot_id() -> Option<String> {
    None
}

/// Make this test thread observe `boot_ms` as the host boot time; `None`
/// restores "unknown".
#[cfg(test)]
pub(crate) fn set_boot_time_ms_for_tests(boot_ms: Option<i64>) {
    TEST_BOOT_TIME_MS.with(|cell| cell.set(boot_ms));
}

/// The host boot time in epoch milliseconds, or `None` when the platform
/// cannot say. Read once per process: the boot time of a running process
/// cannot change.
pub(crate) fn boot_time_ms() -> Option<i64> {
    #[cfg(test)]
    {
        // Named, never called: the native reader stays in use under test.
        let _native_reader: fn() -> Option<i64> = crate::git_maintenance_lock::os_boot_time_ms;
        TEST_BOOT_TIME_MS.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        static BOOT_TIME_MS: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
        *BOOT_TIME_MS.get_or_init(crate::git_maintenance_lock::os_boot_time_ms)
    }
}

/// What this process knows about the current boot, read once per decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BootEvidence {
    /// The current boot's identifier, when the platform provides one.
    pub boot_id: Option<String>,
    /// The boot time in epoch milliseconds, when known.
    pub boot_ms: Option<i64>,
    /// [`pre_boot_cutoff_ms`]: only a record last written before this, and
    /// written without a boot identifier, may be judged by the clock.
    pub cutoff_ms: Option<i64>,
}

impl BootEvidence {
    pub(crate) fn current() -> Self {
        Self {
            boot_id: boot_id(),
            boot_ms: boot_time_ms(),
            cutoff_ms: pre_boot_cutoff_ms(),
        }
    }
}

/// Everything last written strictly before this instant (epoch ms) was
/// written before the current boot, margin included. `None` when the boot
/// time is unknown: then nothing is judged pre-boot.
pub(crate) fn pre_boot_cutoff_ms() -> Option<i64> {
    boot_time_ms()
        .filter(|boot| *boot > PRE_BOOT_MARGIN_MS)
        .map(|boot| boot - PRE_BOOT_MARGIN_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cutoff_keeps_the_margin_and_an_unknown_boot_judges_nothing() {
        set_boot_time_ms_for_tests(None);
        assert_eq!(pre_boot_cutoff_ms(), None);
        set_boot_time_ms_for_tests(Some(10 * PRE_BOOT_MARGIN_MS));
        assert_eq!(pre_boot_cutoff_ms(), Some(9 * PRE_BOOT_MARGIN_MS));
        set_boot_time_ms_for_tests(None);
    }
}
