//! Implementation-blind checks for `aethyme repo branches audit`.
//!
//! Each case builds a scratch repository whose `origin` is a GitHub-looking
//! URL rewritten (`url.<bare>.insteadOf`) to a local bare repository, so
//! `ls-remote` is real and the GitHub slug is detected. A stub `gh` on PATH
//! answers `gh pr list --head <branch>` with canned merged PRs.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_testkit::Invoke;
use aethyme_testkit::repos::write;
use aethyme_testkit::tmp_dir;
use serde_json::Value;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit_file(dir: &Path, file: &str, content: &str, message: &str) -> String {
    write(dir.join(file), content);
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Fixture {
    _tmp: aethyme_testkit::tempfile::TempDir,
    work: PathBuf,
    worktree: PathBuf,
    stub_dir: PathBuf,
}

/// Builds every classification at once:
/// - `pushed`: tip pushed as is (on-remote), checked out in a dirty worktree;
/// - `older`: an ancestor of the pushed `feature` (contained);
/// - `picked`: its commit was cherry-picked onto main (merged-via-pr, patch-id);
/// - `squashed`: squash-merged into main as PR #7 (merged-via-pr, squash patch);
/// - `draft`: a merged PR #9 exists for the name, but its commit matches nothing;
/// - `wip`: never pushed, no PR (local-only).
fn fixture() -> Fixture {
    let tmp = tmp_dir();
    let bare = tmp.path().join("remote.git");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");
    git(
        tmp.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            bare.to_str().expect("utf8"),
        ],
    );
    git(&work, &["init", "-q", "-b", "main"]);
    let url = "https://github.com/acme/widgets.git";
    git(&work, &["remote", "add", "origin", url]);
    git(
        &work,
        &["config", &format!("url.{}.insteadOf", bare.display()), url],
    );
    commit_file(&work, "base.txt", "base\n", "base");
    git(&work, &["push", "-q", "origin", "main"]);

    git(&work, &["switch", "-q", "-c", "pushed"]);
    commit_file(&work, "pushed.txt", "pushed\n", "pushed work");
    git(&work, &["push", "-q", "origin", "pushed"]);

    git(&work, &["switch", "-q", "-c", "feature", "main"]);
    commit_file(&work, "feature.txt", "one\n", "feature one");
    commit_file(&work, "feature.txt", "one\ntwo\n", "feature two");
    git(&work, &["push", "-q", "origin", "feature"]);
    git(&work, &["branch", "older", "feature~1"]);
    git(&work, &["switch", "-q", "main"]);
    git(&work, &["branch", "-D", "feature"]);

    git(&work, &["switch", "-q", "-c", "picked", "main"]);
    let picked = commit_file(&work, "picked.txt", "picked\n", "picked change");
    git(&work, &["switch", "-q", "main"]);
    // Move main first so the cherry-pick gets a different parent and SHA.
    commit_file(&work, "main.txt", "main moves\n", "main moves");
    git(&work, &["cherry-pick", picked.as_str()]);

    git(&work, &["switch", "-q", "-c", "squashed", "main"]);
    commit_file(&work, "squash.txt", "a\n", "squash part a");
    commit_file(&work, "squash.txt", "a\nb\n", "squash part b");
    git(&work, &["switch", "-q", "main"]);
    git(&work, &["merge", "-q", "--squash", "squashed"]);
    git(&work, &["commit", "-q", "-m", "squashed (#7)"]);
    let squash_commit = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["push", "-q", "origin", "main"]);

    git(&work, &["switch", "-q", "-c", "draft", "main"]);
    commit_file(&work, "draft.txt", "early draft\n", "early draft");
    git(&work, &["switch", "-q", "-c", "wip", "main"]);
    commit_file(&work, "wip.txt", "wip\n", "unpublished wip");
    git(&work, &["switch", "-q", "main"]);

    let worktree = tmp.path().join("wt-pushed");
    git(
        &work,
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().expect("utf8"),
            "pushed",
        ],
    );
    write(worktree.join("scratch.txt"), "uncommitted\n");

    let stub_dir = tmp.path().join("stub-bin");
    std::fs::create_dir_all(&stub_dir).expect("mkdir stub");
    let missing_head = "1111111111111111111111111111111111111111";
    write(
        stub_dir.join("gh"),
        &format!(
            r#"#!/bin/sh
head=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--head" ]; then head="$2"; fi
  shift
done
case "$head" in
  squashed) echo '[{{"number":7,"url":"https://github.com/acme/widgets/pull/7","mergedAt":"2026-10-01T00:00:00Z","headRefOid":"{missing_head}","mergeCommit":{{"oid":"{squash_commit}"}}}}]' ;;
  draft) echo '[{{"number":9,"url":"https://github.com/acme/widgets/pull/9","mergedAt":"2026-10-01T00:00:00Z","headRefOid":"{missing_head}","mergeCommit":{{"oid":"{missing_head}"}}}}]' ;;
  *) echo '[]' ;;
esac
"#
        ),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(stub_dir.join("gh"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }
    Fixture {
        _tmp: tmp,
        work,
        worktree,
        stub_dir,
    }
}

fn audit(fixture: &Fixture, extra: &[&str]) -> Value {
    let path = format!(
        "{}:{}",
        fixture.stub_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut args = vec!["repo", "branches", "audit", "--json-output"];
    args.extend_from_slice(extra);
    let result = Invoke::new(args).cwd(&fixture.work).env("PATH", path).run();
    result.ok();
    result.json()
}

fn branch<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["branches"]
        .as_array()
        .expect("branches array")
        .iter()
        .find(|branch| branch["name"] == name)
        .unwrap_or_else(|| panic!("branch {name} missing from {report}"))
}

#[test]
fn branches_audit_classifies_each_branch_with_evidence() {
    let fixture = fixture();
    let report = audit(&fixture, &[]);

    assert_eq!(report["remote_source"], "git ls-remote origin");
    assert_eq!(report["default_branch"], "main");
    assert_eq!(report["gh"], "used for acme/widgets");

    let pushed = branch(&report, "pushed");
    assert_eq!(pushed["classification"], "on-remote");
    assert_eq!(pushed["evidence"]["remote_branches"][0], "pushed");
    assert_eq!(pushed["protected"], true);

    let older = branch(&report, "older");
    assert_eq!(older["classification"], "contained");
    assert_eq!(older["evidence"]["contained_in"], "feature");

    let picked = branch(&report, "picked");
    assert_eq!(picked["classification"], "merged-via-pr");
    assert_eq!(picked["evidence"]["matched_by"][0]["kind"], "patch-id");
    assert_eq!(picked["evidence"]["matched_by"][0]["on"], "main");

    let squashed = branch(&report, "squashed");
    assert_eq!(squashed["classification"], "merged-via-pr", "{squashed}");
    assert_eq!(
        squashed["evidence"]["matched_by"][0]["kind"],
        "pr-squash-patch"
    );
    assert_eq!(squashed["evidence"]["matched_by"][0]["pr"], 7);

    let draft = branch(&report, "draft");
    assert_eq!(draft["classification"], "local-only");
    assert_eq!(draft["local_commits"][0]["subject"], "early draft");
    assert_eq!(draft["evidence"]["pull_requests"][0]["number"], 9);
    assert!(draft["evidence"]["note"].as_str().is_some(), "{draft}");

    let wip = branch(&report, "wip");
    assert_eq!(wip["classification"], "local-only");
    assert_eq!(wip["uncovered_commit_count"], 1);
    assert!(wip["evidence"].get("note").is_none(), "{wip}");

    let main = branch(&report, "main");
    assert_eq!(main["protected"], true);
    let reasons = main["protected_reasons"].to_string();
    assert!(reasons.contains("default branch"), "{reasons}");

    assert_eq!(report["summary"]["local-only"], 2);
    assert_eq!(report["summary"]["merged-via-pr"], 2);

    let worktree_path = fixture.worktree.canonicalize().expect("canonical worktree");
    let worktree = report["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|entry| {
            Path::new(entry["path"].as_str().unwrap_or_default())
                .canonicalize()
                .ok()
                .as_deref()
                == Some(worktree_path.as_path())
        })
        .unwrap_or_else(|| panic!("worktree missing from {report}"));
    assert_eq!(worktree["branch"], "pushed");
    assert_eq!(worktree["dirty_paths"], 1);
}

#[test]
fn branches_audit_without_gh_keeps_squash_merged_work_local_only() {
    let fixture = fixture();
    let report = audit(&fixture, &["--no-gh"]);
    assert_eq!(report["gh"], "disabled (--no-gh)");
    // Without the PR lookup a squash merge is not provable from git alone.
    assert_eq!(branch(&report, "squashed")["classification"], "local-only");
    // Patch-id equivalence needs no network and still holds.
    assert_eq!(branch(&report, "picked")["classification"], "merged-via-pr");
}

#[test]
fn branches_audit_is_read_only() {
    let fixture = fixture();
    let refs_before = git(&fixture.work, &["for-each-ref"]);
    let index = fixture.work.join(".git").join("index");
    let index_before = std::fs::metadata(&index)
        .and_then(|meta| meta.modified())
        .ok();
    let path = format!(
        "{}:{}",
        fixture.stub_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let result = Invoke::new(["repo", "branches", "audit"])
        .cwd(&fixture.work)
        .env("PATH", path)
        .run();
    result.ok();
    result.assert_contains("local-only:");
    result.assert_contains("unpublished wip");
    result.assert_contains("Read-only report: nothing was fetched or deleted.");
    assert_eq!(git(&fixture.work, &["for-each-ref"]), refs_before);
    assert_eq!(
        std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .ok(),
        index_before
    );
}
