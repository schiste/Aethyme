//! Host-scoped shared environments owned by a ref (#1217).
//!
//! A `[[shared]]` environment (e.g. a baseline backend tracking the default
//! branch) serves every worktree whose profile `uses_shared` it, for the cost
//! of one. It is owned by a ref, not a session: session cleanup, finish and
//! the orphan sweep of a worktree never touch it. It is destroyed only by
//! `env shared destroy`, or when its declaration disappears from the default
//! branch's `environments.toml` (removal on a feature branch does not count).
//!
//! It runs from an export of the ref's commit (`git archive`, no worktree),
//! kept under `<host state>/environments/shared/`. When the ref moves it is
//! redeployed by **hold-and-swap**: the new revision is exported and its
//! `build` hooks run in that export while the old one keeps serving; then
//! park (requests to its routes are now held by the proxy), swap the recorded
//! snapshot and source in place, wake. The reservation, namespace and ports
//! are kept, so consumers' URLs never change. A build that fails leaves the
//! old revision serving untouched; a revision that fails to start is rolled
//! back. Either way the failed commit is recorded as the error. A redeploy
//! holds an exclusive host lease for the environment, so the supervisor and
//! `env shared redeploy` never swap it concurrently. A revision whose
//! component set differs from the serving one cannot be swapped in place
//! (new components would never be provisioned) and is recreated instead.
//!
//! **Blue/green (#1274).** A shared environment alternates between two
//! slots, `<name>@<ref>` and `<name>@<ref>#b`, each with its own ports and
//! namespace; `<host state>/environments/shared/<repo>/<name>/serving` names
//! the serving one. Consumers reach it through a stable alias route
//! (`<name>.<repo>.shared.<route_domain>` on the proxy), which only the
//! serving slot answers. A redeploy of an active environment brings the idle
//! slot up at the new revision (build, provision, start, health) while the
//! serving one keeps answering, then moves the pointer and the alias and
//! destroys the old slot: no request is held. When the pool has no room for
//! a second slot, the redeploy falls back to hold-and-swap above. A new slot
//! that fails is destroyed and the serving one is never touched.
//!
//! The declaration is read from the default branch. When it cannot be read
//! (Git fails, the file does not parse), nothing is destroyed; only a file
//! absent from the default branch, or a `[[shared]]` entry removed from it,
//! destroys.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::lifecycle::{Lifecycle, LifecycleError, UpOptions, export_commit, now_ms};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile, SharedSpec};
use super::store::{EnvironmentPhase, EnvironmentRecord, StoreError};
use super::{ENVIRONMENTS_FILE, parse_loaded};

pub const SHARED_DIRECTORY: &str = "shared";
/// Written into every shared export: the commit it was exported from. An
/// export carries no Git metadata, so this is how destroy proves the
/// directory is the one Aethyme made for the recorded revision.
pub const EXPORT_MARKER: &str = ".aethyme-export-commit";

/// Whether `directory` is an Aethyme-made shared export of `commit` under
/// `root`.
pub fn is_shared_export_of(root: &Path, directory: &Path, commit: &str) -> bool {
    let inside = directory
        .canonicalize()
        .ok()
        .zip(root.join(SHARED_DIRECTORY).canonicalize().ok())
        .is_some_and(|(directory, shared)| directory.starts_with(shared));
    inside
        && std::fs::read_to_string(directory.join(EXPORT_MARKER))
            .is_ok_and(|marker| marker.trim() == commit)
}
/// Present in an environment's directory while a redeploy swaps it; the
/// proxy holds requests (and never wakes the old revision) meanwhile.
pub const SWAP_MARKER: &str = "swap.in-progress";

/// A marker whose owner cannot be identified expires after this long.
const SWAP_MARKER_TTL_MS: i64 = 15 * 60 * 1000;

#[derive(Serialize, serde::Deserialize)]
struct SwapOwner {
    pid: i32,
    start_time: Option<u64>,
    at: i64,
}

/// Whether a live redeploy is swapping the environment. A marker left by a
/// redeployer that died (crash, SIGKILL, reboot) does not count, so the proxy
/// never holds requests forever behind it.
pub fn swap_in_progress(root: &Path, env_id: &str) -> bool {
    let Some(directory) = super::logs::environment_directory(root, env_id) else {
        return false;
    };
    let Ok(bytes) = std::fs::read(directory.join(SWAP_MARKER)) else {
        return false;
    };
    match serde_json::from_slice::<SwapOwner>(&bytes) {
        Ok(SwapOwner {
            pid,
            start_time: Some(start_time),
            ..
        }) => crate::gates::process_start_time(pid) == Some(start_time),
        Ok(owner) => now_ms() - owner.at < SWAP_MARKER_TTL_MS,
        Err(_) => false,
    }
}

/// Creates the swap marker and removes it when dropped, however the redeploy
/// ends.
struct SwapGuard {
    path: PathBuf,
}

impl SwapGuard {
    fn begin(root: &Path, env_id: &str) -> Result<Self, LifecycleError> {
        let directory = super::logs::environment_directory(root, env_id)
            .ok_or_else(|| io("not an environment id"))?;
        std::fs::create_dir_all(&directory).map_err(io)?;
        let path = directory.join(SWAP_MARKER);
        let pid = std::process::id() as i32;
        let owner = SwapOwner {
            pid,
            start_time: crate::gates::process_start_time(pid),
            at: now_ms(),
        };
        std::fs::write(&path, serde_json::to_vec(&owner).map_err(io)?).map_err(io)?;
        Ok(Self { path })
    }
}

impl Drop for SwapGuard {
    fn drop(&mut self) {
        crate::warn_unrecorded("remove the swap marker", std::fs::remove_file(&self.path));
    }
}
pub const DEFAULT_MIN_REDEPLOY_INTERVAL: Duration = Duration::from_secs(300);

fn io(error: impl std::fmt::Display) -> LifecycleError {
    LifecycleError::Io(error.to_string())
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::git::git_command()
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The commit a shared environment's `ref` names right now. Local refs only:
/// fetching stays with the commands that already fetch.
pub fn resolve_ref(main_root: &Path, reference: &str) -> Result<String, String> {
    let candidates: Vec<&str> = match reference {
        "default_branch" => vec![
            "origin/HEAD",
            "origin/main",
            "origin/master",
            "main",
            "master",
        ],
        "integration" => vec!["aethyme/integration"],
        other => vec![other],
    };
    for candidate in &candidates {
        if let Ok(sha) = git(
            main_root,
            &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
        ) {
            return Ok(sha);
        }
    }
    Err(format!(
        "ref {reference:?} does not resolve (tried {})",
        candidates.join(", ")
    ))
}

/// The environments file as committed at the default branch, if any.
pub fn default_branch_file(main_root: &Path) -> Option<EnvironmentsFile> {
    declared_file(main_root).ok().flatten()
}

/// The default branch's environments file: `Ok(None)` only when the branch
/// resolves and has no such file; an error when it cannot be read or parsed,
/// so callers never mistake a failure for "nothing declared".
pub fn declared_file(main_root: &Path) -> Result<Option<EnvironmentsFile>, String> {
    let sha = resolve_ref(main_root, "default_branch")?;
    let path = format!("{sha}:{ENVIRONMENTS_FILE}");
    if git(main_root, &["cat-file", "-e", &path]).is_err() {
        return Ok(None);
    }
    let text = git(main_root, &["show", &path])?;
    parse_loaded::<EnvironmentsFile>(PathBuf::from(ENVIRONMENTS_FILE), ENVIRONMENTS_FILE, &text)
        .value
        .map(Some)
        .map_err(|diagnostic| diagnostic.render())
}

fn owner_name(spec: &SharedSpec) -> String {
    format!("{}@{}", spec.name, spec.reference)
}

/// The second blue/green slot's owner suffix.
const SECOND_SLOT: &str = "#b";
/// In a shared environment's home: the serving slot's owner name.
const SERVING_FILE: &str = "serving";
/// How long a blue/green redeploy waits for pool capacity for the second
/// slot before falling back to hold-and-swap.
const BLUE_GREEN_CAPACITY_WAIT: Duration = Duration::from_secs(2);

/// The declared owner (`<name>@<ref>`) a slot's owner belongs to.
fn declared_owner(owner: &str) -> &str {
    owner.strip_suffix(SECOND_SLOT).unwrap_or(owner)
}

/// The other slot's owner name.
fn other_slot(owner: &str) -> String {
    match owner.strip_suffix(SECOND_SLOT) {
        Some(first) => first.to_string(),
        None => format!("{owner}{SECOND_SLOT}"),
    }
}

/// The stable alias host consumers use: the serving slot answers on it.
pub fn alias_host(repository: &str, name: &str, route_domain: &str) -> String {
    let repository: String = repository
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    format!(
        "{}.{repository}.shared.{route_domain}",
        name.replace('_', "-")
    )
    .to_ascii_lowercase()
}

/// Stable per (repository, name): the shared environment's identity and its
/// worktree fingerprint survive redeploys to new exports.
fn home(root: &Path, repository: &str, name: &str) -> PathBuf {
    let short: String = repository
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(48)
        .collect();
    root.join(SHARED_DIRECTORY).join(short).join(name)
}

fn export_for(root: &Path, repository: &str, name: &str, sha: &str) -> PathBuf {
    home(root, repository, name).join(&sha[..12.min(sha.len())])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Redeploy {
    Unchanged {
        commit: String,
    },
    Redeployed {
        from: Option<String>,
        to: String,
    },
    /// The new revision failed; the previous one serves again.
    RolledBack {
        failed: String,
        serving: Option<String>,
        error: String,
    },
    /// Too soon after the last redeploy.
    Deferred {
        until_ms: i64,
    },
    /// Another redeploy of this environment is running.
    InProgress,
}

impl Lifecycle {
    fn shared_spec(file: &EnvironmentsFile, name: &str) -> Result<SharedSpec, LifecycleError> {
        file.shared
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| io(format!("no [[shared]] environment named {name:?}")))
    }

    /// Export `sha` (once) and compile the shared profile from the file as
    /// committed at that revision.
    fn shared_snapshot(
        &self,
        main_root: &Path,
        repository: &str,
        name: &str,
        sha: &str,
        host: &HostEnvironmentsFile,
        owner: &str,
    ) -> Result<(CompiledEnvironment, PathBuf), LifecycleError> {
        let export = export_for(&self.root, repository, name, sha);
        // Complete only once the marker is in place: export into a staging
        // directory and rename, so an interrupted export is never reused.
        if !export.join(EXPORT_MARKER).is_file() {
            let staging = export.with_extension("staging");
            for stale in [&staging, &export] {
                if stale.exists() {
                    std::fs::remove_dir_all(stale).map_err(io)?;
                }
            }
            std::fs::create_dir_all(&staging).map_err(io)?;
            export_commit(main_root, sha, &staging).map_err(io)?;
            std::fs::write(staging.join(EXPORT_MARKER), sha).map_err(io)?;
            std::fs::rename(&staging, &export).map_err(io)?;
        }
        let text = std::fs::read_to_string(export.join(ENVIRONMENTS_FILE)).map_err(io)?;
        let file = parse_loaded::<EnvironmentsFile>(
            export.join(ENVIRONMENTS_FILE),
            ENVIRONMENTS_FILE,
            &text,
        )
        .value
        .map_err(|diagnostic| io(diagnostic.render()))?;
        let spec = Self::shared_spec(&file, name)?;
        let fingerprint = crate::console::worktree_fingerprint(&home(&self.root, repository, name));
        let mut snapshot = compile(
            &file,
            host,
            &spec.profile,
            ENVIRONMENTS_FILE,
            &CompileContext {
                repository: repository.to_string(),
                owner: EnvironmentOwner::Ref {
                    name: owner.to_string(),
                },
                worktree_fingerprint: fingerprint,
                source_commit: Some(sha.to_string()),
                source_dirty: false,
                source_digest: String::new(),
            },
        )
        .map_err(io)?;
        if let Some(idle) = spec.idle_seconds {
            snapshot.idle_seconds = idle;
        }
        Ok((snapshot, export))
    }

    /// The serving slot's owner name.
    fn serving_owner(&self, repository: &str, spec: &SharedSpec) -> String {
        let first = owner_name(spec);
        let pointer =
            std::fs::read_to_string(home(&self.root, repository, &spec.name).join(SERVING_FILE))
                .unwrap_or_default();
        if pointer.trim() == other_slot(&first) {
            other_slot(&first)
        } else {
            first
        }
    }

    fn set_serving(&self, repository: &str, name: &str, owner: &str) -> Result<(), LifecycleError> {
        let directory = home(&self.root, repository, name);
        std::fs::create_dir_all(&directory).map_err(io)?;
        let staging = directory.join(format!("{SERVING_FILE}.next"));
        std::fs::write(&staging, owner).map_err(io)?;
        std::fs::rename(&staging, directory.join(SERVING_FILE)).map_err(io)
    }

    /// Record (or with `None` remove) the alias host `env_id` answers on.
    fn set_alias(&self, env_id: &str, alias: Option<&str>) -> Result<(), LifecycleError> {
        let path = self.root.join(env_id).join(super::routes::ROUTE_ALIAS_FILE);
        match alias {
            Some(alias) => {
                std::fs::create_dir_all(self.root.join(env_id)).map_err(io)?;
                std::fs::write(&path, alias).map_err(io)
            }
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(io(error)),
                _ => Ok(()),
            },
        }
    }

    /// The serving slot's environment id for `spec`.
    fn serving_env_id(&self, repository: &str, spec: &SharedSpec) -> String {
        super::compile::environment_id(
            repository,
            &EnvironmentOwner::Ref {
                name: self.serving_owner(repository, spec),
            },
            &spec.profile,
        )
    }

    /// Destroy a slot that is not serving, if it holds anything.
    fn retire_slot(&mut self, env_id: &str) {
        if let Ok(Some(record)) = self.store.get(env_id)
            && record.phase != EnvironmentPhase::Destroyed
        {
            crate::warn_unrecorded("retire a shared environment slot", self.down(env_id, None));
        }
    }

    /// The shared environment's record, if it has been created.
    pub fn shared_record(
        &self,
        main_root: &Path,
        repository: &str,
        name: &str,
    ) -> Result<Option<EnvironmentRecord>, LifecycleError> {
        let file = default_branch_file(main_root)
            .ok_or_else(|| io("the default branch has no environments file"))?;
        let spec = Self::shared_spec(&file, name)?;
        Ok(self.store.get(&self.serving_env_id(repository, &spec))?)
    }

    /// Make sure the shared environment exists and is up: create it at its
    /// ref's current commit, or wake it.
    pub fn shared_up(
        &mut self,
        main_root: &Path,
        repository: &str,
        name: &str,
        host: &HostEnvironmentsFile,
        wait: Duration,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let file = default_branch_file(main_root)
            .ok_or_else(|| io("the default branch has no environments file"))?;
        let spec = Self::shared_spec(&file, name)?;
        let sha = resolve_ref(main_root, &spec.reference).map_err(io)?;
        let owner = self.serving_owner(repository, &spec);
        let (snapshot, export) =
            self.shared_snapshot(main_root, repository, name, &sha, host, &owner)?;
        let record = match self.store.get(&snapshot.env_id)? {
            Some(record) if record.phase.retains_worktree() => match record.phase {
                EnvironmentPhase::Parked => self.wake(&record.env_id, wait)?,
                _ => record,
            },
            _ => self.up(
                &snapshot,
                &export,
                Some(main_root),
                None,
                &UpOptions {
                    wait,
                    reconfigure: false,
                },
            )?,
        };
        let alias = alias_host(repository, name, &record.snapshot.route_domain);
        self.set_alias(&record.env_id, Some(&alias))?;
        Ok(record)
    }

    /// Bring the shared environment to its ref's current commit by
    /// hold-and-swap. Debounced by `min_redeploy_interval_seconds` unless
    /// `force`.
    pub fn shared_redeploy(
        &mut self,
        main_root: &Path,
        repository: &str,
        name: &str,
        host: &HostEnvironmentsFile,
        wait: Duration,
        force: bool,
    ) -> Result<Redeploy, LifecycleError> {
        let file = default_branch_file(main_root)
            .ok_or_else(|| io("the default branch has no environments file"))?;
        let spec = Self::shared_spec(&file, name)?;
        let sha = resolve_ref(main_root, &spec.reference).map_err(io)?;
        let env_id = self.serving_env_id(repository, &spec);
        let Some(current) = self.store.get(&env_id)? else {
            self.shared_up(main_root, repository, name, host, wait)?;
            return Ok(Redeploy::Redeployed {
                from: None,
                to: sha,
            });
        };
        // Both checks before exporting, so skipped revisions leave nothing
        // on disk.
        if current.snapshot.source.commit.as_deref() == Some(sha.as_str()) {
            return Ok(Redeploy::Unchanged { commit: sha });
        }
        let interval = spec
            .min_redeploy_interval_seconds
            .map_or(DEFAULT_MIN_REDEPLOY_INTERVAL, Duration::from_secs)
            .as_millis() as i64;
        let last = self
            .store
            .transitions(&current.env_id, 200)?
            .iter()
            .rev()
            .find(|t| {
                t.reason == "redeployed"
                    || t.reason == "redeploy rolled back"
                    || t.reason == "redeploy build failed"
            })
            .map(|t| t.at);
        if !force
            && let Some(last) = last
            && now_ms() - last < interval
        {
            return Ok(Redeploy::Deferred {
                until_ms: last + interval,
            });
        }
        let key = format!("aethyme-env-shared-redeploy:{env_id}");
        let outcome = self.exclusive(repository, &key, Duration::from_secs(5), |this| {
            this.swap_revision(main_root, repository, name, host, wait, &env_id, &sha)
        });
        match outcome {
            Some(result) => result,
            None => Ok(Redeploy::InProgress),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn swap_revision(
        &mut self,
        main_root: &Path,
        repository: &str,
        name: &str,
        host: &HostEnvironmentsFile,
        wait: Duration,
        env_id: &str,
        sha: &str,
    ) -> Result<Redeploy, LifecycleError> {
        // Re-read under the lease: another redeployer may have finished.
        let current = self
            .store
            .get(env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.to_string()))?;
        if current.snapshot.source.commit.as_deref() == Some(sha) {
            return Ok(Redeploy::Unchanged {
                commit: sha.to_string(),
            });
        }
        let current_owner = match &current.owner {
            EnvironmentOwner::Ref { name } => name.clone(),
            EnvironmentOwner::Worktree { .. } => return Err(io("not a shared environment")),
        };
        let (snapshot, export) =
            self.shared_snapshot(main_root, repository, name, sha, host, &current_owner)?;
        let previous_snapshot = current.snapshot.clone();
        let previous_export = current.worktree_root.clone().map(PathBuf::from);
        let from = previous_snapshot.source.commit.clone();
        let names = |s: &CompiledEnvironment| -> Vec<(String, String)> {
            s.components
                .iter()
                .map(|c| (c.name.clone(), format!("{:?}", c.runtime)))
                .collect()
        };
        // Build the new revision in its own export while the old one serves.
        if let Err(error) = self.build_export(&current, &snapshot, &export) {
            let message = error.to_string();
            let detail = json!({ "code": "redeploy_failed", "failed_commit": sha, "serving": from, "message": message });
            self.store.set_error(env_id, Some(&detail))?;
            self.store
                .note(env_id, "redeploy build failed", &self.actor, Some(&detail))?;
            return Ok(Redeploy::RolledBack {
                failed: sha.to_string(),
                serving: from,
                error: message,
            });
        }
        // Blue/green when the environment serves: nothing is held.
        if current.phase == EnvironmentPhase::Active
            && let Some(outcome) =
                self.blue_green(main_root, repository, name, host, &current, sha)?
        {
            return Ok(outcome);
        }
        if names(&snapshot) != names(&previous_snapshot) {
            // Components added, removed or changed runtime: an in-place swap
            // would never provision them. Recreate at the new revision.
            self.down(env_id, None)?;
            self.up(
                &snapshot,
                &export,
                Some(main_root),
                None,
                &UpOptions {
                    wait,
                    reconfigure: false,
                },
            )?;
            self.store.note(
                env_id,
                "redeployed",
                &self.actor,
                Some(&json!({ "from": from, "to": sha, "recreated": "component set changed" })),
            )?;
            remove_previous_export(previous_export, &export);
            return Ok(Redeploy::Redeployed {
                from,
                to: sha.to_string(),
            });
        }
        let was_active = current.phase == EnvironmentPhase::Active;
        let _swap = SwapGuard::begin(&self.root, env_id)?;
        if was_active {
            self.park(env_id, "redeploy")?;
        }
        // Hold: from here until the wake succeeds, the proxy holds requests to
        // this environment's routes instead of failing them.
        self.store.replace_snapshot(env_id, &snapshot, &export)?;
        if !was_active {
            // Parked: swap only; the new revision starts on its next wake.
            self.store.note(
                env_id,
                "redeployed",
                &self.actor,
                Some(&json!({ "from": from, "to": sha, "while": "parked" })),
            )?;
            remove_previous_export(previous_export, &export);
            return Ok(Redeploy::Redeployed {
                from,
                to: sha.to_string(),
            });
        }
        match self.wake(env_id, wait) {
            Ok(_) => {
                self.store.note(
                    env_id,
                    "redeployed",
                    &self.actor,
                    Some(&json!({ "from": from, "to": sha })),
                )?;
                remove_previous_export(previous_export, &export);
                Ok(Redeploy::Redeployed {
                    from,
                    to: sha.to_string(),
                })
            }
            Err(error) => {
                // Roll back to the revision that was serving, and say so only
                // if it really serves again.
                let mut message = error.to_string();
                let mut serving = None;
                if let Some(old) = &previous_export {
                    let restored = self
                        .store
                        .replace_snapshot(env_id, &previous_snapshot, old)
                        .map_err(LifecycleError::from)
                        .and_then(|_| self.wake(env_id, wait).map(|_| ()));
                    match restored {
                        Ok(()) => serving = from.clone(),
                        Err(restart) => {
                            message = format!(
                                "{message}; restarting the previous revision failed: {restart}"
                            );
                        }
                    }
                }
                let detail = json!({ "code": "redeploy_failed", "failed_commit": sha, "serving": serving, "message": message });
                self.store.set_error(env_id, Some(&detail))?;
                self.store
                    .note(env_id, "redeploy rolled back", &self.actor, Some(&detail))?;
                if serving.is_some() {
                    crate::warn_unrecorded(
                        "remove the failed shared export",
                        std::fs::remove_dir_all(&export),
                    );
                }
                Ok(Redeploy::RolledBack {
                    failed: sha.to_string(),
                    serving,
                    error: message,
                })
            }
        }
    }

    /// Bring the idle slot up at `sha` while `current` serves, then move the
    /// serving pointer and the alias to it and destroy `current`. `None` when
    /// the pool has no room for a second slot (the caller holds and swaps
    /// instead). A slot that fails to come up is destroyed; `current` is
    /// never touched.
    fn blue_green(
        &mut self,
        main_root: &Path,
        repository: &str,
        name: &str,
        host: &HostEnvironmentsFile,
        current: &EnvironmentRecord,
        sha: &str,
    ) -> Result<Option<Redeploy>, LifecycleError> {
        let EnvironmentOwner::Ref {
            name: current_owner,
        } = &current.owner
        else {
            return Ok(None);
        };
        let next_owner = other_slot(current_owner);
        let (snapshot, export) =
            self.shared_snapshot(main_root, repository, name, sha, host, &next_owner)?;
        // A slot left by an interrupted redeploy.
        self.retire_slot(&snapshot.env_id);
        let from = current.snapshot.source.commit.clone();
        let started = self.up(
            &snapshot,
            &export,
            Some(main_root),
            None,
            &UpOptions {
                wait: BLUE_GREEN_CAPACITY_WAIT,
                reconfigure: false,
            },
        );
        let next = match started {
            Ok(next) => next,
            Err(error) if error.code() == "capacity_timeout" => {
                self.retire_slot(&snapshot.env_id);
                return Ok(None);
            }
            Err(error) => {
                self.retire_slot(&snapshot.env_id);
                let message = error.to_string();
                let detail = json!({ "code": "redeploy_failed", "failed_commit": sha, "serving": from, "message": message, "strategy": "blue_green" });
                self.store.set_error(&current.env_id, Some(&detail))?;
                self.store.note(
                    &current.env_id,
                    "redeploy rolled back",
                    &self.actor,
                    Some(&detail),
                )?;
                return Ok(Some(Redeploy::RolledBack {
                    failed: sha.to_string(),
                    serving: from,
                    error: message,
                }));
            }
        };
        // Flip: the alias moves before the pointer, so the new slot answers
        // first; the old slot keeps answering its own hosts until destroyed.
        let alias = alias_host(repository, name, &next.snapshot.route_domain);
        self.set_alias(&next.env_id, Some(&alias))?;
        self.set_serving(repository, name, &next_owner)?;
        self.set_alias(&current.env_id, None)?;
        crate::warn_unrecorded(
            "destroy the previous shared slot",
            self.down(&current.env_id, None),
        );
        self.store.note(
            &next.env_id,
            "redeployed",
            &self.actor,
            Some(&json!({ "from": from, "to": sha, "strategy": "blue_green", "previous": current.env_id })),
        )?;
        remove_previous_export(current.worktree_root.clone().map(PathBuf::from), &export);
        Ok(Some(Redeploy::Redeployed {
            from,
            to: sha.to_string(),
        }))
    }

    /// Run every component's `build` hook in the new export.
    fn build_export(
        &self,
        current: &EnvironmentRecord,
        snapshot: &CompiledEnvironment,
        export: &Path,
    ) -> Result<(), LifecycleError> {
        let mut bindings = self.bindings_for(current)?;
        bindings.worktree = export.to_path_buf();
        for component in &snapshot.components {
            if let Some(build) = &component.commands.build {
                let ctx = self.context(snapshot, component, &bindings, 1);
                ctx.hook("build", build)?;
            }
        }
        Ok(())
    }

    /// Supervisor work for one repository: redeploy live shared environments
    /// whose ref moved, and destroy those no longer declared on the default
    /// branch. Returns what it did, one line per action.
    pub fn sync_shared(
        &mut self,
        main_root: &Path,
        repository: &str,
        host: &HostEnvironmentsFile,
    ) -> Result<Vec<String>, LifecycleError> {
        let declared: Vec<SharedSpec> = match declared_file(main_root) {
            Ok(file) => file.map(|file| file.shared).unwrap_or_default(),
            // Unreadable is not "nothing declared": destroy nothing.
            Err(error) => {
                return Ok(vec![format!(
                    "skipped: the default branch's {ENVIRONMENTS_FILE} cannot be read ({error}); nothing destroyed or redeployed"
                )]);
            }
        };
        let mut actions = Vec::new();
        for record in self.store.list(Some(repository))? {
            let EnvironmentOwner::Ref { name } = &record.owner else {
                continue;
            };
            if !record.phase.retains_worktree() {
                continue;
            }
            let shared = name.split('@').next().unwrap_or_default().to_string();
            let declared_name = declared_owner(name);
            let Some(spec) = declared.iter().find(|s| owner_name(s) == declared_name) else {
                match self.down(&record.env_id, None) {
                    Ok(_) => actions.push(format!(
                        "destroyed {shared}: no longer declared on the default branch"
                    )),
                    Err(error) => actions.push(format!("destroying {shared} failed: {error}")),
                }
                continue;
            };
            if *name != self.serving_owner(repository, spec) {
                // An idle slot left by an interrupted blue/green redeploy.
                // Only under the redeploy key: a redeploy in progress owns it.
                let key = format!(
                    "aethyme-env-shared-redeploy:{}",
                    self.serving_env_id(repository, spec)
                );
                let env_id = record.env_id.clone();
                if self
                    .exclusive(repository, &key, Duration::ZERO, |this| {
                        this.retire_slot(&env_id)
                    })
                    .is_some()
                {
                    actions.push(format!("retired an idle {shared} slot ({env_id})"));
                }
                continue;
            }
            if spec.profile != record.profile {
                // Its id is per profile: without this the old one would be
                // kept forever beside the new one.
                match self.down(&record.env_id, None) {
                    Ok(_) => actions.push(format!(
                        "destroyed {shared} ({}): its profile changed to {}",
                        record.profile, spec.profile
                    )),
                    Err(error) => {
                        actions.push(format!("replacing {shared} failed: {error}"));
                        continue;
                    }
                }
            }
            // One environment's failure never stops the others.
            match self.shared_redeploy(
                main_root,
                repository,
                &shared,
                host,
                Duration::from_secs(300),
                false,
            ) {
                Ok(Redeploy::Redeployed { to, .. }) => {
                    actions.push(format!("redeployed {shared} to {to}"))
                }
                Ok(Redeploy::RolledBack {
                    failed, serving, ..
                }) => actions.push(match serving {
                    Some(_) => format!(
                        "redeploy of {shared} to {failed} failed; previous revision serves"
                    ),
                    None => format!(
                        "redeploy of {shared} to {failed} failed and the previous revision did not restart"
                    ),
                }),
                Ok(Redeploy::Unchanged { .. } | Redeploy::Deferred { .. } | Redeploy::InProgress) => {}
                Err(error) => actions.push(format!("redeploy of {shared} failed: {error}")),
            }
        }
        Ok(actions)
    }

    /// Destroy a shared environment explicitly.
    pub fn shared_destroy(
        &mut self,
        main_root: &Path,
        repository: &str,
        name: &str,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let file = default_branch_file(main_root)
            .ok_or_else(|| io("the default branch has no environments file"))?;
        let spec = Self::shared_spec(&file, name)?;
        let record = self
            .shared_record(main_root, repository, name)?
            .ok_or_else(|| StoreError::NotFound(format!("shared environment {name}")))?;
        let EnvironmentOwner::Ref { name: owner } = &record.owner else {
            return Err(io("not a shared environment"));
        };
        let idle = super::compile::environment_id(
            repository,
            &EnvironmentOwner::Ref {
                name: other_slot(owner),
            },
            &spec.profile,
        );
        self.retire_slot(&idle);
        self.down(&record.env_id, None)
    }
}

fn remove_previous_export(previous: Option<PathBuf>, current: &Path) {
    if let Some(old) = previous
        && old != current
    {
        crate::warn_unrecorded(
            "remove the previous shared export",
            std::fs::remove_dir_all(&old),
        );
    }
}

/// `AETHYME_SHARED_<NAME>_URL` / `_DIRECT_URL` / `_PORT_<COMPONENT>` for a
/// consumer, from the shared environment's reservation.
///
/// With a proxy port, `_URL` goes through the wake-on-request proxy
/// (`http://<namespace>.<route_domain>:<proxy port>`), so a consumer's request
/// during a redeploy is held rather than refused, and a parked shared
/// environment is woken by the request itself (#1274). That needs the route
/// host to resolve to loopback here (`*.localhost` does on macOS and most
/// Linux resolvers); otherwise `_URL` is the direct port, as before.
/// `_DIRECT_URL` and the ports always address the component directly.
pub fn consumer_variables(
    root: &Path,
    shared_name: &str,
    record: &EnvironmentRecord,
    proxy_port: Option<u16>,
) -> Vec<(String, String)> {
    let suffix = super::validate::env_suffix(shared_name);
    let mut vars = Vec::new();
    let grant =
        super::lifecycle::read_grant(root, &record.env_id, super::lifecycle::RESERVATION_GRANT);
    let mut url = None;
    for component in &record.snapshot.components {
        let Some(port) = grant.as_ref().and_then(|grant| {
            grant
                .lease
                .allocations
                .iter()
                .find(|a| Some(&a.key) == component.port_key.as_ref())
                .map(|a| a.value.clone())
        }) else {
            continue;
        };
        vars.push((
            format!(
                "AETHYME_SHARED_{suffix}_PORT_{}",
                super::validate::env_suffix(&component.name)
            ),
            port.clone(),
        ));
        if url.is_none() || component.routed {
            url = Some(format!("http://127.0.0.1:{port}"));
        }
    }
    if let Some(url) = url {
        let proxied = proxy_port.and_then(|port| proxied_url(root, record, port));
        vars.push((format!("AETHYME_SHARED_{suffix}_DIRECT_URL"), url.clone()));
        vars.push((
            format!("AETHYME_SHARED_{suffix}_URL"),
            proxied.unwrap_or(url),
        ));
    }
    vars
}

/// The proxy URL of the shared environment's first routed component, when its
/// route host resolves to loopback on this host.
fn proxied_url(root: &Path, record: &EnvironmentRecord, proxy_port: u16) -> Option<String> {
    use std::net::ToSocketAddrs;
    record.snapshot.components.iter().find(|c| c.routed)?;
    // The alias survives blue/green flips; the namespace host is the slot's.
    let host = match super::routes::route_alias(root, &record.env_id) {
        Some(alias) => alias,
        None => format!(
            "{}.{}",
            record.namespace.as_deref()?,
            record.snapshot.route_domain
        ),
    };
    let loopback = (host.as_str(), proxy_port)
        .to_socket_addrs()
        .ok()?
        .any(|address| address.ip().is_loopback());
    loopback.then(|| format!("http://{host}:{proxy_port}"))
}
