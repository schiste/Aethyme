//! Content-addressed builds and provision templates (#1219).
//!
//! **Builds.** A component's `build` whose output is cacheable (`build_output`,
//! or `serve_dir` for `static`) and that declares `cache_inputs` is keyed by
//! an input digest: the component, its command, its `env`, the values of
//! `pass_env` and `cache_env`, the content of every tracked or
//! untracked-but-not-ignored file matching `cache_inputs` (globset, like
//! rules; a deleted file, a symlink or a submodule counts by what it is), and
//! the OS and architecture. A cached build runs with exactly those inputs:
//! none of the environment's own bindings (`AETHYME_ENV_*` ports, namespace,
//! receipts), which differ per worktree and would otherwise be baked into an
//! output served to every other worktree. The output is stored once per
//! (repository, component, digest) and shared by every worktree and clone; a
//! hit is materialised with a copy-on-write clone where the filesystem
//! supports it (`cp -c` on APFS, `--reflink=auto` on Linux). The output path
//! must be a subdirectory of the worktree (validated): it is replaced on a
//! hit.
//!
//! **Templates.** A component with `template`, `template_destroy` and
//! `template_inputs` gets a template per digest of those inputs. Aethyme
//! never clones databases; it guarantees the template for the digest exists
//! before `provision` runs and passes `AETHYME_ENV_TEMPLATE_ID`. Templates
//! live outside Aethyme (in a database server), so eviction runs the
//! component's `template_destroy`, with the component's `env` and `pass_env`
//! and the hook timeout, in the cache's own directory: it must not depend on
//! a worktree, which may be long gone by then.
//!
//! **Coalescing and use.** Building, ensuring and evicting one digest all run
//! under one exclusive host key (`aethyme-env-build|template:<repository>:
//! <component>:<digest>`), so concurrent builders run it once and prune
//! never evicts an entry while it is being made. An environment records each
//! entry it uses under that key, before relying on it; prune keeps every
//! entry a live environment uses, and when it cannot tell what a live
//! environment uses, it evicts nothing.
//!
//! The cache lives in `<host state>/environments/cache/`, bounded by
//! [`DEFAULT_CACHE_MAX_BYTES`] (`AETHYME_ENV_CACHE_MAX_BYTES`); `env cache
//! prune` evicts least recently used entries no live environment uses.
//! `broker gc plan` reports a repository's part of it and proposes the builds
//! prune would evict, and `gc apply` evicts each through the same key and use
//! check, and only if nobody used it since the plan (#1275).

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use globset::{GlobBuilder, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compile::CompiledComponent;
use super::drivers::DriverContext;
use super::exec::{DriverError, append_log, run_hook};
use super::lifecycle::Lifecycle;
use super::spec::Runtime;
use super::store::EnvironmentStore;

pub const CACHE_DIRECTORY: &str = "cache";
pub const BUILDS: &str = "builds";
pub const TEMPLATES: &str = "templates";
pub const COMPLETE: &str = ".complete";
/// The entry's exclusive key, so prune can take it before evicting.
pub const KEY_FILE: &str = ".key";
pub const OUTPUT: &str = "output";
pub const USES_FILE: &str = "cache-uses.json";
pub const MAX_BYTES_ENV: &str = "AETHYME_ENV_CACHE_MAX_BYTES";
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const TEMPLATE_ID_VARIABLE: &str = "AETHYME_ENV_TEMPLATE_ID";
const COALESCE_WAIT: Duration = Duration::from_secs(1800);
/// Prune never waits long for an entry someone is using right now.
const PRUNE_WAIT: Duration = Duration::from_secs(2);
const CACHE_GC_MAX_DURATION: Duration = Duration::from_secs(3);
const CACHE_GC_MAX_USE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_GC_MAX_USE_PATHS: usize = 100_000;

fn cache_gc_deadline(deadline: Option<Instant>) -> Option<Instant> {
    let ceiling = Instant::now() + CACHE_GC_MAX_DURATION;
    Some(deadline.map_or(ceiling, |deadline| deadline.min(ceiling)))
}

fn io(what: &str) -> impl FnOnce(std::io::Error) -> DriverError + '_ {
    move |source| DriverError::Io {
        what: what.to_string(),
        source,
    }
}

fn short(digest: &str) -> &str {
    &digest[..16.min(digest.len())]
}

fn repository_dir(repository: &str) -> String {
    repository
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(48)
        .collect()
}

fn planned_build_cache_lock(
    candidate: &crate::GcEnvironmentCacheCandidate,
) -> Result<(&str, &str), String> {
    candidate.coordinated_build_lock().ok_or_else(|| {
        let code = if candidate.cache_key.is_none() {
            "environment_cache_lock_key_missing"
        } else {
            "environment_cache_lock_key_invalid"
        };
        format!(
            "{}: {code}: the exact historical build-reader lock cannot be proven; retain the entry, archive an old plan with aethyme broker gc abandon --confirm <digest>, and review a fresh aethyme broker gc plan",
            candidate.path
        )
    })
}

/// Whether `path` names a subdirectory of the worktree: relative, non-empty,
/// only normal components. `.`, `..` or an absolute path would make a cache
/// hit replace the worktree itself, or something outside it.
pub fn is_worktree_subdirectory(path: &str) -> bool {
    let path = Path::new(path);
    path.components().next().is_some()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

/// The directory a cacheable build writes, relative to the worktree.
pub fn output_of(component: &CompiledComponent) -> Option<&str> {
    component
        .build_output
        .as_deref()
        .or(match component.runtime {
            Runtime::Static => component.serve_dir.as_deref(),
            _ => None,
        })
}

/// Files under `worktree` matching `patterns`: tracked plus untracked files
/// Git does not ignore, sorted.
fn matching_files(worktree: &Path, patterns: &[String]) -> Result<Vec<String>, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("{pattern:?}: {e}"))?,
        );
    }
    let set = builder.build().map_err(|e| e.to_string())?;
    let output = crate::git::git_command()
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(worktree)
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut files: Vec<String> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| set.is_match(p.as_str()))
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// What one input path contributes: a file's content, a symlink's target, or
/// what kind of thing it is. A tracked file deleted from the working tree is
/// an input change, not an error.
fn path_fingerprint(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(b"deleted".to_vec());
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(format!("link:{}", target.display()).into_bytes());
    }
    if metadata.is_dir() {
        // A submodule gitlink or a directory matched by name.
        return Ok(b"directory".to_vec());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes).to_vec())
}

/// The content digest of one cacheable step.
pub fn input_digest(
    worktree: &Path,
    kind: &str,
    component: &CompiledComponent,
    command: &str,
    patterns: &[String],
) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut part = |bytes: &[u8]| {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    for text in [
        "aethyme-env-cache/v2",
        kind,
        &component.name,
        command,
        std::env::consts::OS,
        std::env::consts::ARCH,
    ] {
        part(text.as_bytes());
    }
    for path in matching_files(worktree, patterns)? {
        part(path.as_bytes());
        part(&path_fingerprint(&worktree.join(&path))?);
    }
    for (name, value) in &component.env {
        part(format!("env:{name}={value}").as_bytes());
    }
    let mut passed: Vec<&String> = component
        .pass_env
        .iter()
        .chain(&component.cache_env)
        .collect();
    passed.sort();
    passed.dedup();
    for name in passed {
        let value = std::env::var(name).unwrap_or_default();
        part(format!("host:{name}={value}").as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Copy a directory tree, cloning copy-on-write where the filesystem can.
pub fn materialize(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let clone = if cfg!(target_os = "macos") {
        Command::new("cp")
            .arg("-cR")
            .arg(source)
            .arg(destination)
            .status()
    } else {
        Command::new("cp")
            .arg("-R")
            .arg("--reflink=auto")
            .arg(source)
            .arg(destination)
            .status()
    };
    if clone.is_ok_and(|status| status.success()) {
        return Ok(());
    }
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    copy_tree(source, destination)
}

/// The portable fallback: symlinks are recreated, never followed.
fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
            #[cfg(not(unix))]
            std::fs::copy(entry.path(), &target)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            _ => entry.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// What one template entry needs to be evicted later, without the worktree
/// that created it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateMarker {
    pub template_id: String,
    pub template_destroy: String,
    /// The component's `env`, given to `template_destroy` as to `template`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Host variables passed through, by name (values are read at eviction).
    #[serde(default)]
    pub pass_env: Vec<String>,
    /// The exclusive key the template was made under.
    pub key: String,
    pub created_at: i64,
}

/// Cache entries a live environment depends on, recorded under each entry's
/// key as it is ensured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheUses {
    pub entries: Vec<PathBuf>,
}

impl Lifecycle {
    pub fn cache_root(&self) -> PathBuf {
        self.root.join(CACHE_DIRECTORY)
    }

    /// Run `body` holding the cache key, failing if another holder keeps it
    /// past `COALESCE_WAIT`.
    fn coalesce<T>(
        &mut self,
        repository: &str,
        key: &str,
        body: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        self.exclusive(repository, key, COALESCE_WAIT, body)
            .unwrap_or_else(|| {
                Err(DriverError::Engine(format!(
                    "cache key {key} is held by another process"
                )))
            })
    }

    /// Build a component, reusing the cached output for its input digest.
    pub(super) fn cached_build(&mut self, ctx: &DriverContext<'_>) -> Result<(), DriverError> {
        let component = ctx.component;
        let Some(command) = component.commands.build.clone() else {
            return Ok(());
        };
        let Some(output) = output_of(component).filter(|_| !component.cache_inputs.is_empty())
        else {
            ctx.hook("build", &command)?;
            return Ok(());
        };
        if !is_worktree_subdirectory(output) {
            return Err(DriverError::Unsupported(format!(
                "{}: build output {output:?} must be a subdirectory of the worktree",
                component.name
            )));
        }
        let worktree = ctx.bindings.worktree.clone();
        let digest = input_digest(
            &worktree,
            "build",
            component,
            &command,
            &component.cache_inputs,
        )
        .map_err(DriverError::Unsupported)?;
        let repository = ctx.environment.repository.clone();
        let env_id = ctx.environment.env_id.clone();
        let entry = self
            .cache_root()
            .join(BUILDS)
            .join(repository_dir(&repository))
            .join(&component.name)
            .join(short(&digest));
        let key = format!("aethyme-env-build:{repository}:{}:{digest}", component.name);
        let log = ctx.log();
        let target = worktree.join(output);
        self.coalesce(&repository, &key, |this| {
            this.add_cache_use(&env_id, &entry)?;
            if entry.join(COMPLETE).exists() {
                materialize(&entry.join(OUTPUT), &target)
                    .map_err(io("materialize a cached build"))?;
                crate::warn_unrecorded(
                    "record cache use",
                    std::fs::write(entry.join(COMPLETE), b""),
                );
                append_log(
                    &log,
                    &format!(
                        "[aethyme] {}: build cache hit {}\n",
                        component.name,
                        short(&digest)
                    ),
                );
                return Ok(());
            }
            // Only the declared inputs: no per-environment bindings.
            run_hook(
                &format!("{}: build", component.name),
                &command,
                &worktree,
                &component.pass_env,
                &component.env,
                &log,
                ctx.hook_timeout,
            )?;
            if !target.is_dir() {
                return Err(DriverError::Unsupported(format!(
                    "{}: build did not produce {output}",
                    component.name
                )));
            }
            let staging = entry.with_extension("staging");
            if staging.exists() {
                std::fs::remove_dir_all(&staging).map_err(io("clear build staging"))?;
            }
            materialize(&target, &staging.join(OUTPUT)).map_err(io("store a build"))?;
            std::fs::write(staging.join(KEY_FILE), &key).map_err(io("record a build key"))?;
            std::fs::write(staging.join(COMPLETE), b"").map_err(io("mark a build complete"))?;
            if entry.exists() {
                std::fs::remove_dir_all(&entry).map_err(io("replace an incomplete build"))?;
            }
            std::fs::rename(&staging, &entry).map_err(io("publish a build"))?;
            append_log(
                &log,
                &format!(
                    "[aethyme] {}: build cached {}\n",
                    component.name,
                    short(&digest)
                ),
            );
            Ok(())
        })
    }

    /// Make sure the component's template for its current input digest
    /// exists. Returns the template id for `AETHYME_ENV_TEMPLATE_ID`.
    pub(super) fn ensure_template(
        &mut self,
        ctx: &DriverContext<'_>,
    ) -> Result<Option<String>, DriverError> {
        let component = ctx.component;
        let (Some(command), Some(destroy)) = (
            component.commands.template.clone(),
            component.commands.template_destroy.clone(),
        ) else {
            return Ok(None);
        };
        let worktree = ctx.bindings.worktree.clone();
        let digest = input_digest(
            &worktree,
            "template",
            component,
            &command,
            &component.template_inputs,
        )
        .map_err(DriverError::Unsupported)?;
        let id = short(&digest).to_string();
        let repository = ctx.environment.repository.clone();
        let env_id = ctx.environment.env_id.clone();
        let marker = self
            .cache_root()
            .join(TEMPLATES)
            .join(repository_dir(&repository))
            .join(&component.name)
            .join(format!("{id}.json"));
        let key = format!(
            "aethyme-env-template:{repository}:{}:{digest}",
            component.name
        );
        self.coalesce(&repository, &key, |this| {
            this.add_cache_use(&env_id, &marker)?;
            if marker.exists() {
                return Ok(());
            }
            let mut vars = ctx.vars();
            vars.insert(TEMPLATE_ID_VARIABLE.into(), id.clone());
            run_hook(
                &format!("{}: template", component.name),
                &command,
                &worktree,
                &component.pass_env,
                &vars,
                &ctx.log(),
                ctx.hook_timeout,
            )?;
            if let Some(parent) = marker.parent() {
                std::fs::create_dir_all(parent).map_err(io("create the template index"))?;
            }
            let record = TemplateMarker {
                template_id: id.clone(),
                template_destroy: destroy.clone(),
                env: component.env.clone(),
                pass_env: component.pass_env.clone(),
                key: key.clone(),
                created_at: super::activity::now_ms(),
            };
            let bytes = serde_json::to_vec(&record)
                .map_err(std::io::Error::other)
                .map_err(io("encode a template marker"))?;
            std::fs::write(&marker, bytes).map_err(io("record a template"))?;
            Ok(())
        })?;
        Ok(Some(id))
    }

    /// Start an environment's use record empty: present means "known".
    pub(super) fn record_cache_uses(&self, env_id: &str, entries: Vec<PathBuf>) {
        crate::warn_unrecorded(
            "record cache uses",
            self.write_cache_uses(env_id, &CacheUses { entries }),
        );
    }

    fn write_cache_uses(&self, env_id: &str, uses: &CacheUses) -> std::io::Result<()> {
        let directory = super::logs::environment_directory(&self.root, env_id)
            .ok_or_else(|| std::io::Error::other("not an environment id"))?;
        std::fs::create_dir_all(&directory)?;
        let bytes = serde_json::to_vec(uses).map_err(std::io::Error::other)?;
        std::fs::write(directory.join(USES_FILE), bytes)
    }

    /// Record a use before relying on the entry. A failure fails the step:
    /// an unrecorded use is one prune could evict.
    fn add_cache_use(&self, env_id: &str, entry: &Path) -> Result<(), DriverError> {
        let mut uses = super::logs::environment_directory(&self.root, env_id)
            .and_then(|d| std::fs::read(d.join(USES_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice::<CacheUses>(&bytes).ok())
            .unwrap_or_default();
        if !uses.entries.iter().any(|e| e == entry) {
            uses.entries.push(entry.to_path_buf());
        }
        self.write_cache_uses(env_id, &uses)
            .map_err(io("record a cache use"))
    }

    fn entries_in_use(&self) -> Result<Vec<PathBuf>, String> {
        entries_in_use(&self.store, &self.root)
    }

    pub fn cache_entries(&self) -> Vec<CacheEntry> {
        let in_use = self.entries_in_use().unwrap_or_default();
        cache_entries(&self.cache_root(), &in_use)
    }

    /// Evict one build a reviewed `broker gc apply` proposed (#1275), as
    /// prune would: under its key, never while in use, and only if nobody
    /// used it since the plan. `Ok(false)` when it is already gone.
    pub fn evict_planned_build(
        &mut self,
        candidate: &crate::GcEnvironmentCacheCandidate,
    ) -> Result<bool, String> {
        self.evict_planned_build_with(candidate, None, || Ok(()))
    }

    fn evict_planned_build_with(
        &mut self,
        candidate: &crate::GcEnvironmentCacheCandidate,
        deadline: Option<Instant>,
        before_delete: impl FnOnce() -> Result<(), String>,
    ) -> Result<bool, String> {
        let deadline = cache_gc_deadline(deadline);
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("environment cache eviction exceeded its bounded deadline".into());
        }
        if !candidate.has_identity_bound_location() {
            return Err(format!(
                "{}: {}: saved environment-cache candidate lacks its planned filesystem identity",
                candidate.path,
                crate::retention::GC_ENVIRONMENT_CACHE_IDENTITY_MISSING
            ));
        }
        let environment_root =
            std::fs::canonicalize(&self.root).map_err(|error| error.to_string())?;
        let cache_root = environment_root.join(CACHE_DIRECTORY);
        let path = cache_root
            .join(BUILDS)
            .join(&candidate.repository)
            .join(&candidate.component)
            .join(&candidate.digest);
        if candidate.repository.is_empty()
            || candidate.component.is_empty()
            || candidate.digest.is_empty()
            || candidate.path != path.to_string_lossy().as_ref()
            || candidate.cache_root != cache_root.to_string_lossy().as_ref()
        {
            return Err(format!(
                "{}: the cache candidate has no valid identity-bound location",
                candidate.path
            ));
        }
        if !single_path_component(&candidate.repository)
            || !single_path_component(&candidate.component)
            || !single_path_component(&candidate.digest)
        {
            return Err(format!(
                "{}: not an environment build cache entry",
                candidate.path
            ));
        }
        let (lock_repository, expected_key) = planned_build_cache_lock(candidate)?;
        let wait = deadline.map_or(PRUNE_WAIT, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(PRUNE_WAIT)
        });
        if wait.is_zero() {
            return Err("environment cache eviction exceeded its bounded deadline".into());
        }
        let outcome = self.exclusive(lock_repository, expected_key, wait, |this| {
            let in_use = entries_in_use_until(&this.store, &this.root, deadline)?;
            if in_use.contains(&path) {
                return Err("in use".to_string());
            }
            let result = crate::resources::cache_gc_fs::remove_build(
                &environment_root,
                &cache_root,
                crate::resources::cache_gc_fs::CacheFsIdentity {
                    device: candidate.cache_root_device,
                    inode: candidate.cache_root_inode,
                },
                &candidate.repository,
                &candidate.component,
                &candidate.digest,
                crate::resources::cache_gc_fs::CacheFsIdentity {
                    device: candidate.entry_device,
                    inode: candidate.entry_inode,
                },
                Some(expected_key),
                candidate.last_used_at_ms,
                candidate.deletion_started,
                before_delete,
                deadline,
            )
            .map_err(|error| error.to_string())?;
            Ok(matches!(
                result,
                crate::resources::cache_gc_fs::DeleteOutcome::Removed
            ))
        });
        outcome.unwrap_or_else(|| {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                Err("environment cache eviction exceeded its bounded deadline".into())
            } else {
                Err("being built or used right now".into())
            }
        })
    }

    /// Evict one entry under its key, re-checking under it that no live
    /// environment uses it and that nobody used it since it was listed.
    fn evict_entry(&mut self, entry: &CacheEntry) -> Result<(), String> {
        let key = entry.key.clone().unwrap_or_else(|| {
            let path = entry.path.to_string_lossy();
            format!(
                "aethyme-env-cache-path:{:x}",
                Sha256::digest(path.as_bytes())
            )
        });
        let outcome = self.exclusive("environment-cache", &key, PRUNE_WAIT, |this| {
            let canonical_entry =
                std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone());
            match this.entries_in_use() {
                Ok(in_use) if in_use.contains(&canonical_entry) => {
                    return Err("in use".to_string());
                }
                Ok(_) => {}
                Err(reason) => return Err(reason),
            }
            if entry.kind == "build" && last_used(&entry.path) != entry.last_used {
                return Err("used since it was listed".to_string());
            }
            match entry.kind {
                "template" => this.evict_template(&entry.path),
                _ => std::fs::remove_dir_all(&entry.path).map_err(|e| e.to_string()),
            }
        });
        outcome.unwrap_or_else(|| Err("being built or used right now".into()))
    }

    /// Evict unused entries: every unused one with `all`, else least recently
    /// used builds until the cache is within its byte budget. Each eviction
    /// takes the entry's key and re-checks use under it. Templates are
    /// evicted only through their `template_destroy`; one whose destroy fails
    /// is kept and reported.
    pub fn prune_cache(&mut self, all: bool) -> PruneReport {
        let mut report = PruneReport::default();
        if let Err(reason) = self.entries_in_use() {
            report.refused = Some(reason);
            return report;
        }
        let budget = budget_bytes();
        let everything = self.cache_entries();
        let mut total: u64 = everything.iter().map(|e| e.bytes).sum();
        let mut candidates: Vec<CacheEntry> =
            everything.into_iter().filter(|e| !e.in_use).collect();
        candidates.sort_by_key(|e| e.last_used.unwrap_or(0));
        for entry in candidates {
            if !all && (entry.kind == "template" || total <= budget) {
                continue;
            }
            match self.evict_entry(&entry) {
                Ok(()) => {
                    total = total.saturating_sub(entry.bytes);
                    report.evicted.push(entry.path);
                }
                Err(reason) => report.kept.push((entry.path, reason)),
            }
        }
        report
    }

    fn evict_template(&self, marker: &Path) -> Result<(), String> {
        let record: TemplateMarker = std::fs::read(marker)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or("unreadable template marker")?;
        let directory = marker
            .parent()
            .ok_or("template marker without a directory")?;
        let mut vars = record.env.clone();
        vars.insert(TEMPLATE_ID_VARIABLE.into(), record.template_id.clone());
        run_hook(
            "template_destroy",
            &record.template_destroy,
            directory,
            &record.pass_env,
            &vars,
            &self.cache_root().join("prune.log"),
            self.hook_timeout,
        )
        .map_err(|error| format!("template_destroy failed: {error}"))?;
        std::fs::remove_file(marker).map_err(|e| e.to_string())
    }
}

fn single_path_component(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\0')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn budget_bytes() -> u64 {
    std::env::var(MAX_BYTES_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CACHE_MAX_BYTES)
}

/// When a build was last used: its `.complete` marker is rewritten on every hit.
fn last_used(entry: &Path) -> Option<i64> {
    std::fs::metadata(entry.join(COMPLETE))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

/// Every entry a live environment uses. Fails closed: when the store
/// cannot be read, or a live environment with cacheable components has
/// no readable use record, nobody can say what is safe to evict.
fn entries_in_use(store: &EnvironmentStore, root: &Path) -> Result<Vec<PathBuf>, String> {
    let records = store.list(None).map_err(|error| error.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for record in records
        .iter()
        .filter(|record| record.phase.retains_worktree())
    {
        if !snapshot_uses_cache(&record.snapshot) {
            continue;
        }
        let directory = root.join(&record.env_id);
        let Some(bytes) = std::fs::read(directory.join(USES_FILE)).ok() else {
            return Err(format!(
                "cannot tell which cache entries {} uses",
                record.env_id
            ));
        };
        let uses: CacheUses = serde_json::from_slice(&bytes).map_err(|error| {
            format!("cache use record for {} is invalid: {error}", record.env_id)
        })?;
        for entry in uses.entries {
            if !entry.is_absolute() {
                return Err(format!(
                    "cache use record for {} contains a non-absolute path",
                    record.env_id
                ));
            }
            match std::fs::canonicalize(&entry) {
                Ok(path) => {
                    entries.insert(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "cannot resolve cache use {} for {}: {error}",
                        entry.display(),
                        record.env_id
                    ));
                }
            }
        }
    }
    Ok(entries)
}

fn snapshot_uses_cache(snapshot: &CompiledEnvironment) -> bool {
    snapshot.components.iter().any(|component| {
        component.commands.template.is_some()
            || (component.commands.build.is_some() && !component.cache_inputs.is_empty())
    })
}

fn entries_in_use_until(
    store: &EnvironmentStore,
    root: &Path,
    deadline: Option<Instant>,
) -> Result<HashSet<PathBuf>, String> {
    let records = store
        .cache_gc_records(deadline)
        .map_err(|error| error.to_string())?;
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("cache-use inspection exceeded its bounded deadline".into());
    }
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("cache-use inspection exceeded its bounded deadline".into());
    }
    let mut entries = HashSet::new();
    let mut use_record_bytes = 0_usize;
    let mut use_paths = 0_usize;
    for record in records.iter().filter(|r| r.phase.retains_worktree()) {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("cache-use inspection exceeded its bounded deadline".into());
        }
        if !snapshot_uses_cache(&record.snapshot) {
            continue;
        }
        let directory = root.join(&record.env_id);
        let bytes = crate::resources::cache_gc_fs::read_regular_file(
            &directory,
            USES_FILE,
            1024 * 1024,
            deadline,
        )
        .map_err(|error| error.to_string())?;
        match bytes {
            Some(bytes) => {
                use_record_bytes = use_record_bytes
                    .checked_add(bytes.len())
                    .filter(|total| *total <= CACHE_GC_MAX_USE_BYTES)
                    .ok_or_else(|| "cache-use metadata byte limit exceeded".to_string())?;
                let uses: CacheUses = serde_json::from_slice(&bytes).map_err(|error| {
                    format!("cache use record for {} is invalid: {error}", record.env_id)
                })?;
                for entry in uses.entries {
                    use_paths = use_paths
                        .checked_add(1)
                        .filter(|count| *count <= CACHE_GC_MAX_USE_PATHS)
                        .ok_or_else(|| "cache-use path count limit exceeded".to_string())?;
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err("cache-use inspection exceeded its bounded deadline".into());
                    }
                    if !entry.is_absolute() {
                        return Err(format!(
                            "cache use record for {} contains a non-absolute path",
                            record.env_id
                        ));
                    }
                    let resolved = std::fs::canonicalize(&entry);
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err("cache-use inspection exceeded its bounded deadline".into());
                    }
                    match resolved {
                        Ok(path) => {
                            entries.insert(path);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!(
                                "cannot resolve cache use {} for {}: {error}",
                                entry.display(),
                                record.env_id
                            ));
                        }
                    }
                }
            }
            None => {
                return Err(format!(
                    "cannot tell which cache entries {} uses",
                    record.env_id
                ));
            }
        }
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("cache-use inspection exceeded its bounded deadline".into());
    }
    Ok(entries)
}

fn cache_entries(cache_root: &Path, in_use: &[PathBuf]) -> Vec<CacheEntry> {
    let mut entries = Vec::new();
    let builds = cache_root.join(BUILDS);
    for repository in read_dirs(&builds) {
        for component in read_dirs(&repository) {
            for entry in read_dirs(&component) {
                let last_used = last_used(&entry);
                let canonical_entry =
                    std::fs::canonicalize(&entry).unwrap_or_else(|_| entry.clone());
                entries.push(CacheEntry {
                    kind: "build",
                    component: file_name(&component),
                    digest: file_name(&entry),
                    bytes: directory_bytes(&entry),
                    last_used,
                    in_use: in_use.contains(&canonical_entry),
                    key: std::fs::read_to_string(entry.join(KEY_FILE)).ok(),
                    path: entry,
                });
            }
        }
    }
    let templates = cache_root.join(TEMPLATES);
    for repository in read_dirs(&templates) {
        for component in read_dirs(&repository) {
            for marker in std::fs::read_dir(&component)
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = marker.path();
                let canonical_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                let key = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<TemplateMarker>(&bytes).ok())
                    .map(|m| m.key);
                entries.push(CacheEntry {
                    kind: "template",
                    component: file_name(&component),
                    digest: path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    bytes: 0,
                    last_used: None,
                    in_use: in_use.contains(&canonical_path),
                    key,
                    path,
                });
            }
        }
    }
    entries
}

/// This repository's part of the cache, and the builds `env cache prune`
/// would evict from it now: unused, least recently used first, while the
/// whole cache is over `budget`. Nothing is proposed when live use cannot
/// be known. `None` when the repository has no entries.
pub fn gc_inventory(
    store: Option<&EnvironmentStore>,
    root: &Path,
    repository: &str,
    budget: u64,
    deadline: Option<Instant>,
) -> (
    Option<crate::GcEnvironmentCacheInventory>,
    Vec<crate::GcEnvironmentCacheCandidate>,
) {
    let deadline = cache_gc_deadline(deadline);
    let fallback_cache_root = root.join(CACHE_DIRECTORY);
    let fallback_repo_root = fallback_cache_root
        .join(BUILDS)
        .join(repository_dir(repository));
    let scanned = match crate::resources::cache_gc_fs::inventory(root, deadline) {
        Ok(Some(scanned)) => scanned,
        Ok(None) => return (None, Vec::new()),
        Err(error) => {
            return (
                Some(incomplete_cache_inventory(
                    &fallback_repo_root,
                    budget,
                    error.code,
                    error.to_string(),
                )),
                Vec::new(),
            );
        }
    };
    let repository_directory = repository_dir(repository);
    let mine: Vec<&crate::resources::cache_gc_fs::BuildEntry> = scanned
        .builds
        .iter()
        .filter(|entry| entry.repository == repository_directory)
        .collect();
    let template_entries = scanned
        .templates_by_repository
        .get(&repository_directory)
        .copied()
        .unwrap_or(0);
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return (
            Some(incomplete_cache_inventory(
                &scanned.cache_root.join(BUILDS).join(&repository_directory),
                budget,
                "deadline_exceeded",
                "environment cache inventory exceeded its bounded deadline".into(),
            )),
            Vec::new(),
        );
    }
    if mine.is_empty() && template_entries == 0 {
        return (None, Vec::new());
    }
    let in_use = match store.map(|store| entries_in_use_until(store, root, deadline)) {
        Some(Ok(in_use)) => in_use,
        Some(Err(reason)) => {
            let code = if reason.contains("bounded deadline") {
                "deadline_exceeded"
            } else {
                "store_unreadable"
            };
            return (
                Some(incomplete_cache_inventory(
                    &scanned.cache_root.join(BUILDS).join(&repository_directory),
                    budget,
                    code,
                    reason,
                )),
                Vec::new(),
            );
        }
        // No store: no environment has ever run, so none uses anything.
        None => HashSet::new(),
    };
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return (
            Some(incomplete_cache_inventory(
                &scanned.cache_root.join(BUILDS).join(&repository_directory),
                budget,
                "deadline_exceeded",
                "environment cache inventory exceeded its bounded deadline".into(),
            )),
            Vec::new(),
        );
    }
    let mut everything: Vec<(&crate::resources::cache_gc_fs::BuildEntry, bool)> = scanned
        .builds
        .iter()
        .map(|entry| (entry, in_use.contains(&entry.path)))
        .collect();
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return (
            Some(incomplete_cache_inventory(
                &scanned.cache_root.join(BUILDS).join(&repository_directory),
                budget,
                "deadline_exceeded",
                "environment cache inventory exceeded its bounded deadline".into(),
            )),
            Vec::new(),
        );
    }
    let mut total = scanned.host_total_bytes;
    let host_total_bytes = total;
    let mut unused: Vec<_> = everything
        .iter()
        .filter_map(|(entry, in_use)| (!*in_use).then_some(*entry))
        .collect();
    unused.sort_by_key(|entry| entry.last_used_at_ms.unwrap_or(0));
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return (
            Some(incomplete_cache_inventory(
                &scanned.cache_root.join(BUILDS).join(&repository_directory),
                budget,
                "deadline_exceeded",
                "environment cache inventory exceeded its bounded deadline".into(),
            )),
            Vec::new(),
        );
    }
    let mut candidates = Vec::new();
    let mut lock_key_refusal = None;
    for entry in unused {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return (
                Some(incomplete_cache_inventory(
                    &scanned.cache_root.join(BUILDS).join(&repository_directory),
                    budget,
                    "deadline_exceeded",
                    "environment cache inventory exceeded its bounded deadline".into(),
                )),
                Vec::new(),
            );
        }
        if total <= budget {
            break;
        }
        let candidate = crate::GcEnvironmentCacheCandidate {
            path: entry.path.to_string_lossy().into_owned(),
            repository: entry.repository.clone(),
            component: entry.component.clone(),
            digest: entry.digest.clone(),
            cache_root: scanned.cache_root.to_string_lossy().into_owned(),
            cache_root_device: scanned.cache_identity.device,
            cache_root_inode: scanned.cache_identity.inode,
            entry_device: entry.identity.device,
            entry_inode: entry.identity.inode,
            cache_key: entry.key.clone(),
            deletion_started: false,
            estimated_bytes: entry.bytes,
            estimated_inodes: Some(entry.inodes),
            last_used_at_ms: entry.last_used_at_ms,
            reason:
                "unused build, least recently used, while the environment cache is over its budget"
                    .into(),
        };
        match planned_build_cache_lock(&candidate) {
            Ok(_) => {
                total = total.saturating_sub(entry.bytes);
                if entry.repository == repository_directory {
                    candidates.push(candidate);
                }
            }
            Err(reason) if lock_key_refusal.is_none() => lock_key_refusal = Some(reason),
            Err(_) => {}
        }
    }
    let builds: Vec<_> = everything
        .drain(..)
        .filter(|(entry, _)| entry.repository == repository_directory)
        .collect();
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return (
            Some(incomplete_cache_inventory(
                &scanned.cache_root.join(BUILDS).join(&repository_directory),
                budget,
                "deadline_exceeded",
                "environment cache inventory exceeded its bounded deadline".into(),
            )),
            Vec::new(),
        );
    }
    let build_bytes = builds.iter().map(|(entry, _)| entry.bytes).sum();
    let in_use_entries = builds.iter().filter(|(_, in_use)| *in_use).count();
    let in_use_bytes = builds
        .iter()
        .filter(|(_, in_use)| *in_use)
        .map(|(entry, _)| entry.bytes)
        .sum();
    let reclaimable_bytes = candidates
        .iter()
        .map(|candidate| candidate.estimated_bytes)
        .sum();
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return (
            Some(incomplete_cache_inventory(
                &scanned.cache_root.join(BUILDS).join(&repository_directory),
                budget,
                "deadline_exceeded",
                "environment cache inventory exceeded its bounded deadline".into(),
            )),
            Vec::new(),
        );
    }
    let inventory = crate::GcEnvironmentCacheInventory {
        root: scanned
            .cache_root
            .join(BUILDS)
            .join(&repository_directory)
            .to_string_lossy()
            .into_owned(),
        complete: true,
        refusal_code: None,
        budget_bytes: budget,
        host_total_bytes: Some(host_total_bytes),
        build_entries: Some(builds.len()),
        build_bytes: Some(build_bytes),
        in_use_entries: Some(in_use_entries),
        in_use_bytes: Some(in_use_bytes),
        reclaimable_bytes: Some(reclaimable_bytes),
        template_entries: Some(template_entries),
        refused: lock_key_refusal,
    };
    (Some(inventory), candidates)
}

fn incomplete_cache_inventory(
    root: &Path,
    budget: u64,
    code: &str,
    reason: String,
) -> crate::GcEnvironmentCacheInventory {
    crate::GcEnvironmentCacheInventory {
        root: root.to_string_lossy().into_owned(),
        complete: false,
        refusal_code: Some(code.to_string()),
        budget_bytes: budget,
        host_total_bytes: None,
        build_entries: None,
        build_bytes: None,
        in_use_entries: None,
        in_use_bytes: None,
        reclaimable_bytes: None,
        template_entries: None,
        refused: Some(reason),
    }
}

/// [`gc_inventory`] for the host's own cache, reading the environment store
/// without creating or migrating it.
pub fn host_gc_inventory(
    repository: &str,
    deadline: Option<Instant>,
) -> (
    Option<crate::GcEnvironmentCacheInventory>,
    Vec<crate::GcEnvironmentCacheCandidate>,
) {
    let deadline = cache_gc_deadline(deadline);
    let Some(root) = super::logs::environments_directory() else {
        return (None, Vec::new());
    };
    match EnvironmentStore::open_default_cache_gc_read_only(deadline) {
        Ok(store) => gc_inventory(Some(&store), &root, repository, budget_bytes(), deadline),
        Err(error) => {
            let (inventory, _) = gc_inventory(None, &root, repository, budget_bytes(), deadline);
            let refusal_code = if matches!(&error, super::store::StoreError::CacheGcDeadline) {
                "deadline_exceeded"
            } else {
                "store_unreadable"
            };
            let inventory = inventory.map(|inventory| crate::GcEnvironmentCacheInventory {
                complete: false,
                refusal_code: Some(refusal_code.into()),
                host_total_bytes: None,
                build_entries: None,
                build_bytes: None,
                in_use_entries: None,
                in_use_bytes: None,
                reclaimable_bytes: None,
                template_entries: None,
                refused: Some(format!("cannot read the environment store: {error}")),
                ..inventory
            });
            (inventory, Vec::new())
        }
    }
}

/// [`Lifecycle::evict_planned_build`] against the host's own cache.
pub fn host_gc_evict(
    candidate: &crate::GcEnvironmentCacheCandidate,
    deadline: Option<Instant>,
    before_delete: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    let deadline = cache_gc_deadline(deadline);
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("environment cache eviction exceeded its bounded deadline".into());
    }
    let root =
        super::logs::environments_directory().ok_or("the host state directory is unavailable")?;
    let store =
        EnvironmentStore::open_default_cache_gc_read_only(deadline).map_err(|e| e.to_string())?;
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("environment cache eviction exceeded its bounded deadline".into());
    }
    let coordinator =
        crate::resources::HostResourceCoordinator::open_default().map_err(|e| e.to_string())?;
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("environment cache eviction exceeded its bounded deadline".into());
    }
    let mut lifecycle = Lifecycle {
        store,
        coordinator,
        root,
        holder_pid: None,
        hook_timeout: super::exec::DEFAULT_HOOK_TIMEOUT,
        actor: "broker gc apply".into(),
    };
    lifecycle.evict_planned_build_with(candidate, deadline, before_delete)
}

fn read_dirs(path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_none_or(|e| e != "staging"))
        .collect();
    dirs.sort();
    dirs
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheEntry {
    pub kind: &'static str,
    pub component: String,
    pub digest: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<i64>,
    pub in_use: bool,
    #[serde(skip)]
    pub key: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub evicted: Vec<PathBuf>,
    pub kept: Vec<(PathBuf, String)>,
    /// Set when nothing was evicted because live use could not be known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

#[cfg(test)]
mod cache_gc_lock_tests {
    use super::*;

    fn candidate(cache_key: Option<String>) -> crate::GcEnvironmentCacheCandidate {
        crate::GcEnvironmentCacheCandidate {
            path: "/host/environments/cache/builds/tmprepo/component/0123456789abcdef".into(),
            repository: "tmprepo".into(),
            component: "component".into(),
            digest: "0123456789abcdef".into(),
            cache_root: "/host/environments/cache".into(),
            cache_root_device: 1,
            cache_root_inode: 2,
            entry_device: 1,
            entry_inode: 3,
            cache_key,
            deletion_started: false,
            estimated_bytes: 1,
            estimated_inodes: Some(1),
            last_used_at_ms: Some(1),
            reason: "test".into(),
        }
    }

    #[test]
    fn gc_recovers_the_exact_historical_build_lock_from_the_key() {
        let repository = "/tmp/repo";
        let digest = format!("0123456789abcdef{}", "a".repeat(48));
        let key = format!("aethyme-env-build:{repository}:component:{digest}");

        let lock = planned_build_cache_lock(&candidate(Some(key.clone()))).unwrap();

        assert_eq!(lock, (repository, key.as_str()));
    }

    #[test]
    fn gc_refuses_cache_candidates_without_a_matching_historical_build_lock() {
        let missing = planned_build_cache_lock(&candidate(None)).unwrap_err();
        assert!(missing.contains("environment_cache_lock_key_missing"));

        let mismatched = planned_build_cache_lock(&candidate(Some(
            "aethyme-env-build:/tmp/other:component:0123456789abcdefaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        )))
        .unwrap_err();
        assert!(mismatched.contains("environment_cache_lock_key_invalid"));
    }

    #[test]
    fn gc_inventory_retains_an_entry_without_a_shared_build_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("environments");
        let entry = root
            .join(BUILDS)
            .join(repository_dir("/tmp/repo"))
            .join("component")
            .join("0123456789abcdef");
        std::fs::create_dir_all(entry.join(OUTPUT)).unwrap();
        std::fs::write(entry.join(COMPLETE), b"").unwrap();
        std::fs::write(entry.join(OUTPUT).join("app.bin"), b"payload").unwrap();

        let (inventory, candidates) = gc_inventory(None, &root, "/tmp/repo", 0, None);

        let inventory = inventory.unwrap();
        assert!(inventory.complete);
        assert_eq!(inventory.reclaimable_bytes, Some(0));
        assert!(candidates.is_empty());
        assert!(
            inventory
                .refused
                .as_deref()
                .is_some_and(|reason| reason.contains("environment_cache_lock_key_missing"))
        );
        assert!(entry.join(COMPLETE).exists());
        assert!(entry.join(OUTPUT).join("app.bin").exists());
    }
}
