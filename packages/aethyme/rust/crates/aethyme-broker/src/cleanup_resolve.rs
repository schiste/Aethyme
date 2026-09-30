//! Resolving closed worktrees that cleanup will not remove on its own.
//!
//! A closed session whose worktree is `dirty` (uncommitted or untracked
//! changes), holds `pending_commits`, or whose work cannot be proved to have
//! landed stays on disk forever: cleanup refuses it, correctly, because
//! removing it could lose work. The only other exit was `cleanup <id> --force`,
//! which discards. What operators did instead was build recovery kits by hand
//! -- one repository accumulated 2 GiB of them, and 11 dirty plus 8 pending
//! worktrees in another sat untouched because nobody wanted to be the one to
//! discard them.
//!
//! This is the reviewed middle path. `finish cleanup resolve <id> --archive`
//! prints what would be preserved and a digest; confirming that digest writes
//! a recovery archive (a bundle of the commits no delivery target holds, the
//! staged and unstaged patches, a copy of the untracked files Git does not
//! ignore, a manifest and restore steps), reads every part of it back, and only
//! then removes the worktree through the ordinary cleanup path. A failed
//! verification leaves the worktree exactly where it was.
//!
//! Ignored files are deliberately not archived: they are build output and
//! caches by construction, which is the bulk of every worktree's size and
//! nothing anyone needs back.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::error::BrokerError;
use crate::git::GitRepo;
use crate::types::SessionOrigin;
use crate::{Broker, BrokerOpError, CleanupDisposition};

pub const CLEANUP_RESOLVE_SCHEMA_VERSION: u32 = 1;

/// Directory beneath the host state directory holding one subdirectory of
/// archives per repository.
const RECOVERY_ARCHIVE_DIRECTORY: &str = "recovery-archives";
const BUNDLE_FILE: &str = "commits.bundle";
const STAGED_PATCH_FILE: &str = "staged.patch";
const UNSTAGED_PATCH_FILE: &str = "unstaged.patch";
const UNTRACKED_DIRECTORY: &str = "untracked";
const MANIFEST_FILE: &str = "manifest.json";
const README_FILE: &str = "README.md";

/// One untracked path the archive preserves.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ArchivedFile {
    /// Relative to the worktree root.
    pub path: String,
    pub bytes: u64,
    /// SHA-256 of the file's content, or of `symlink\0<target>` for a link.
    pub sha256: String,
    /// A symbolic link is archived as the link, never as what it points at.
    pub symlink: bool,
}

/// What a confirmed resolve would preserve, and the digest that authorizes it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolvePlan {
    pub schema_version: u32,
    /// Binds the head, the delivery targets and the exact content of every
    /// patch and untracked file, so any change after review refuses the apply.
    pub digest: String,
    pub session_id: i64,
    pub worktree_path: String,
    pub branch_ref: String,
    pub disposition: CleanupDisposition,
    /// Why cleanup alone refuses this worktree.
    pub reason: String,
    pub head: String,
    /// The commits cleanup judges against; the bundle excludes all of them.
    pub delivery_targets: Vec<String>,
    /// The shared landed-work verdict for `head` against the primary checkout.
    pub landing: crate::LandingVerdict,
    /// Commits no delivery target holds, newest first. Empty means no bundle.
    pub unlanded_commits: Vec<String>,
    pub staged_patch_bytes: u64,
    pub staged_patch_sha256: String,
    pub unstaged_patch_bytes: u64,
    pub unstaged_patch_sha256: String,
    pub untracked_files: Vec<ArchivedFile>,
    pub untracked_bytes: u64,
    /// The directory the archive will be written beneath.
    pub archive_root: PathBuf,
    pub apply_command: String,
    /// The existing discard path, named so the choice is explicit.
    pub discard_command: String,
}

/// What a confirmed resolve did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolveOutcome {
    pub plan: CleanupResolvePlan,
    /// The verified archive. It is kept even when the removal below fails.
    pub archive: PathBuf,
    pub worktree_removed: bool,
    /// Why removal failed after the archive was verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
}

/// The part of a plan the digest covers. Sizes are derivable from the hashes;
/// the archive root and commands are presentation.
#[derive(serde::Serialize)]
struct DigestInput<'a> {
    schema_version: u32,
    session_id: i64,
    worktree_path: &'a str,
    disposition: &'a str,
    head: &'a str,
    delivery_targets: &'a [String],
    unlanded_commits: &'a [String],
    staged_patch_sha256: &'a str,
    unstaged_patch_sha256: &'a str,
    untracked_files: &'a [ArchivedFile],
}

#[derive(serde::Serialize)]
struct PatchRecord<'a> {
    file: &'static str,
    bytes: u64,
    sha256: &'a str,
}

/// `manifest.json`: everything needed to tell what the archive holds and
/// whether it is intact, without the broker database that produced it.
#[derive(serde::Serialize)]
struct RecoveryManifest<'a> {
    schema_version: u32,
    created_at_ms: i64,
    aethyme_version: &'static str,
    repository: String,
    session_id: i64,
    task: Option<&'a str>,
    worktree_path: &'a str,
    branch_ref: &'a str,
    disposition: &'a str,
    head: &'a str,
    /// Where `head` diverged from the primary checkout, when Git can say.
    base: Option<String>,
    delivery_targets: &'a [String],
    landing: &'a crate::LandingVerdict,
    bundle: Option<&'static str>,
    unlanded_commits: &'a [String],
    staged_patch: PatchRecord<'a>,
    unstaged_patch: PatchRecord<'a>,
    untracked_directory: &'static str,
    untracked_files: &'a [ArchivedFile],
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn io_error(path: &Path, source: std::io::Error) -> BrokerOpError {
    BrokerError::Io {
        path: path.to_path_buf(),
        source,
    }
    .into()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Hash what an untracked path is, without following a link.
fn describe_untracked(full: &Path, relative: &str) -> std::io::Result<Option<ArchivedFile>> {
    let metadata = std::fs::symlink_metadata(full)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(full)?;
        let mut hashed = b"symlink\0".to_vec();
        hashed.extend_from_slice(target.as_os_str().as_encoded_bytes());
        return Ok(Some(ArchivedFile {
            path: relative.to_owned(),
            bytes: 0,
            sha256: sha256_hex(&hashed),
            symlink: true,
        }));
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(full)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok(Some(ArchivedFile {
        path: relative.to_owned(),
        bytes,
        sha256: format!("{:x}", hasher.finalize()),
        symlink: false,
    }))
}

/// A relative path that stays inside the directory it is joined to.
fn is_contained_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

impl Broker {
    /// Where recovery archives for this repository live: host state, beside
    /// the worktrees, so an archive outlives both the worktree and a deleted
    /// clone. A repository under the system temp directory (a test fixture or
    /// scratch clone) keeps them in its own `.aethyme/` instead, by the same
    /// rule that keeps its worktrees out of durable host state.
    pub(crate) fn recovery_archive_root(&self) -> Result<PathBuf, BrokerOpError> {
        let key = self.worktree_root_plan()?.repository_key;
        let durable = crate::host_state::default_host_state_dir().filter(|_| {
            crate::host_state::host_state_dir_is_explicit()
                || !crate::host_state::path_is_ephemeral(self.main_root())
        });
        Ok(match durable {
            Some(base) => base.join(RECOVERY_ARCHIVE_DIRECTORY).join(key),
            None => self
                .main_root()
                .join(".aethyme")
                .join(RECOVERY_ARCHIVE_DIRECTORY),
        })
    }

    /// Plan preserving one closed session's worktree before removing it.
    /// Read-only: nothing is written or removed.
    pub fn cleanup_resolve_plan(
        &self,
        session_id: i64,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        let refuse = |reason: String| BrokerOpError::CleanupResolveRefused {
            id: session_id,
            reason,
        };
        let session = self.store_ref().session(session_id)?;
        if !session.status.is_closed() {
            return Err(refuse(format!(
                "the session is still {}; close it first with `aethyme broker finish close --session {session_id}`",
                session.status.as_str()
            )));
        }
        if session.origin != SessionOrigin::Spawned {
            return Err(refuse(
                "only broker-spawned worktrees are archived and removed; an adopted checkout \
                 belongs to whoever adopted it"
                    .into(),
            ));
        }
        let mut records = crate::measurement::SizeRecords::default();
        let Some(item) =
            self.cleanup_item_scanned(&session, crate::SizeScan::Recorded, &mut records)?
        else {
            return Err(refuse(
                "nothing is retained: its worktree and branch are already gone".into(),
            ));
        };
        match item.disposition {
            CleanupDisposition::Eligible => {
                return Err(refuse(format!(
                    "cleanup can already remove it without an archive ({}); run \
                     `aethyme broker finish cleanup {session_id}`",
                    item.reason
                )));
            }
            CleanupDisposition::UnsafePath | CleanupDisposition::InspectionFailed => {
                return Err(refuse(format!(
                    "its worktree is {} ({}), which resolve does not archive",
                    item.disposition.as_str(),
                    item.reason
                )));
            }
            CleanupDisposition::Dirty
            | CleanupDisposition::PendingCommits
            | CleanupDisposition::UnprovenProvenance => {}
        }
        let worktree = PathBuf::from(&session.worktree_path);
        if !item.worktree_present || crate::broker::is_orphaned_worktree_directory(&worktree) {
            return Err(refuse(
                "its worktree is not a readable Git checkout, so there is nothing to archive \
                 from; the session branch still records its commits"
                    .into(),
            ));
        }
        self.refuse_live_checkout(session_id, &worktree)?;

        let checkout = GitRepo::discover(&worktree)?;
        let head = checkout.head_commit()?;
        let delivery_targets = self.cleanup_delivery_targets()?;
        let unlanded_commits = checkout.commits_not_on_any(&head, &delivery_targets)?;
        let primary = self.repo_handle().head_commit()?;
        let landing = crate::work_landed(self.repo_handle(), &head, &primary)?;
        let staged = checkout.recovery_patch(true)?;
        let unstaged = checkout.recovery_patch(false)?;

        let mut untracked_files = Vec::new();
        for relative in checkout.untracked_paths()? {
            if relative.ends_with('/') {
                return Err(refuse(format!(
                    "{relative} is a nested repository, which an archive of this checkout \
                     cannot preserve; move or remove it by hand first"
                )));
            }
            if !is_contained_relative(&relative) {
                return Err(refuse(format!(
                    "Git reported an untracked path outside the worktree: {relative:?}"
                )));
            }
            let full = worktree.join(&relative);
            match describe_untracked(&full, &relative).map_err(|error| io_error(&full, error))? {
                Some(file) => untracked_files.push(file),
                None => {
                    return Err(refuse(format!(
                        "{relative} is neither a regular file nor a symbolic link, so it \
                         cannot be archived"
                    )));
                }
            }
        }
        untracked_files.sort_by(|left, right| left.path.cmp(&right.path));
        let untracked_bytes = untracked_files.iter().map(|file| file.bytes).sum();

        let staged_patch_sha256 = sha256_hex(&staged);
        let unstaged_patch_sha256 = sha256_hex(&unstaged);
        let digest_input = DigestInput {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            session_id,
            worktree_path: &session.worktree_path,
            disposition: item.disposition.as_str(),
            head: &head,
            delivery_targets: &delivery_targets,
            unlanded_commits: &unlanded_commits,
            staged_patch_sha256: &staged_patch_sha256,
            unstaged_patch_sha256: &unstaged_patch_sha256,
            untracked_files: &untracked_files,
        };
        let mut hasher = Sha256::new();
        hasher.update(b"aethyme-cleanup-resolve-v1\0");
        hasher
            .update(serde_json::to_vec(&digest_input).map_err(|error| refuse(error.to_string()))?);
        let digest = format!("{:x}", hasher.finalize());

        Ok(CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            apply_command: format!(
                "aethyme broker finish cleanup resolve {session_id} --archive --confirm {digest}"
            ),
            discard_command: item.force_cleanup_command.clone(),
            digest,
            session_id,
            worktree_path: session.worktree_path.clone(),
            branch_ref: item.branch_ref,
            disposition: item.disposition,
            reason: item.reason,
            head,
            delivery_targets,
            landing,
            unlanded_commits,
            staged_patch_bytes: staged.len() as u64,
            staged_patch_sha256,
            unstaged_patch_bytes: unstaged.len() as u64,
            unstaged_patch_sha256,
            untracked_files,
            untracked_bytes,
            archive_root: self.recovery_archive_root()?,
        })
    }

    /// Apply a reviewed resolve plan: write the archive, verify it by reading
    /// it back, and only then remove the worktree through ordinary cleanup.
    pub fn cleanup_resolve_apply(
        &mut self,
        session_id: i64,
        confirm: &str,
    ) -> Result<CleanupResolveOutcome, BrokerOpError> {
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::CleanupResolveConfirmationNotSha256);
        }
        let plan = self.cleanup_resolve_plan(session_id)?;
        if plan.digest != confirm {
            return Err(BrokerOpError::CleanupResolveConfirmationMismatch {
                id: session_id,
                actual: confirm.to_owned(),
            });
        }
        let session = self.store_ref().session(session_id)?;
        let name = format!("{session_id}-{}-{}", &plan.head[..12], now_ms());
        let archive = plan.archive_root.join(&name);
        let partial = plan.archive_root.join(format!("{name}.partial"));
        std::fs::create_dir_all(&partial).map_err(|error| io_error(&partial, error))?;
        let checkout = GitRepo::discover(Path::new(&plan.worktree_path))?;
        if let Err(reason) = write_verified_archive(
            self.repo_handle(),
            &checkout,
            &plan,
            session.task.as_deref(),
            &partial,
        ) {
            return Err(BrokerOpError::RecoveryArchiveUnverified {
                id: session_id,
                path: partial.to_string_lossy().into_owned(),
                reason,
            });
        }
        std::fs::rename(&partial, &archive).map_err(|error| io_error(&archive, error))?;

        // The archive is complete and verified; from here the worktree holds
        // nothing the archive does not, so the discard path is the right one.
        let cleanup_error = self.cleanup(session_id, true).err().map(|e| e.to_string());
        let worktree_removed = cleanup_error.is_none();
        let payload = serde_json::json!({
            "session_id": session_id,
            "archive": archive.to_string_lossy(),
            "head": plan.head,
            "disposition": plan.disposition.as_str(),
            "unlanded_commits": plan.unlanded_commits.len(),
            "untracked_files": plan.untracked_files.len(),
            "worktree_removed": worktree_removed,
        })
        .to_string();
        self.store().append_event(
            crate::events::BROKER_CLEANUP_ARCHIVED,
            Some(session_id),
            Some(&payload),
        )?;
        Ok(CleanupResolveOutcome {
            plan,
            archive,
            worktree_removed,
            cleanup_error,
        })
    }
}

/// Write every part of the archive into `dir`, then read each part back.
/// `Err` carries the first failure in words; the caller keeps the worktree.
fn write_verified_archive(
    repository: &GitRepo,
    checkout: &GitRepo,
    plan: &CleanupResolvePlan,
    task: Option<&str>,
    dir: &Path,
) -> Result<(), String> {
    let write = |name: &str, bytes: &[u8]| -> Result<PathBuf, String> {
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|error| format!("cannot write {name}: {error}"))?;
        Ok(path)
    };

    // Patches are taken again rather than trusted from the plan: the digest
    // check just passed, and a patch that no longer hashes the same means the
    // worktree moved in between.
    let mut patches = Vec::new();
    for (cached, file, expected) in [
        (true, STAGED_PATCH_FILE, &plan.staged_patch_sha256),
        (false, UNSTAGED_PATCH_FILE, &plan.unstaged_patch_sha256),
    ] {
        let bytes = checkout
            .recovery_patch(cached)
            .map_err(|error| format!("cannot take {file}: {error}"))?;
        let path = write(file, &bytes)?;
        let written =
            std::fs::read(&path).map_err(|error| format!("cannot read {file}: {error}"))?;
        if sha256_hex(&written) != *expected {
            return Err(format!("{file} no longer matches the reviewed plan"));
        }
        patches.push((!written.is_empty()).then_some(path));
    }

    let bundle = if plan.unlanded_commits.is_empty() {
        None
    } else {
        let path = dir.join(BUNDLE_FILE);
        checkout
            .create_recovery_bundle(&path, &plan.delivery_targets)
            .map_err(|error| format!("cannot write {BUNDLE_FILE}: {error}"))?;
        let heads = repository
            .verify_bundle(&path)
            .map_err(|error| format!("{BUNDLE_FILE} does not verify: {error}"))?;
        if !heads.contains(&plan.head) {
            return Err(format!("{BUNDLE_FILE} does not hold {}", plan.head));
        }
        Some(BUNDLE_FILE)
    };

    let scratch = tempfile::tempdir().map_err(|error| format!("no scratch index: {error}"))?;
    repository
        .check_recovery_patches(
            &plan.head,
            patches[0].as_deref(),
            patches[1].as_deref(),
            &scratch.path().join("index"),
        )
        .map_err(|error| format!("the patches do not apply to {}: {error}", plan.head))?;

    let worktree = Path::new(&plan.worktree_path);
    let untracked_root = dir.join(UNTRACKED_DIRECTORY);
    for file in &plan.untracked_files {
        let source = worktree.join(&file.path);
        let target = untracked_root.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let copied = if file.symlink {
            std::fs::read_link(&source).and_then(|link| std::os::unix::fs::symlink(link, &target))
        } else {
            std::fs::copy(&source, &target).map(|_| ())
        };
        copied.map_err(|error| format!("cannot copy {}: {error}", file.path))?;
        let archived = describe_untracked(&target, &file.path)
            .map_err(|error| format!("cannot read back {}: {error}", file.path))?;
        if archived.as_ref() != Some(file) {
            return Err(format!(
                "{} changed while it was archived, or did not copy intact",
                file.path
            ));
        }
    }

    let manifest = RecoveryManifest {
        schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
        created_at_ms: now_ms(),
        aethyme_version: env!("CARGO_PKG_VERSION"),
        repository: repository.root().to_string_lossy().into_owned(),
        session_id: plan.session_id,
        task,
        worktree_path: &plan.worktree_path,
        branch_ref: &plan.branch_ref,
        disposition: plan.disposition.as_str(),
        head: &plan.head,
        base: repository
            .head_commit()
            .ok()
            .and_then(|primary| crate::landing_base(repository, &plan.head, &primary).ok()),
        delivery_targets: &plan.delivery_targets,
        landing: &plan.landing,
        bundle,
        unlanded_commits: &plan.unlanded_commits,
        staged_patch: PatchRecord {
            file: STAGED_PATCH_FILE,
            bytes: plan.staged_patch_bytes,
            sha256: &plan.staged_patch_sha256,
        },
        unstaged_patch: PatchRecord {
            file: UNSTAGED_PATCH_FILE,
            bytes: plan.unstaged_patch_bytes,
            sha256: &plan.unstaged_patch_sha256,
        },
        untracked_directory: UNTRACKED_DIRECTORY,
        untracked_files: &plan.untracked_files,
    };
    let manifest =
        serde_json::to_vec_pretty(&manifest).map_err(|error| format!("manifest: {error}"))?;
    write(MANIFEST_FILE, &manifest)?;
    write(
        README_FILE,
        restore_readme(plan, bundle.is_some()).as_bytes(),
    )?;
    Ok(())
}

/// Restore steps written into the archive, specific to what it holds.
fn restore_readme(plan: &CleanupResolvePlan, has_bundle: bool) -> String {
    let checkout = if has_bundle {
        format!(
            "   git fetch <this-archive>/{BUNDLE_FILE} HEAD\n   git switch --detach FETCH_HEAD   # {}\n",
            plan.head
        )
    } else {
        format!(
            "   git switch --detach {}   # already on a delivery target\n",
            plan.head
        )
    };
    format!(
        "# Recovery archive for broker session {id}\n\n\
         Written by `aethyme broker finish cleanup resolve {id} --archive` before the worktree\n\
         `{path}` was removed. `{MANIFEST_FILE}` lists every part with its SHA-256.\n\
         Ignored files (build output, caches) were not archived; reinstall dependencies.\n\n\
         To restore into a NEW checkout of the same repository (never over a checkout in use):\n\n\
         1. Check out the session head:\n{checkout}\
         2. Re-apply the staged changes, then the unstaged ones (skip an empty patch):\n\
            git apply --index <this-archive>/{STAGED_PATCH_FILE}\n\
            git apply <this-archive>/{UNSTAGED_PATCH_FILE}\n\
         3. Copy back the untracked files:\n\
            cp -R <this-archive>/{UNTRACKED_DIRECTORY}/. .\n",
        id = plan.session_id,
        path = plan.worktree_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A checkout with one staged edit and one untracked file, and the plan
    /// that describes it exactly.
    fn staged_checkout() -> (tempfile::TempDir, GitRepo, CleanupResolvePlan) {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("a.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-qm", "init"]);
        std::fs::write(tmp.path().join("a.txt"), "two\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        std::fs::write(tmp.path().join("new.txt"), "untracked\n").unwrap();
        let repo = GitRepo::discover(tmp.path()).unwrap();
        let head = repo.head_commit().unwrap();
        let staged = repo.recovery_patch(true).unwrap();
        let unstaged = repo.recovery_patch(false).unwrap();
        let untracked = describe_untracked(&tmp.path().join("new.txt"), "new.txt")
            .unwrap()
            .unwrap();
        let plan = CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            digest: String::new(),
            session_id: 1,
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch_ref: "refs/heads/main".into(),
            disposition: CleanupDisposition::Dirty,
            reason: String::new(),
            head: head.clone(),
            delivery_targets: vec![head],
            landing: crate::LandingVerdict::NotLanded {
                examined: 0,
                truncated: false,
            },
            unlanded_commits: Vec::new(),
            staged_patch_bytes: staged.len() as u64,
            staged_patch_sha256: sha256_hex(&staged),
            unstaged_patch_bytes: unstaged.len() as u64,
            unstaged_patch_sha256: sha256_hex(&unstaged),
            untracked_bytes: untracked.bytes,
            untracked_files: vec![untracked],
            archive_root: PathBuf::new(),
            apply_command: String::new(),
            discard_command: String::new(),
        };
        (tmp, repo, plan)
    }

    #[test]
    fn an_archive_matching_its_plan_verifies() {
        let (_tmp, repo, plan) = staged_checkout();
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();
        assert!(dir.path().join(MANIFEST_FILE).is_file());
        assert_eq!(
            std::fs::read(dir.path().join("untracked/new.txt")).unwrap(),
            b"untracked\n"
        );
    }

    /// The patch written is re-hashed against the reviewed one, so a
    /// worktree that moved after review never yields an archive.
    #[test]
    fn a_patch_that_differs_from_the_plan_fails_verification() {
        let (_tmp, repo, mut plan) = staged_checkout();
        plan.staged_patch_sha256 = sha256_hex(b"something else");
        let dir = tempfile::tempdir().unwrap();
        let error = write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap_err();
        assert!(error.contains("staged.patch no longer matches"), "{error}");
        assert!(!dir.path().join(MANIFEST_FILE).exists());
    }

    /// Every copied file is read back and compared with what was reviewed.
    #[test]
    fn an_untracked_copy_that_differs_from_the_plan_fails_verification() {
        let (_tmp, repo, mut plan) = staged_checkout();
        plan.untracked_files[0].sha256 = sha256_hex(b"something else");
        let dir = tempfile::tempdir().unwrap();
        let error = write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap_err();
        assert!(error.contains("new.txt changed"), "{error}");
        assert!(!dir.path().join(MANIFEST_FILE).exists());
    }

    /// Patches are applied to the recorded head in a private index before the
    /// archive counts as complete.
    #[test]
    fn patches_that_do_not_apply_to_the_head_fail_verification() {
        let (tmp, repo, mut plan) = staged_checkout();
        // Record a head the staged patch was not taken against.
        std::fs::write(tmp.path().join("b.txt"), "b\n").unwrap();
        git(tmp.path(), &["add", "b.txt"]);
        git(tmp.path(), &["commit", "-qm", "b"]);
        git(tmp.path(), &["rm", "-q", "--cached", "a.txt"]);
        let staged = repo.recovery_patch(true).unwrap();
        plan.staged_patch_sha256 = sha256_hex(&staged);
        plan.staged_patch_bytes = staged.len() as u64;
        let dir = tempfile::tempdir().unwrap();
        let error = write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap_err();
        assert!(error.contains("do not apply"), "{error}");
    }

    #[test]
    fn only_plain_relative_paths_are_contained() {
        assert!(is_contained_relative("src/lib.rs"));
        assert!(!is_contained_relative("../escape"));
        assert!(!is_contained_relative("/etc/passwd"));
        assert!(!is_contained_relative("a/../../b"));
        assert!(!is_contained_relative(""));
    }

    /// A link is archived as a link: its hash names the target, and nothing
    /// outside the worktree is read through it.
    #[test]
    fn a_symlink_is_described_by_its_target_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("secret"), tmp.path().join("link")).unwrap();
        let described = describe_untracked(&tmp.path().join("link"), "link")
            .unwrap()
            .unwrap();
        assert!(described.symlink);
        assert_eq!(described.bytes, 0);
        let mut expected = b"symlink\0".to_vec();
        expected.extend_from_slice(tmp.path().join("secret").as_os_str().as_encoded_bytes());
        assert_eq!(described.sha256, sha256_hex(&expected));
    }
}
