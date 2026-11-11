//! Loose-object pressure in a repository's Git object store.
//!
//! Every session commit, merge trial and index write leaves loose objects in
//! the shared object directory, and Git's own automatic repack did not keep
//! up in the busiest repository: on 2026-11-10 it held 308,452 loose objects,
//! and repacking them made the object reads behind `finish` and `cleanup`
//! 8 to 40 times faster (2,000 `cat-file` lookups 1.7-2.4 s to 0.07 s). The
//! broker therefore estimates the count the way Git does, reports it in
//! status, and starts one detached non-pruning repack a day when a session
//! finishes above the threshold.

use std::path::Path;

use crate::{Broker, BrokerOpError, StatusAdvice, StatusAdviceSeverity};

/// Estimated loose objects above which status reports the store and `finish`
/// starts a repack.
pub(crate) const LOOSE_OBJECT_THRESHOLD: u64 = 25_000;

/// At most one repack is started per repository in this window.
const MAINTENANCE_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

/// When the last repack was started, in the repository's broker metadata.
const MAINTENANCE_STARTED_KEY: &str = "object_store.maintenance_started_at_ms";

/// `0` turns the automatic repack off; the status advice stays.
pub(crate) const MAINTENANCE_ENV: &str = "AETHYME_OBJECT_STORE_MAINTENANCE";

/// Estimate the loose objects under `common_dir` from one fan-out directory,
/// as `git gc --auto` does: object names are uniformly distributed, so the
/// entries of `objects/17` times 256 is the whole store within a few percent
/// at the sizes that matter, for one directory read instead of 256.
pub(crate) fn estimated_loose_objects(common_dir: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(common_dir.join("objects").join("17")).ok()?;
    let sampled = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.len() >= 38 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .count() as u64;
    Some(sampled.saturating_mul(256))
}

/// Whether a repack should be started now.
fn maintenance_due(estimate: u64, last_started_ms: Option<i64>, now_ms: i64) -> bool {
    estimate >= LOOSE_OBJECT_THRESHOLD
        && last_started_ms
            .is_none_or(|started| now_ms.saturating_sub(started) >= MAINTENANCE_INTERVAL_MS)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

fn maintenance_enabled() -> bool {
    std::env::var(MAINTENANCE_ENV).map_or(true, |value| value.trim() != "0")
}

impl Broker {
    fn loose_object_estimate(&self) -> Option<u64> {
        let _span = crate::phase_telemetry::span("object_store_probe");
        let common_dir = self.repo_handle().git_common_dir().ok()?;
        estimated_loose_objects(&common_dir)
    }

    /// Status advice naming an object store that is slowing object reads.
    pub(crate) fn object_store_advice(&self) -> Option<StatusAdvice> {
        let estimate = self.loose_object_estimate()?;
        if estimate < LOOSE_OBJECT_THRESHOLD {
            return None;
        }
        let root = self.main_root().display().to_string();
        Some(StatusAdvice {
            id: "repository.loose-objects",
            severity: StatusAdviceSeverity::Notice,
            reason: "many loose Git objects slow the object reads behind finish and cleanup",
            summary: format!(
                "about {estimate} loose Git objects (threshold {LOOSE_OBJECT_THRESHOLD}); \
                 `broker finish` starts one non-pruning repack a day, or run it now"
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![format!("estimated from {root}/.git/objects/17")],
            commands: vec![format!("git -C {} gc --no-prune", shell_quote(&root))],
        })
    }

    /// Start one detached `git gc --no-prune` when the store is over the
    /// threshold and none was started in the last day. Returns the estimate
    /// the repack was started for.
    ///
    /// `--no-prune` deletes no object, so the repack cannot remove something a
    /// concurrent session is about to reference; Git's own `gc.pid` lock makes
    /// a second concurrent repack a no-op. The start is recorded before the
    /// spawn, so a repack that cannot start is not retried by every finish.
    pub(crate) fn maybe_start_object_store_maintenance(
        &mut self,
    ) -> Result<Option<u64>, BrokerOpError> {
        if !maintenance_enabled()
            || crate::gc::background_process_is_suppressed()
            || crate::host_state::path_is_ephemeral(self.main_root())
        {
            return Ok(None);
        }
        let Some(estimate) = self.loose_object_estimate() else {
            return Ok(None);
        };
        let now = now_ms();
        let last_started = self
            .store_ref()
            .meta_get(MAINTENANCE_STARTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok());
        if !maintenance_due(estimate, last_started, now) {
            return Ok(None);
        }
        self.store()
            .meta_set(MAINTENANCE_STARTED_KEY, &now.to_string())?;
        let mut command = crate::git::git_command();
        command
            .args(["gc", "--no-prune", "--quiet"])
            .current_dir(self.main_root())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            // Its own group: the repack must outlive this command and must not
            // be signalled with it.
            command.process_group(0);
        }
        let started = command.spawn().is_ok();
        let payload = serde_json::json!({
            "estimated_loose_objects": estimate,
            "threshold": LOOSE_OBJECT_THRESHOLD,
            "started": started,
        })
        .to_string();
        self.store().append_event(
            crate::events::BROKER_OBJECT_STORE_MAINTENANCE_STARTED,
            None,
            Some(&payload),
        )?;
        Ok(started.then_some(estimate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loose(common: &Path, count: usize) {
        let dir = common.join("objects").join("17");
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..count {
            std::fs::write(dir.join(format!("{index:038x}")), b"").unwrap();
        }
    }

    #[test]
    fn the_estimate_scales_one_fan_out_directory_and_ignores_other_files() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(estimated_loose_objects(temp.path()), None);
        loose(temp.path(), 3);
        std::fs::write(temp.path().join("objects/17/tmp_obj_AbCdEf"), b"").unwrap();
        assert_eq!(estimated_loose_objects(temp.path()), Some(3 * 256));
    }

    #[test]
    fn a_repack_is_due_only_over_the_threshold_and_once_a_day() {
        let now = 10 * MAINTENANCE_INTERVAL_MS;
        assert!(!maintenance_due(LOOSE_OBJECT_THRESHOLD - 1, None, now));
        assert!(maintenance_due(LOOSE_OBJECT_THRESHOLD, None, now));
        assert!(!maintenance_due(
            LOOSE_OBJECT_THRESHOLD,
            Some(now - MAINTENANCE_INTERVAL_MS + 1),
            now
        ));
        assert!(maintenance_due(
            LOOSE_OBJECT_THRESHOLD,
            Some(now - MAINTENANCE_INTERVAL_MS),
            now
        ));
    }
}
