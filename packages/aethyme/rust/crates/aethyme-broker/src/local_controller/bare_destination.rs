//! Validation for the appointed Local controller's local Git publication
//! endpoint. A receiver must be the bare repository itself: Git otherwise
//! discovers a parent repository and may update an ordinary checkout when
//! `receive.denyCurrentBranch=updateInstead` is configured.

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use super::ControllerError;
use crate::bounded_output::{LimitedOutput, output_limited};
use crate::git::git_command;

const PROBE_BUDGET: Duration = Duration::from_secs(4);
const MAX_PROBE_STREAM_BYTES: usize = 4 * 1024;

/// Canonical path and filesystem identity for a verified bare Git endpoint.
/// Recheck this value immediately before publication. This detects ordinary
/// replacement of the directory between acceptance and publication; it does
/// not lock out a trusted host process that races Git or changes repository
/// configuration after the final check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BareDestination {
    requested_path: PathBuf,
    path: PathBuf,
    identity: FileIdentity,
}

impl BareDestination {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Re-run the bounded Git probe and reject a path replaced since the
    /// destination was first accepted.
    pub(crate) fn revalidate(&self) -> Result<(), ControllerError> {
        // Keep checking the original path the caller will use for the
        // publication URL. Rechecking only the canonical path would miss an
        // ancestor symlink retargeting to another receiver.
        let current = validate_path(&self.requested_path)?;
        if current.path != self.path || current.identity != self.identity {
            return Err(rejected("endpoint changed after it was verified"));
        }
        Ok(())
    }
}

/// Accept only an absolute local filesystem path whose canonical directory is
/// itself Git's bare git-dir, with no linked worktrees. Git is invoked through
/// the broker's trusted binary, with inherited environment and global/system
/// configuration removed, a finite output cap and a short deadline.
pub(crate) fn validate_endpoint(raw: &str) -> Result<BareDestination, ControllerError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(rejected("endpoint must be an absolute local path"));
    }
    validate_path(path)
}

fn validate_path(path: &Path) -> Result<BareDestination, ControllerError> {
    if !path.is_absolute() {
        return Err(rejected("endpoint must be an absolute local path"));
    }

    // Remove a trailing separator when lstat'ing the final component; on
    // Unix, `symlink_metadata("link/")` follows the link instead of exposing
    // it as a symlink.
    let final_component = path
        .file_name()
        .ok_or_else(|| rejected("endpoint must name a repository directory"))?;
    let lstat_path = path
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join(final_component);
    let link_metadata = fs::symlink_metadata(&lstat_path)
        .map_err(|error| rejected(format!("cannot inspect endpoint: {error}")))?;
    if link_metadata.file_type().is_symlink() {
        return Err(rejected("endpoint directory must not be a symlink"));
    }

    let canonical = path
        .canonicalize()
        .map_err(|error| rejected(format!("cannot resolve endpoint: {error}")))?;
    if !canonical.is_absolute() {
        return Err(rejected("canonical endpoint is not absolute"));
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|error| rejected(format!("cannot inspect canonical endpoint: {error}")))?;
    if !metadata.is_dir() {
        return Err(rejected("endpoint is not a directory"));
    }
    let identity = file_identity(&metadata)?;

    let output = trusted_git_probe(
        &canonical,
        &["rev-parse", "--is-bare-repository", "--absolute-git-dir"],
        "repository endpoint probe",
    )?;

    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| rejected("Git endpoint probe returned non-UTF-8 output"))?;
    let mut lines = stdout.lines();
    let is_bare = lines.next() == Some("true");
    let reported_git_dir = lines
        .next()
        .ok_or_else(|| rejected("Git endpoint probe omitted the git-dir"))?;
    if lines.next().is_some() {
        return Err(rejected("Git endpoint probe returned unexpected output"));
    }
    if !is_bare {
        return Err(rejected("endpoint is not a bare Git repository"));
    }

    let reported_git_dir = PathBuf::from(reported_git_dir);
    if !reported_git_dir.is_absolute() {
        return Err(rejected("Git reported a non-absolute git-dir"));
    }
    let canonical_git_dir = reported_git_dir
        .canonicalize()
        .map_err(|error| rejected(format!("cannot resolve Git's reported git-dir: {error}")))?;
    if canonical_git_dir != canonical {
        return Err(rejected(
            "endpoint resolves through Git parent discovery or a separate git-dir",
        ));
    }

    let worktrees = trusted_git_probe(
        &canonical,
        &["worktree", "list", "--porcelain", "-z"],
        "bare endpoint worktree inventory",
    )?;
    if worktrees.stdout != single_bare_worktree_record(&canonical)? {
        return Err(rejected(
            "bare endpoint has linked worktrees or an unexpected worktree inventory",
        ));
    }

    // Catch ordinary replacement while Git was inspecting the path. The
    // caller rechecks the same identity immediately before publication.
    let after = fs::symlink_metadata(&canonical)
        .map_err(|error| rejected(format!("endpoint changed during verification: {error}")))?;
    if !after.is_dir() || after.file_type().is_symlink() || file_identity(&after)? != identity {
        return Err(rejected("endpoint changed during verification"));
    }

    Ok(BareDestination {
        requested_path: path.to_path_buf(),
        path: canonical,
        identity,
    })
}

fn trusted_git_probe(
    repository: &Path,
    args: &[&str],
    operation: &str,
) -> Result<Output, ControllerError> {
    let mut command = git_command();
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_config_path())
        .env("GIT_CONFIG_GLOBAL", null_config_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(repository)
        .args(args);
    match output_limited(&mut command, PROBE_BUDGET, MAX_PROBE_STREAM_BYTES)
        .map_err(|error| rejected(format!("could not run trusted Git {operation}: {error}")))?
    {
        LimitedOutput::Completed(output) if output.status.success() => Ok(output),
        LimitedOutput::Completed(output) => Err(rejected(format!(
            "Git {operation} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
        LimitedOutput::Deadline => Err(rejected(format!("Git {operation} exceeded its deadline"))),
        LimitedOutput::OutputLimit => Err(rejected(format!(
            "Git {operation} exceeded its output limit"
        ))),
    }
}

fn single_bare_worktree_record(path: &Path) -> Result<Vec<u8>, ControllerError> {
    let path = path
        .to_str()
        .ok_or_else(|| rejected("canonical endpoint path is not UTF-8"))?;
    let mut expected = b"worktree ".to_vec();
    expected.extend_from_slice(path.as_bytes());
    expected.extend_from_slice(b"\0bare\0\0");
    Ok(expected)
}

fn rejected(reason: impl std::fmt::Display) -> ControllerError {
    ControllerError::Canonical(format!("local Git publication endpoint rejected: {reason}"))
}

fn null_config_path() -> &'static str {
    #[cfg(windows)]
    {
        "NUL"
    }
    #[cfg(not(windows))]
    {
        "/dev/null"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> Result<FileIdentity, ControllerError> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_identity(_: &Metadata) -> Result<FileIdentity, ControllerError> {
    Err(rejected(
        "this platform cannot provide the filesystem identity required for endpoint pinning",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn run_git(repo: &Path, args: &[&str]) -> Output {
        let mut command = git_command();
        command
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", null_config_path())
            .env("GIT_CONFIG_GLOBAL", null_config_path())
            .env("GIT_AUTHOR_NAME", "endpoint fixture")
            .env("GIT_AUTHOR_EMAIL", "endpoint@example.invalid")
            .env("GIT_COMMITTER_NAME", "endpoint fixture")
            .env("GIT_COMMITTER_EMAIL", "endpoint@example.invalid")
            .current_dir(repo)
            .args(args);
        let LimitedOutput::Completed(output) =
            output_limited(&mut command, Duration::from_secs(5), MAX_PROBE_STREAM_BYTES).unwrap()
        else {
            panic!("fixture Git command was not completed");
        };
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_git_text(repo: &Path, args: &[&str]) -> String {
        String::from_utf8(run_git(repo, args).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn init_bare(path: &Path) {
        fs::create_dir(path).unwrap();
        run_git(path, &["init", "--bare", "-q"]);
    }

    fn init_update_instead_checkout(path: &Path) {
        fs::create_dir(path).unwrap();
        run_git(path, &["init", "-q"]);
        fs::write(path.join("tracked.txt"), "original checkout content\n").unwrap();
        run_git(path, &["add", "tracked.txt"]);
        run_git(path, &["commit", "-q", "-m", "baseline"]);
        run_git(
            path,
            &[
                "config",
                "--local",
                "receive.denyCurrentBranch",
                "updateInstead",
            ],
        );
    }

    fn init_bare_with_checked_out_linked_worktree(root: &Path, bare: &Path, linked: &Path) {
        init_bare(bare);
        let seed = root.join("seed");
        fs::create_dir(&seed).unwrap();
        run_git(&seed, &["init", "-q"]);
        run_git(&seed, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        fs::write(seed.join("tracked.txt"), "linked worktree baseline\n").unwrap();
        run_git(&seed, &["add", "tracked.txt"]);
        run_git(&seed, &["commit", "-q", "-m", "baseline"]);

        run_git(bare, &["remote", "add", "seed", seed.to_str().unwrap()]);
        run_git(bare, &["fetch", "-q", "seed", "main"]);
        run_git(bare, &["branch", "main", "FETCH_HEAD"]);
        run_git(bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        run_git(
            bare,
            &[
                "config",
                "--local",
                "receive.denyCurrentBranch",
                "updateInstead",
            ],
        );
        run_git(bare, &["worktree", "add", linked.to_str().unwrap(), "main"]);
    }

    fn err(raw: &str) -> String {
        validate_endpoint(raw).unwrap_err().to_string()
    }

    #[test]
    fn accepts_only_the_exact_canonical_bare_git_directory() {
        let temp = tempfile::tempdir().unwrap();
        let bare = temp.path().join("publication.git");
        init_bare(&bare);
        let refs_before = run_git_text(&bare, &["for-each-ref", "--format=%(refname)"]);

        let accepted = validate_endpoint(bare.to_str().unwrap()).unwrap();
        assert_eq!(accepted.path(), bare.canonicalize().unwrap());
        accepted.revalidate().unwrap();
        assert_eq!(
            refs_before,
            run_git_text(&bare, &["for-each-ref", "--format=%(refname)"])
        );

        let nested = bare.join("objects");
        assert!(err(nested.to_str().unwrap()).contains("parent discovery"));
        assert!(err("relative-publication.git").contains("absolute local path"));
        assert!(err("file:///tmp/publication.git").contains("absolute local path"));
    }

    #[test]
    fn rejects_update_instead_checkout_without_changing_its_ref_or_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        init_update_instead_checkout(&checkout);
        assert_eq!(
            run_git_text(&checkout, &["config", "--get", "receive.denyCurrentBranch"]),
            "updateInstead"
        );

        let head_before = run_git_text(&checkout, &["rev-parse", "HEAD"]);
        let status_before = run_git_text(&checkout, &["status", "--porcelain"]);
        let content_before = fs::read(checkout.join("tracked.txt")).unwrap();
        assert!(err(checkout.to_str().unwrap()).contains("not a bare Git repository"));
        assert_eq!(head_before, run_git_text(&checkout, &["rev-parse", "HEAD"]));
        assert_eq!(
            status_before,
            run_git_text(&checkout, &["status", "--porcelain"])
        );
        assert_eq!(
            content_before,
            fs::read(checkout.join("tracked.txt")).unwrap()
        );
    }

    #[test]
    fn rejects_symlinked_endpoint_and_replacement_after_validation() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.git");
        let second = temp.path().join("second.git");
        let retired = temp.path().join("retired.git");
        let alias = temp.path().join("alias.git");
        init_bare(&first);
        init_bare(&second);
        symlink(&first, &alias).unwrap();
        assert!(err(alias.to_str().unwrap()).contains("must not be a symlink"));

        let accepted = validate_endpoint(first.to_str().unwrap()).unwrap();
        fs::rename(&first, &retired).unwrap();
        fs::rename(&second, &first).unwrap();
        assert!(accepted.revalidate().is_err());
    }

    #[test]
    fn revalidation_detects_an_ancestor_symlink_retarget() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let parent_a = temp.path().join("parent-a");
        let parent_b = temp.path().join("parent-b");
        let parent_link = temp.path().join("publication-parent");
        fs::create_dir(&parent_a).unwrap();
        fs::create_dir(&parent_b).unwrap();

        let first_receiver = parent_a.join("publication.git");
        let second_receiver = parent_b.join("publication.git");
        init_bare(&first_receiver);
        init_update_instead_checkout(&second_receiver);
        symlink(&parent_a, &parent_link).unwrap();

        let requested_path = parent_link.join("publication.git");
        let accepted = validate_endpoint(requested_path.to_str().unwrap()).unwrap();
        assert_eq!(accepted.path(), first_receiver.canonicalize().unwrap());

        fs::remove_file(&parent_link).unwrap();
        symlink(&parent_b, &parent_link).unwrap();
        assert!(accepted.revalidate().is_err());
    }

    #[test]
    fn rejects_bare_endpoint_with_update_instead_linked_worktree_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let bare = temp.path().join("publication.git");
        let linked = temp.path().join("linked");
        init_bare_with_checked_out_linked_worktree(temp.path(), &bare, &linked);

        let head_before = run_git_text(&linked, &["rev-parse", "HEAD"]);
        let branch_before = run_git_text(&bare, &["rev-parse", "refs/heads/main"]);
        let status_before = run_git_text(&linked, &["status", "--porcelain"]);
        let content_before = fs::read(linked.join("tracked.txt")).unwrap();
        assert!(err(bare.to_str().unwrap()).contains("linked worktrees"));
        assert_eq!(head_before, run_git_text(&linked, &["rev-parse", "HEAD"]));
        assert_eq!(
            branch_before,
            run_git_text(&bare, &["rev-parse", "refs/heads/main"])
        );
        assert_eq!(
            status_before,
            run_git_text(&linked, &["status", "--porcelain"])
        );
        assert_eq!(
            content_before,
            fs::read(linked.join("tracked.txt")).unwrap()
        );
    }
}
