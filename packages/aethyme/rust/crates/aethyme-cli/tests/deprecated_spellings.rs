//! The deprecation window: every old spelling keeps working, prints exactly
//! one warning line on stderr naming the new spelling, and leaves stdout (and
//! so `--json`) untouched. The new spelling reaches the same implementation.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn aethyme(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(cwd)
        .env_remove("AETHYME_REPO")
        .env_remove("AETHYME_AGENT")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme")
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    tmp
}

fn warnings(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|line| line.contains("is deprecated"))
        .map(str::to_string)
        .collect()
}

fn keys(output: &Output) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let mut keys: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[test]
fn old_adopt_spelling_warns_once_on_stderr_and_matches_start_adopt() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    let old_path = tmp.path().join("old");
    let new_path = tmp.path().join("new");
    for (branch, path) in [("old", &old_path), ("new", &new_path)] {
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
    }

    let old = aethyme(&old_path, &["broker", "adopt", "--task", "old", "--json"]);
    assert!(
        old.status.success(),
        "{}",
        String::from_utf8_lossy(&old.stderr)
    );
    assert_eq!(
        warnings(&old),
        [
            "warning: 'aethyme broker adopt' is deprecated; use 'aethyme broker start --adopt' \
          (the old spelling is removed in v0.8.8)"
        ]
    );

    let new = aethyme(
        &new_path,
        &["broker", "start", "--adopt", "--task", "new", "--json"],
    );
    assert!(
        new.status.success(),
        "{}",
        String::from_utf8_lossy(&new.stderr)
    );
    assert!(warnings(&new).is_empty(), "{:?}", warnings(&new));
    assert_eq!(keys(&old), keys(&new));

    // `adopt --reuse` names its own replacement.
    let reused = aethyme(
        &old_path,
        &["broker", "adopt", "--reuse", "--task", "again", "--json"],
    );
    assert!(
        reused.status.success(),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
    assert_eq!(warnings(&reused).len(), 1);
    assert!(warnings(&reused)[0].contains("use 'aethyme broker start --reuse'"));
    let reused_new = aethyme(
        &new_path,
        &["broker", "start", "--reuse", "--task", "again", "--json"],
    );
    assert!(
        reused_new.status.success(),
        "{}",
        String::from_utf8_lossy(&reused_new.stderr)
    );
    assert!(warnings(&reused_new).is_empty());
    assert_eq!(keys(&reused), keys(&reused_new));
}

#[test]
fn merged_verbs_reach_the_same_implementation() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    for (old, new) in [
        (
            vec!["broker", "blockers", "--json"],
            vec!["broker", "unblock", "--json"],
        ),
        (
            vec!["broker", "reclaim", "plan", "--json"],
            vec!["broker", "gc", "reclaim", "plan", "--json"],
        ),
        (
            vec!["broker", "promotion-record", "plan", "--json"],
            vec!["broker", "submit", "promotion-record", "plan", "--json"],
        ),
        (
            vec!["broker", "leases", "--json"],
            vec!["broker", "advanced", "leases", "--json"],
        ),
    ] {
        let old_output = aethyme(&repo, &old);
        let new_output = aethyme(&repo, &new);
        assert_eq!(
            old_output.status.code(),
            new_output.status.code(),
            "{old:?} vs {new:?}"
        );
        assert_eq!(warnings(&old_output).len(), 1, "{old:?}");
        assert!(warnings(&new_output).is_empty(), "{new:?}");
        if old_output.status.success() {
            assert_eq!(keys(&old_output), keys(&new_output), "{old:?} vs {new:?}");
        } else {
            // Same implementation, same refusal: only the warning differs.
            let errors = |output: &Output| -> Vec<String> {
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .filter(|line| !line.contains("is deprecated"))
                    .map(str::to_string)
                    .collect()
            };
            assert_eq!(errors(&old_output), errors(&new_output), "{old:?}");
        }
    }
}

#[test]
fn top_level_duplicates_warn_and_keep_working() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    let readiness = aethyme(&repo, &["readiness", "--json"]);
    assert_eq!(warnings(&readiness).len(), 1);
    assert!(warnings(&readiness)[0].contains("use 'aethyme broker status readiness'"));
    let merged = aethyme(&repo, &["broker", "status", "readiness", "--json"]);
    assert!(warnings(&merged).is_empty());
    assert_eq!(readiness.status.code(), merged.status.code());
    assert_eq!(keys(&readiness), keys(&merged));

    let certify = aethyme(&repo, &["broker", "certify", "--json"]);
    assert_eq!(warnings(&certify).len(), 1);
    assert!(warnings(&certify)[0].contains("use 'aethyme certify'"));
    let top = aethyme(&repo, &["certify", "--json"]);
    assert!(warnings(&top).is_empty());
    assert_eq!(certify.status.code(), top.status.code());

    let enhance = aethyme(&repo, &["enhance", "verify", "--repo", "."]);
    assert_eq!(warnings(&enhance).len(), 1);
    assert!(warnings(&enhance)[0].contains("use 'aethyme deploy verify'"));
}

#[test]
fn installed_hook_entry_points_never_warn() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    let output = aethyme(&repo, &["broker", "hooks", "post-commit"]);
    assert!(warnings(&output).is_empty(), "{:?}", warnings(&output));
}

/// A public verb spelled under `advanced` used to be stripped twice: the
/// router classified `advanced status doctor` as `status` (a diagnostic read)
/// while the broker ran `doctor` (a shared mutation), so a repository on a
/// newer schema refused `broker doctor` but let `advanced status doctor`
/// through. Those spellings are now refused before the preflight, and each
/// sub-form is gated exactly like the internal command it runs.
#[test]
fn public_verbs_under_advanced_cannot_bypass_the_compatibility_gate() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/repository.json"),
        "{\"schema_version\":999}\n",
    )
    .unwrap();

    for line in [
        "broker doctor --json",
        "broker status doctor --json",
        "broker promote --entry 1 --json",
        "broker submit promote --entry 1 --json",
    ] {
        let args: Vec<&str> = line.split(' ').collect();
        let output = aethyme(&repo, &args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "`{line}` must be refused by the compatibility gate: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for (line, public) in [
        (
            "broker advanced status doctor --json",
            "status doctor --json",
        ),
        (
            "broker advanced status readiness recover --plan x",
            "status readiness recover --plan x",
        ),
        (
            "broker advanced submit promote --entry 1 --json",
            "submit promote --entry 1 --json",
        ),
        (
            "broker advanced submit prepare --session 1",
            "submit prepare --session 1",
        ),
    ] {
        let args: Vec<&str> = line.split(' ').collect();
        let output = aethyme(&repo, &args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "`{line}`: {stderr}");
        assert!(
            stderr.contains(&format!("use 'aethyme broker {public}'")),
            "`{line}`: {stderr}"
        );
        assert!(output.stdout.is_empty(), "`{line}` printed on stdout");
    }
}
