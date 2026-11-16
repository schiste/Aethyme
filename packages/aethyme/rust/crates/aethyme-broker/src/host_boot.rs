//! The host's boot time: the one piece of evidence that every process which
//! last wrote before it has exited, whatever its record says.
//!
//! A restart kills every process, but nothing the broker persisted learns of
//! it. A durable row written before the boot by a process that is supposed to
//! move it on later is therefore provably orphaned, and a holder whose
//! recorded start predates the boot has been gone since the boot.

/// A recorded time must predate the boot by at least this much to count as
/// pre-boot. Wall-clock corrections after boot (NTP) shift recorded times by
/// seconds, so a row written moments around the boot is never judged by it.
pub(crate) const PRE_BOOT_MARGIN_MS: i64 = 60_000;

#[cfg(test)]
thread_local! {
    /// The boot time tests on this thread observe. Unit tests never read the
    /// real host boot time: unset means unknown.
    static TEST_BOOT_TIME_MS: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
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
