//! Activity, idle parking and pins (#1213).
//!
//! "Has anyone used this environment in the last N minutes?" has several
//! answers, and HTTP traffic alone is the wrong one for agents: a test suite
//! talking to an environment's database sends nothing through a proxy. So
//! activity has several sources, all biased toward keeping an environment
//! awake:
//!
//! - **touch**: the mtime of `<env dir>/activity`. `env touch`, `env exec`,
//!   `env up` and (later) proxy requests update it with a `utimes`, which
//!   takes no database lock and so never contends with admission.
//! - **busy markers**: `env exec` writes `<env dir>/busy/<pid>-<start>` for the
//!   life of its command. An environment with a live marker is never idle,
//!   however old its last touch.
//! - **activity probes**: a component's `activity_probe` command exits 0
//!   when there is activity. A probe that fails to run or times out counts as
//!   active: failing toward staying up.
//!
//! The supervisor parks an `active`, unpinned environment with no live busy
//! marker whose last activity is older than its `idle_seconds`.
//!
//! **Caps (#1293).** The host file can cap both keep-awake signals:
//! `max_pin_seconds` bounds `env pin --for`, and `max_exec_busy_seconds`
//! bounds how long a running `env exec` keeps an environment busy. Past that
//! cap the command keeps running, but the environment can park, and the
//! park records `exec busy cap reached`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::identity::ProcessIdentity;

pub const ACTIVITY_FILE: &str = "activity";
pub const BUSY_DIRECTORY: &str = "busy";
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest pin `env pin --for` accepts.
pub const MAX_PIN: Duration = Duration::from_secs(24 * 3600);

fn env_dir(root: &Path, env_id: &str) -> Option<PathBuf> {
    super::logs::environment_directory(root, env_id)
}

/// Record activity now. Creates the marker file on first use.
pub fn touch(root: &Path, env_id: &str) -> std::io::Result<()> {
    let directory = env_dir(root, env_id).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "not an environment id")
    })?;
    fs::create_dir_all(&directory)?;
    let path = directory.join(ACTIVITY_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.set_modified(SystemTime::now())
}

/// Milliseconds since the epoch of the last touch, if any.
pub fn last_touch_ms(root: &Path, env_id: &str) -> Option<i64> {
    let path = env_dir(root, env_id)?.join(ACTIVITY_FILE);
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// A live command running against an environment.
pub struct BusyMarker {
    path: PathBuf,
}

impl BusyMarker {
    /// Mark `pid` busy for this environment until the marker is dropped.
    pub fn register(root: &Path, env_id: &str, pid: i32) -> std::io::Result<Self> {
        let identity = ProcessIdentity::capture(pid).ok_or_else(|| {
            std::io::Error::other(format!("process {pid} has no readable identity"))
        })?;
        let directory = env_dir(root, env_id)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not an environment id")
            })?
            .join(BUSY_DIRECTORY);
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}-{}", identity.pid, identity.start_time));
        fs::write(
            &path,
            serde_json::to_vec(&identity).map_err(std::io::Error::other)?,
        )?;
        Ok(Self { path })
    }
}

impl Drop for BusyMarker {
    fn drop(&mut self) {
        crate::warn_unrecorded("remove a busy marker", fs::remove_file(&self.path));
    }
}

/// Whether any busy marker names a command that is still running.
///
/// A marker is live exactly while its recorded pid runs with the recorded
/// start time. Only the command process matters here, not its process group:
/// `env exec` leaves the command in the caller's group so ^C reaches it, and
/// a group check would keep a marker left by a killed wrapper "unproven"
/// forever. Stale markers are removed. An unreadable marker counts as busy
/// (fail toward staying up) for at most [`MAX_PIN`], then is removed.
pub fn has_live_busy_marker(root: &Path, env_id: &str) -> bool {
    busy_state(root, env_id, now_ms(), None) == BusyState::Busy
}

/// What the busy markers say at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyState {
    Idle,
    Busy,
    /// Only commands running longer than `max_exec_busy_seconds` (#1293).
    Capped,
}

/// [`has_live_busy_marker`], with a live marker older than `cap` at `now`
/// reported as [`BusyState::Capped`] (kept: its command still runs).
pub fn busy_state(root: &Path, env_id: &str, now: i64, cap: Option<Duration>) -> BusyState {
    let Some(directory) = env_dir(root, env_id).map(|d| d.join(BUSY_DIRECTORY)) else {
        return BusyState::Idle;
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        return BusyState::Idle;
    };
    let mut busy = false;
    let mut capped = false;
    for entry in entries.flatten() {
        let identity = fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ProcessIdentity>(&bytes).ok());
        let live = match identity {
            Some(identity) => {
                crate::gates::process_start_time(identity.pid) == Some(identity.start_time)
            }
            None => entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_none_or(|age| age < MAX_PIN),
        };
        if !live {
            crate::warn_unrecorded("remove a stale busy marker", fs::remove_file(entry.path()));
            continue;
        }
        // The marker is written when the command starts.
        let started = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        match (cap, started) {
            (Some(cap), Some(started)) if now - started > cap.as_millis() as i64 => {
                capped = true;
            }
            _ => busy = true,
        }
    }
    if busy {
        BusyState::Busy
    } else if capped {
        BusyState::Capped
    } else {
        BusyState::Idle
    }
}

/// Why an environment is (not) idle, for status output and tests.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Idleness {
    Pinned {
        until: i64,
    },
    Busy,
    Probed,
    Recent {
        last_activity_at: i64,
        idle_after: i64,
    },
    Idle {
        last_activity_at: i64,
        idle_for_ms: i64,
        /// A command is still running, past `max_exec_busy_seconds`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        busy_capped: bool,
    },
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl super::lifecycle::Lifecycle {
    /// Whether an environment is idle at `now`. Probes run only when every
    /// cheaper signal says idle, so their cost is bounded to environments
    /// about to be parked; a probe that says active counts as a touch.
    pub fn idleness(
        &self,
        record: &super::store::EnvironmentRecord,
        now: i64,
        probe: bool,
    ) -> Idleness {
        if let Some(until) = record.pinned_until
            && until > now
        {
            return Idleness::Pinned { until };
        }
        let cap = super::host_file_beside(&self.root)
            .max_exec_busy_seconds
            .map(Duration::from_secs);
        let busy = busy_state(&self.root, &record.env_id, now, cap);
        if busy == BusyState::Busy {
            return Idleness::Busy;
        }
        // A shared environment is in use while any environment that uses it
        // is active (#1217).
        if let super::compile::EnvironmentOwner::Ref { name } = &record.owner {
            let shared = name.split('@').next().unwrap_or_default();
            let consumed = self
                .store
                .list(Some(&record.repository))
                .is_ok_and(|records| {
                    records.iter().any(|other| {
                        other.phase == super::store::EnvironmentPhase::Active
                            && other.snapshot.uses_shared.iter().any(|used| used == shared)
                    })
                });
            if consumed {
                return Idleness::Busy;
            }
        }
        let last = record
            .last_activity_at
            .unwrap_or(record.updated_at)
            .max(last_touch_ms(&self.root, &record.env_id).unwrap_or(0));
        let idle_after = record.snapshot.idle_seconds as i64 * 1000;
        if now - last < idle_after {
            return Idleness::Recent {
                last_activity_at: last,
                idle_after: last + idle_after,
            };
        }
        if probe && self.probe_active(record) {
            crate::warn_unrecorded("record probed activity", touch(&self.root, &record.env_id));
            return Idleness::Probed;
        }
        Idleness::Idle {
            last_activity_at: last,
            idle_for_ms: now - last,
            busy_capped: busy == BusyState::Capped,
        }
    }

    /// Run each component's `activity_probe`. Exit 0 means active; a probe
    /// that cannot run, times out or overflows counts as active too.
    fn probe_active(&self, record: &super::store::EnvironmentRecord) -> bool {
        let Ok(bindings) = self.bindings_for(record) else {
            return true;
        };
        for component in &record.snapshot.components {
            let Some(text) = &component.commands.activity_probe else {
                continue;
            };
            let vars =
                super::exec::environment_variables(&record.snapshot, component, &bindings, 1);
            let mut command =
                super::exec::command(text, &bindings.worktree, &component.pass_env, &vars);
            match crate::bounded_output::output_limited_in_owned_group(
                &mut command,
                PROBE_TIMEOUT,
                64 * 1024,
            ) {
                Ok(crate::bounded_output::LimitedOutput::Completed(output))
                    if !output.status.success() => {}
                _ => return true,
            }
        }
        false
    }

    /// Park every active environment that is idle at `now`. Returns the ids
    /// it parked; a park that fails is recorded on its environment.
    pub fn park_idle(&mut self, now: i64) -> Result<Vec<String>, super::lifecycle::LifecycleError> {
        let mut parked = Vec::new();
        for record in self.store.list(None)? {
            if record.phase != super::store::EnvironmentPhase::Active {
                continue;
            }
            if let Idleness::Idle {
                idle_for_ms,
                busy_capped,
                ..
            } = self.idleness(&record, now, true)
            {
                if busy_capped {
                    crate::warn_unrecorded(
                        "record a capped exec",
                        self.store.note(
                            &record.env_id,
                            "exec busy cap reached",
                            "supervisor",
                            Some(&serde_json::json!({
                                "max_exec_busy_seconds":
                                    super::host_file_beside(&self.root).max_exec_busy_seconds,
                            })),
                        ),
                    );
                }
                match self.park(&record.env_id, "idle") {
                    Ok(_) => {
                        parked.push(record.env_id.clone());
                        crate::warn_unrecorded(
                            "record why an environment parked",
                            self.store.note(
                                &record.env_id,
                                "parked after idle",
                                "supervisor",
                                Some(&serde_json::json!({ "idle_for_ms": idle_for_ms })),
                            ),
                        );
                    }
                    Err(error) => crate::warn_unrecorded(
                        &format!("park idle environment {}", record.env_id),
                        Err::<(), _>(error),
                    ),
                }
            }
        }
        Ok(parked)
    }

    /// Keep an environment awake until `now + duration` (at most 24 h).
    pub fn pin(
        &mut self,
        env_id: &str,
        duration: Duration,
        reason: &str,
    ) -> Result<super::store::EnvironmentRecord, super::lifecycle::LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let until = now_ms() + duration.min(MAX_PIN).as_millis() as i64;
        self.store
            .set_pin(&record.env_id, Some(until), Some(reason))?;
        self.store.note(
            &record.env_id,
            "pinned",
            &self.actor,
            Some(&serde_json::json!({ "until": until, "reason": reason })),
        )?;
        Ok(self.store.resolve(&record.env_id)?)
    }

    pub fn unpin(
        &mut self,
        env_id: &str,
    ) -> Result<super::store::EnvironmentRecord, super::lifecycle::LifecycleError> {
        let record = self.store.resolve(env_id)?;
        self.store.set_pin(&record.env_id, None, None)?;
        self.store
            .note(&record.env_id, "unpinned", &self.actor, None)?;
        Ok(self.store.resolve(&record.env_id)?)
    }
}
