//! Content-addressed builds and provision templates (#1219).
//!
//! **Builds.** A component's `build` whose output is cacheable (`build_output`,
//! or `serve_dir` for `static`) and that declares `cache_inputs` is keyed by
//! an input digest: the component, its command, the content of every tracked
//! or untracked-but-not-ignored file matching `cache_inputs` (globset, like
//! rules), the values of `cache_env`, and the OS and architecture. The output
//! is stored once per (repository, component, digest) and shared by every
//! worktree and clone; a hit is materialised with a copy-on-write clone where
//! the filesystem supports it (`cp -c` on APFS, `--reflink=auto` on Linux).
//!
//! **Templates.** A component with `template` and `template_inputs` gets a
//! template per digest of those inputs. Aethyme never clones databases; it
//! guarantees the template for the digest exists before `provision` runs and
//! passes `AETHYME_ENV_TEMPLATE_ID`. Templates live outside Aethyme (in a
//! database server), so eviction runs the component's `template_destroy`.
//!
//! **Coalescing.** Both run under an exclusive host lease named by the digest
//! (`aethyme-env-build:<repo>:<component>:<digest>`), so concurrent builders
//! of one digest run it once and the others reuse the result.
//!
//! The cache lives in `<host state>/environments/cache/`, bounded by
//! [`DEFAULT_CACHE_MAX_BYTES`] (`AETHYME_ENV_CACHE_MAX_BYTES`); `env cache
//! prune` evicts least recently used entries no live environment uses. It is
//! not yet charged to the host managed-storage ledger (`broker gc plan`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use globset::{GlobBuilder, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compile::CompiledComponent;
use super::drivers::DriverContext;
use super::exec::{DriverError, append_log};
use super::lifecycle::Lifecycle;
use super::spec::Runtime;
use crate::resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HostResourceKind, HostResourceRequest,
    HostResourceRequirement,
};

pub const CACHE_DIRECTORY: &str = "cache";
pub const BUILDS: &str = "builds";
pub const TEMPLATES: &str = "templates";
pub const COMPLETE: &str = ".complete";
pub const OUTPUT: &str = "output";
pub const USES_FILE: &str = "cache-uses.json";
pub const MAX_BYTES_ENV: &str = "AETHYME_ENV_CACHE_MAX_BYTES";
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const COALESCE_WAIT: Duration = Duration::from_secs(1800);

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
    let output = Command::new("git")
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

/// The content digest of one cacheable step.
pub fn input_digest(
    worktree: &Path,
    kind: &str,
    component: &str,
    command: &str,
    patterns: &[String],
    env_names: &[String],
) -> Result<String, String> {
    let mut digest = Sha256::new();
    for part in [
        "aethyme-env-cache/v1",
        kind,
        component,
        command,
        std::env::consts::OS,
        std::env::consts::ARCH,
    ] {
        digest.update(part.as_bytes());
        digest.update(b"\0");
    }
    for path in matching_files(worktree, patterns)? {
        let bytes = std::fs::read(worktree.join(&path)).map_err(|e| format!("{path}: {e}"))?;
        digest.update(path.as_bytes());
        digest.update(b"\0");
        digest.update(Sha256::digest(&bytes));
    }
    let mut names = env_names.to_vec();
    names.sort();
    for name in names {
        digest.update(name.as_bytes());
        digest.update(b"=");
        digest.update(std::env::var(&name).unwrap_or_default().as_bytes());
        digest.update(b"\0");
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
    copy_tree(source, destination)
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
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

/// What one template entry needs to be evicted later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateMarker {
    pub template_id: String,
    pub template_destroy: String,
    /// Where the template command ran; eviction runs destroy there.
    pub cwd: PathBuf,
    pub created_at: i64,
}

/// Cache entries a live environment depends on, recorded at bring-up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheUses {
    pub entries: Vec<PathBuf>,
}

impl Lifecycle {
    pub fn cache_root(&self) -> PathBuf {
        self.root.join(CACHE_DIRECTORY)
    }

    /// Run `body` holding an exclusive host lease on `key`.
    fn coalesce<T>(
        &mut self,
        repository: &str,
        key: &str,
        body: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        let request = HostResourceRequest {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: format!("env-cache:{}:{}", &digest[..24], super::activity::now_ms()),
            repository: repository.to_string(),
            worktree_fingerprint: "environment-cache".into(),
            run_id: format!("env-cache-{}", &digest[..12]),
            ttl_seconds: COALESCE_WAIT.as_secs(),
            holder_pid: Some(std::process::id()),
            resources: vec![HostResourceRequirement {
                key: "cache".into(),
                resource: HostResourceKind::ExclusiveKey {
                    name: key.to_string(),
                },
            }],
        };
        let grant = self
            .coordinator
            .acquire_with_wait(&request, COALESCE_WAIT, |_| {})
            .map_err(|e| DriverError::Engine(format!("cache lease {key}: {e}")))?;
        let result = body(self);
        crate::warn_unrecorded(
            "release a cache lease",
            self.coordinator.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            ),
        );
        result
    }

    /// Build a component, reusing the cached output for its input digest.
    /// Returns the cache entry used, if the build is cacheable.
    pub(super) fn cached_build(
        &mut self,
        ctx: &DriverContext<'_>,
    ) -> Result<Option<PathBuf>, DriverError> {
        let component = ctx.component;
        let Some(command) = component.commands.build.clone() else {
            return Ok(None);
        };
        let Some(output) = output_of(component).filter(|_| !component.cache_inputs.is_empty())
        else {
            ctx.hook("build", &command)?;
            return Ok(None);
        };
        let worktree = ctx.bindings.worktree.clone();
        let digest = input_digest(
            &worktree,
            "build",
            &component.name,
            &command,
            &component.cache_inputs,
            &component.cache_env,
        )
        .map_err(DriverError::Unsupported)?;
        let repository = ctx.environment.repository.clone();
        let entry = self
            .cache_root()
            .join(BUILDS)
            .join(repository_dir(&repository))
            .join(&component.name)
            .join(short(&digest));
        let key = format!("aethyme-env-build:{repository}:{}:{digest}", component.name);
        let log = ctx.log();
        let target = worktree.join(output);
        self.coalesce(&repository, &key, |_| {
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
                        ctx.component.name,
                        short(&digest)
                    ),
                );
                return Ok(());
            }
            ctx.hook("build", &command)?;
            if !target.is_dir() {
                return Err(DriverError::Unsupported(format!(
                    "{}: build did not produce {output}",
                    ctx.component.name
                )));
            }
            let staging = entry.with_extension("staging");
            if staging.exists() {
                std::fs::remove_dir_all(&staging).map_err(io("clear build staging"))?;
            }
            materialize(&target, &staging.join(OUTPUT)).map_err(io("store a build"))?;
            std::fs::write(staging.join(COMPLETE), b"").map_err(io("mark a build complete"))?;
            if entry.exists() {
                std::fs::remove_dir_all(&entry).map_err(io("replace an incomplete build"))?;
            }
            std::fs::rename(&staging, &entry).map_err(io("publish a build"))?;
            append_log(
                &log,
                &format!(
                    "[aethyme] {}: build cached {}\n",
                    ctx.component.name,
                    short(&digest)
                ),
            );
            Ok(())
        })?;
        Ok(Some(entry))
    }

    /// Make sure the component's template for its current input digest
    /// exists. Returns the template id for `AETHYME_ENV_TEMPLATE_ID`.
    pub(super) fn ensure_template(
        &mut self,
        ctx: &DriverContext<'_>,
    ) -> Result<Option<(String, PathBuf)>, DriverError> {
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
            &component.name,
            &command,
            &component.template_inputs,
            &component.cache_env,
        )
        .map_err(DriverError::Unsupported)?;
        let id = short(&digest).to_string();
        let repository = ctx.environment.repository.clone();
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
        self.coalesce(&repository, &key, |_| {
            if marker.exists() {
                return Ok(());
            }
            let mut vars = ctx.vars();
            vars.insert("AETHYME_ENV_TEMPLATE_ID".into(), id.clone());
            super::exec::run_hook(
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
                cwd: worktree.clone(),
                created_at: super::activity::now_ms(),
            };
            std::fs::write(
                &marker,
                serde_json::to_vec(&record)
                    .map_err(std::io::Error::other)
                    .map_err(io("encode a template marker"))?,
            )
            .map_err(io("record a template"))?;
            Ok(())
        })?;
        Ok(Some((id, marker)))
    }

    /// Record which cache entries an environment uses, so prune keeps them.
    pub(super) fn record_cache_uses(&self, env_id: &str, entries: Vec<PathBuf>) {
        let Some(directory) = super::logs::environment_directory(&self.root, env_id) else {
            return;
        };
        let uses = CacheUses { entries };
        crate::warn_unrecorded(
            "record cache uses",
            std::fs::create_dir_all(&directory)
                .and_then(|()| serde_json::to_vec(&uses).map_err(std::io::Error::other))
                .and_then(|bytes| std::fs::write(directory.join(USES_FILE), bytes)),
        );
    }

    fn entries_in_use(&self) -> Vec<PathBuf> {
        let Ok(records) = self.store.list(None) else {
            return Vec::new();
        };
        records
            .iter()
            .filter(|r| r.phase.retains_worktree())
            .filter_map(|r| super::logs::environment_directory(&self.root, &r.env_id))
            .filter_map(|d| std::fs::read(d.join(USES_FILE)).ok())
            .filter_map(|bytes| serde_json::from_slice::<CacheUses>(&bytes).ok())
            .flat_map(|uses| uses.entries)
            .collect()
    }

    pub fn cache_entries(&self) -> Vec<CacheEntry> {
        let in_use = self.entries_in_use();
        let mut entries = Vec::new();
        let builds = self.cache_root().join(BUILDS);
        for repository in read_dirs(&builds) {
            for component in read_dirs(&repository) {
                for entry in read_dirs(&component) {
                    let last_used = std::fs::metadata(entry.join(COMPLETE))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64);
                    entries.push(CacheEntry {
                        kind: "build",
                        component: file_name(&component),
                        digest: file_name(&entry),
                        bytes: directory_bytes(&entry),
                        last_used,
                        in_use: in_use.contains(&entry),
                        path: entry,
                    });
                }
            }
        }
        let templates = self.cache_root().join(TEMPLATES);
        for repository in read_dirs(&templates) {
            for component in read_dirs(&repository) {
                for marker in std::fs::read_dir(&component)
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let path = marker.path();
                    entries.push(CacheEntry {
                        kind: "template",
                        component: file_name(&component),
                        digest: path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        bytes: 0,
                        last_used: None,
                        in_use: in_use.contains(&path),
                        path,
                    });
                }
            }
        }
        entries
    }

    /// Evict unused entries: every unused one with `all`, else least recently
    /// used builds until the cache is within its byte budget. Templates are
    /// evicted only through their `template_destroy`; one whose destroy fails
    /// or cannot run is kept and reported.
    pub fn prune_cache(&mut self, all: bool) -> PruneReport {
        let budget = std::env::var(MAX_BYTES_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CACHE_MAX_BYTES);
        let mut entries: Vec<CacheEntry> = self
            .cache_entries()
            .into_iter()
            .filter(|e| !e.in_use)
            .collect();
        entries.sort_by_key(|e| e.last_used.unwrap_or(0));
        let mut total: u64 = self.cache_entries().iter().map(|e| e.bytes).sum();
        let mut report = PruneReport::default();
        for entry in entries {
            if !all && (entry.kind == "template" || total <= budget) {
                continue;
            }
            match entry.kind {
                "template" => match evict_template(&entry.path) {
                    Ok(()) => report.evicted.push(entry.path.clone()),
                    Err(reason) => report.kept.push((entry.path.clone(), reason)),
                },
                _ => match std::fs::remove_dir_all(&entry.path) {
                    Ok(()) => {
                        total = total.saturating_sub(entry.bytes);
                        report.evicted.push(entry.path.clone());
                    }
                    Err(error) => report.kept.push((entry.path.clone(), error.to_string())),
                },
            }
        }
        report
    }
}

fn evict_template(marker: &Path) -> Result<(), String> {
    let record: TemplateMarker = std::fs::read(marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("unreadable template marker")?;
    if !record.cwd.is_dir() {
        return Err(format!(
            "template_destroy cannot run: {} no longer exists",
            record.cwd.display()
        ));
    }
    let vars = BTreeMap::from([(
        "AETHYME_ENV_TEMPLATE_ID".to_string(),
        record.template_id.clone(),
    )]);
    let mut command = super::exec::command(&record.template_destroy, &record.cwd, &[], &vars);
    let status = command.status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("template_destroy exited {status}"));
    }
    std::fs::remove_file(marker).map_err(|e| e.to_string())
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
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub evicted: Vec<PathBuf>,
    pub kept: Vec<(PathBuf, String)>,
}
