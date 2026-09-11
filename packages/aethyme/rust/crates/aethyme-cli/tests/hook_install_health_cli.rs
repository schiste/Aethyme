//! `SessionStart` is where the broker tells an agent what it could not have
//! known. Two things about the installation itself belong in that category:
//! a router and engine built from different commits, and a release newer than
//! the one installed.
//!
//! Both are reported, neither is acted on, and the hook has hard constraints
//! that these exist to hold. Its stdout is parsed as an envelope, so a stray
//! line corrupts the session. It runs at a turn boundary, so it must not wait
//! on a network. And it must never fail: a hook that errors breaks the surface
//! it was meant to help.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ROUTER: &str = env!("CARGO_BIN_EXE_aethyme");

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    path_dir: PathBuf,
    cache: PathBuf,
    release: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let path_dir = temp.path().join("bin");
        let cache = temp.path().join("cache");
        let release = temp.path().join("release");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&path_dir).unwrap();

        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README.md"), "fixture\n").unwrap();
        fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);

        // The router under test is the one the hook resolves from PATH, so
        // the fixture's PATH has to contain it and nothing else that matters.
        std::os::unix::fs::symlink(ROUTER, path_dir.join("aethyme")).unwrap();

        Self {
            _temp: temp,
            repo,
            path_dir,
            cache,
            release,
        }
    }

    /// Put an engine on PATH whose `--version` banner is exactly `banner`.
    fn with_engine(self, banner: &str) -> Self {
        let script = self.path_dir.join("aethyme-engine-cli");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{banner}'; exit 0; fi\nexit 2\n"
            ),
        )
        .unwrap();
        make_executable(&script);
        self
    }

    fn session_start(&self) -> Output {
        let mut child = Command::new(ROUTER)
            .args(["hook", "SessionStart"])
            .current_dir(&self.repo)
            .env("PATH", &self.path_dir)
            // Point the release lookup at a directory that does not exist:
            // the hook must never reach a network, so the only correct
            // behaviour here is to say nothing about releases.
            .env(
                "AETHYME_RELEASE_BASE_URL",
                format!("file://{}", self.release.display()),
            )
            .env("AETHYME_HOST_CACHE_DIR", &self.cache)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"{}").unwrap();
        child.wait_with_output().unwrap()
    }
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
}

/// Parse the hook's stdout, asserting it is either empty or exactly one
/// envelope. Anything else is what "corrupts the session" means.
fn context(output: &Output) -> Option<String> {
    assert!(
        output.status.success(),
        "the hook must never fail: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    assert_eq!(
        text.lines().count(),
        1,
        "stdout is an envelope slot and must carry exactly one line: {text}"
    );
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("stdout must be JSON: {error}: {text}"));
    Some(
        value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// The skew that prompted this: one version, two commits. A check comparing
/// only the version number would have called this pair healthy.
#[test]
fn a_split_pair_is_reported_at_session_start() {
    let fixture = Fixture::new().with_engine("aethyme-engine-cli 0.0.1 (v0.0.1-6-gdeadbee)");
    let context = context(&fixture.session_start()).expect("a split pair must produce a notice");
    assert!(
        context.contains("different builds"),
        "the notice must name the problem: {context}"
    );
    assert!(
        context.contains("aethyme-engine-cli 0.0.1 (v0.0.1-6-gdeadbee)"),
        "the notice must show the evidence: {context}"
    );
    assert!(
        context.contains("cargo install"),
        "the notice must name the remedy: {context}"
    );
}

/// Reporting is the whole contract. A hook that repaired the installation on
/// its own would be changing binaries out from under a running agent.
#[test]
fn the_notice_changes_nothing_on_disk() {
    let fixture = Fixture::new().with_engine("aethyme-engine-cli 0.0.1 (v0.0.1-6-gdeadbee)");
    let before = fs::read_to_string(fixture.path_dir.join("aethyme-engine-cli")).unwrap();
    fixture.session_start();
    let after = fs::read_to_string(fixture.path_dir.join("aethyme-engine-cli")).unwrap();
    assert_eq!(before, after, "the hook must not touch the installation");
}

/// A missing engine is a different fault from a mismatched one and has to
/// read as one: `aethyme` alone is not a working install.
#[test]
fn a_missing_engine_is_reported_as_missing() {
    let fixture = Fixture::new();
    let context = context(&fixture.session_start()).expect("a missing engine must be reported");
    assert!(
        context.contains("aethyme-engine-cli"),
        "the notice must name the binary: {context}"
    );
    assert!(
        context.contains("not optional"),
        "the notice must say it is required: {context}"
    );
}

/// The silent case, which is the one that runs thousands of times. A matched
/// pair with no cached release answer has nothing to add, and a `SessionStart`
/// with nothing to add must cost nothing.
#[test]
fn a_healthy_pair_adds_nothing_about_the_installation() {
    let banner = format!("aethyme-engine-cli {}", router_build());
    let fixture = Fixture::new().with_engine(&banner);
    let context = context(&fixture.session_start()).unwrap_or_default();
    assert!(
        !context.contains("different builds") && !context.contains("not optional"),
        "a matched pair must produce no installation notice: {context}"
    );
    assert!(
        !context.contains("is available"),
        "an unreachable release server must produce no release notice: {context}"
    );
}

/// The opt-out has to reach the hook, not just the command.
#[test]
fn the_opt_out_silences_the_notice() {
    let fixture = Fixture::new().with_engine("aethyme-engine-cli 0.0.1 (v0.0.1-6-gdeadbee)");
    let mut child = Command::new(ROUTER)
        .args(["hook", "SessionStart"])
        .current_dir(&fixture.repo)
        .env("PATH", &fixture.path_dir)
        .env("AETHYME_HOST_CACHE_DIR", &fixture.cache)
        .env("AETHYME_UPDATE_CHECK", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    let context = context(&output).unwrap_or_default();
    assert!(
        !context.contains("different builds"),
        "AETHYME_UPDATE_CHECK=off must silence the notice: {context}"
    );
}

/// `aethyme --version`'s build identity, which the engine banner must match
/// for the pair to count as aligned.
fn router_build() -> String {
    let output = Command::new(ROUTER).arg("--version").output().unwrap();
    let banner = String::from_utf8_lossy(&output.stdout);
    let banner = banner.lines().next().unwrap().trim();
    banner
        .split_once(' ')
        .expect("a version banner is `<program> <version> ...`")
        .1
        .to_string()
}
