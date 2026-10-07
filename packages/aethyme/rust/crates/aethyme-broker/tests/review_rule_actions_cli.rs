//! #584 part B: rules act on the measured classification -- a mention for a
//! large change, an automatic waiver bound to the head for a trivial one.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

const WAIVE_TRIVIAL: &str = r#"
[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "trivial-code"
waive = ["code"]
max_tier = "trivial"

[[review.trigger.rule]]
name = "large-code"
require = ["code"]
min_tier = "large"
"#;

struct Fixture {
    root: tempfile::TempDir,
    fake_bin: PathBuf,
}

impl Fixture {
    /// A repository whose `main` carries `policy`, and a fake `gh` that
    /// describes pull request 7 from `$FAKE_PR_FILES` at `$FAKE_PR_HEAD`.
    fn new(policy: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        git(path, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(path.join(".aethyme")).unwrap();
        std::fs::write(path.join(".aethyme/config.toml"), policy).unwrap();
        std::fs::write(
            path.join(".gitignore"),
            "/.aethyme/broker.db*\n/host-state/\n/fake-bin/\n",
        )
        .unwrap();
        std::fs::write(path.join("README.md"), "initial\n").unwrap();
        git(path, &["add", "-A"]);
        git(path, &["commit", "-qm", "initial"]);
        git(
            path,
            &["remote", "add", "origin", "git@github.com:acme/product.git"],
        );
        let fake_bin = path.join("fake-bin");
        std::fs::create_dir(&fake_bin).unwrap();
        let gh = fake_bin.join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  printf '{"number":7,"state":"OPEN","isDraft":false,"isCrossRepository":false,"authorAssociation":"MEMBER","baseRefName":"main","headRefOid":"%s","labels":[],"comments":[],"commits":[],"files":%s}\n' "$FAKE_PR_HEAD" "$FAKE_PR_FILES"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "diff" ]; then
  exit 0
fi
if [ "$1" = "api" ]; then
  printf '[]\n'
  exit 0
fi
echo "unexpected gh $*" >&2
exit 1
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).unwrap();
        Self { root, fake_bin }
    }

    fn broker(&self, args: &[&str], head: &str, files: &str) -> serde_json::Value {
        let path = format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = common::broker_cli(CLI, args)
            .current_dir(self.root.path())
            .env("PATH", path)
            .env(
                "AETHYME_HOST_STATE_DIR",
                self.root.path().join("host-state"),
            )
            .env("AETHYME_AGENT_PID", std::process::id().to_string())
            .env("FAKE_PR_HEAD", head)
            .env("FAKE_PR_FILES", files)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null)
    }

    fn run(&self, head: &str, files: &str) {
        self.broker(
            &[
                "advanced",
                "review",
                "run",
                "--session",
                "1",
                "--repo",
                "acme/product",
                "--pr",
                "7",
                "--from-provider",
                "--json",
            ],
            head,
            files,
        );
    }

    fn ledger(&self) -> Vec<serde_json::Value> {
        let value = self.broker(
            &[
                "advanced",
                "review",
                "ledger",
                "--repo",
                "acme/product",
                "--pr",
                "7",
                "--json",
            ],
            "",
            "[]",
        );
        value
            .as_array()
            .cloned()
            .or_else(|| value["requests"].as_array().cloned())
            .unwrap_or_default()
    }
}

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TRIVIAL: &str = r#"[{"path":"docs/a.md","additions":3,"deletions":0}]"#;
const LARGE: &str = r#"[{"path":"src/lib.rs","additions":2000,"deletions":0}]"#;
const TRIVIAL_WORKFLOW: &str =
    r#"[{"path":".github/workflows/ci.yml","additions":1,"deletions":0}]"#;

fn waivers(ledger: &[serde_json::Value]) -> Vec<(String, String)> {
    ledger
        .iter()
        .filter(|row| row["state"] == "waived")
        .map(|row| {
            (
                row["review_type"].as_str().unwrap().to_string(),
                row["head_commit"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_trivial_change_is_auto_waived_at_its_head_and_a_new_head_is_not() {
    let fixture = Fixture::new(WAIVE_TRIVIAL);
    fixture.run(HEAD_A, TRIVIAL);
    let ledger = fixture.ledger();
    assert_eq!(
        waivers(&ledger),
        vec![("code".to_string(), HEAD_A.to_string())],
        "{ledger:#?}"
    );
    let row = ledger.iter().find(|row| row["state"] == "waived").unwrap();
    let detail = row["detail"].as_str().unwrap();
    assert!(
        detail.contains("aethyme review rule `trivial-code`"),
        "{detail}"
    );
    assert!(
        detail.contains("trivial change, 1 files, 3 changed lines"),
        "{detail}"
    );

    // The next head is large: the waiver stays bound to HEAD_A, and nothing
    // waives HEAD_B.
    fixture.run(HEAD_B, LARGE);
    assert_eq!(
        waivers(&fixture.ledger()),
        vec![("code".to_string(), HEAD_A.to_string())]
    );

    // Re-running the same head writes nothing new.
    fixture.run(HEAD_A, TRIVIAL);
    assert_eq!(waivers(&fixture.ledger()).len(), 1);
}

#[test]
fn a_guarded_signal_keeps_the_review_even_when_the_rule_matches() {
    let fixture = Fixture::new(WAIVE_TRIVIAL);
    fixture.run(HEAD_A, TRIVIAL_WORKFLOW);
    assert!(waivers(&fixture.ledger()).is_empty());
}

#[test]
fn plan_shows_the_mention_for_a_large_change_only_and_the_waiver_for_a_trivial_one() {
    let policy = format!(
        "{WAIVE_TRIVIAL}\n[review.routing]\nenabled = true\n\n\
         [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n\n\
         [review.projection]\nenabled = true\n"
    );
    let fixture = Fixture::new(&policy);
    let root = fixture.root.path();
    git(root, &["checkout", "-q", "-b", "change"]);
    std::fs::write(root.join("docs.md"), "one\ntwo\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "trivial"]);
    let plan_args = [
        "advanced",
        "review",
        "plan",
        "--base",
        "main",
        "--pr",
        "7",
        "--repo",
        "acme/product",
    ];
    let plan = fixture.broker(&plan_args, "", "[]");
    assert_eq!(plan["change"]["tier"], "trivial");
    assert_eq!(plan["dispatch"], serde_json::json!([]), "{plan:#}");
    assert_eq!(plan["rule_actions"]["waivers"][0]["review_type"], "code");
    let labels: Vec<String> = plan["projection"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["action"] == "add_labels")
        .flat_map(|action| action["names"].as_array().unwrap().clone())
        .map(|name| name.as_str().unwrap().to_string())
        .collect();
    assert!(
        labels.contains(&"aethyme/review:waived".to_string()),
        "{labels:?}"
    );

    let lines: String = (0..900).map(|line| format!("line {line}\n")).collect();
    std::fs::write(root.join("big.rs"), lines).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "large"]);
    let plan = fixture.broker(&plan_args, "", "[]");
    assert_eq!(plan["change"]["tier"], "large");
    assert!(
        plan["rule_actions"]["waivers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let dispatch = plan["dispatch"].to_string();
    assert!(dispatch.contains("@codex"), "{dispatch}");
}
