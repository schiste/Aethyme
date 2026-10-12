//! Validation for the appointed Local controller's local Git publication
//! endpoint. A receiver must be the bare repository itself: Git otherwise
//! discovers a parent repository and may update an ordinary checkout when
//! `receive.denyCurrentBranch=updateInstead` is configured.

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
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
        let current = validate_path(&self.path)?;
        if current.path != self.path || current.identity != self.identity {
            return Err(rejected("endpoint changed after it was verified"));
        }
        Ok(())
    }
}

/// Accept only an absolute local filesystem path whose canonical directory is
/// itself Git's bare git-dir. Git is invoked through the broker's trusted
/// binary, with inherited environment and global/system configuration removed,
/// a finite output cap and a short deadline.
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

    let mut command = git_command();
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_config_path())
        .env("GIT_CONFIG_GLOBAL", null_config_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--is-bare-repository", "--absolute-git-dir"]);
    let output = match output_limited(&mut command, PROBE_BUDGET, MAX_PROBE_STREAM_BYTES)
        .map_err(|error| rejected(format!("could not run trusted Git probe: {error}")))?
    {
        LimitedOutput::Completed(output) if output.status.success() => output,
        LimitedOutput::Completed(output) => {
            return Err(rejected(format!(
                "Git did not recognize a repository endpoint: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        LimitedOutput::Deadline => {
            return Err(rejected("Git endpoint probe exceeded its deadline"));
        }
        LimitedOutput::OutputLimit => {
            return Err(rejected("Git endpoint probe exceeded its output limit"));
        }
    };

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

    // Catch ordinary replacement while Git was inspecting the path. The
    // caller rechecks the same identity immediately before publication.
    let after = fs::symlink_metadata(&canonical)
        .map_err(|error| rejected(format!("endpoint changed during verification: {error}")))?;
    if !after.is_dir() || after.file_type().is_symlink() || file_identity(&after)? != identity {
        return Err(rejected("endpoint changed during verification"));
    }

    Ok(BareDestination {
        path: canonical,
        identity,
    })
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
    use crate::git::git_command;
    use std::process::Output;

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
        fs::create_dir(&checkout).unwrap();
        run_git(&checkout, &["init", "-q"]);
        fs::write(checkout.join("tracked.txt"), "original checkout content\n").unwrap();
        run_git(&checkout, &["add", "tracked.txt"]);
        run_git(&checkout, &["commit", "-q", "-m", "baseline"]);
        run_git(
            &checkout,
            &[
                "config",
                "--local",
                "receive.denyCurrentBranch",
                "updateInstead",
            ],
        );
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
}
