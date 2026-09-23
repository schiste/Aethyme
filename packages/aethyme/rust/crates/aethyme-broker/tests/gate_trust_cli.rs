//! Trust on first use for repository-defined commands, end to end through the
//! CLI: nothing a cloned `.aethyme/gates.toml` declares may run until a human
//! on this machine trusts it, and a landed policy change needs trusting again.
//!
//! Every command here runs with its own host state directory and WITHOUT the
//! test-only escape the rest of the suite inherits from `.cargo/config.toml`,
//! except where a step sets it on purpose.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use aethyme_broker::{GitRepo, hooks};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const ESCAPE: &str = "AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS";
const REFUSED: i32 = 3;

struct Fixture {
    repo: tempfile::TempDir,
    host: tempfile::TempDir,
}

impl Fixture {
    /// A fresh "clone": a gates.toml whose single gate writes `marker`, and no
    /// broker history.
    fn new(marker: &str) -> Self {
        let repo = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        let fixture = Self { repo, host };
        let root = fixture.root();
        fixture.git(&root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(root.join("tracked.txt"), "first\n").unwrap();
        std::fs::write(
            root.join(".gitignore"),
            "*.marker\n.aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n",
        )
        .unwrap();
        fixture.write_gate(marker);
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[graph]\nauthority='disabled'\n",
        )
        .unwrap();
        fixture.git(&root, &["add", "-A"]);
        fixture.git(&root, &["commit", "-qm", "fixture"]);
        fixture
    }

    fn root(&self) -> PathBuf {
        self.repo.path().canonicalize().unwrap()
    }

    /// The gate appends to `marker` in the main checkout, so a run from any
    /// worktree or verification slot is visible in one place.
    fn write_gate(&self, marker: &str) {
        let target = self.root().join(marker);
        std::fs::write(
            self.root().join(".aethyme/gates.toml"),
            format!(
                "[[gate]]\nname = \"marker\"\ncommand = \"echo ran >> '{}'\"\ncost = 1\ncache = false\n",
                target.display()
            ),
        )
        .unwrap();
    }

    fn marker(&self, marker: &str) -> bool {
        self.root().join(marker).exists()
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(cwd)
            .env("AETHYME_HOST_STATE_DIR", self.host.path())
            .env_remove(ESCAPE)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdin(Stdio::null());
        command
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.command("git", cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(CLI, cwd).args(args).output().unwrap()
    }

    /// `broker trust` through the test-only escape: no terminal, no prompt.
    fn trust(&self) {
        let output = self
            .command(CLI, &self.root())
            .env(ESCAPE, "1")
            .args(["trust", "--repo", self.root().to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        succeeded(&output);
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            !report["recorded"].as_array().unwrap().is_empty(),
            "trust recorded nothing: {report}"
        );
    }

    fn trust_status(&self) -> serde_json::Value {
        let output = self.run(&self.root(), &["trust", "status", "--json"]);
        succeeded(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Adopt a linked worktree on `main` with one committed change.
    fn session(&self, name: &str) -> (PathBuf, String) {
        let worktree = self.root().join(".aethyme/worktrees").join(name);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        self.git(
            &self.root(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("agent/{name}"),
                worktree.to_str().unwrap(),
                "main",
            ],
        );
        let adopted = self.run(&worktree, &["adopt", "--task", name, "--json"]);
        succeeded(&adopted);
        let adopted: serde_json::Value = serde_json::from_slice(&adopted.stdout).unwrap();
        let session = adopted["id"].as_i64().unwrap().to_string();
        self.commit_change(&worktree, name);
        (worktree, session)
    }

    fn commit_change(&self, worktree: &Path, name: &str) {
        std::fs::write(worktree.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
        self.git(worktree, &["add", "-A"]);
        self.git(worktree, &["commit", "-qm", name]);
    }
}

fn succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_refused_with_hint(output: &Output, root: &Path) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(REFUSED),
        "stdout={}\nstderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains(&format!("aethyme broker trust --repo {}", root.display()))
            || stderr.contains(&format!("aethyme broker trust --repo '{}'", root.display())),
        "refusal does not name the trust command: {stderr}"
    );
}

#[test]
fn a_new_clone_refuses_gates_and_submit_until_trusted() {
    let fixture = Fixture::new("gate.marker");
    let root = fixture.root();

    let all = fixture.run(&root, &["gates", "run", "--all", "--json"]);
    assert_refused_with_hint(&all, &root);
    let (worktree, session) = fixture.session("first");
    let session_gates = fixture.run(&worktree, &["gates", "run", "--session", &session]);
    assert_refused_with_hint(&session_gates, &root);
    let submit = fixture.run(&worktree, &["submit", "--session", &session, "--json"]);
    assert_refused_with_hint(&submit, &root);
    assert!(
        !fixture.marker("gate.marker"),
        "a gate command ran before its policy was trusted"
    );
    assert_eq!(fixture.trust_status()["trusted"], false);

    // After a human trusts it, the same commands run -- without the escape.
    fixture.trust();
    assert_eq!(fixture.trust_status()["trusted"], true);
    succeeded(&fixture.run(&root, &["gates", "run", "--all", "--json"]));
    assert!(fixture.marker("gate.marker"));
    std::fs::remove_file(root.join("gate.marker")).unwrap();
    succeeded(&fixture.run(&worktree, &["submit", "--session", &session, "--json"]));
    assert!(fixture.marker("gate.marker"), "submit did not run the gate");
}

#[test]
fn a_landed_gate_change_refuses_the_next_submit_until_trusted_again() {
    let fixture = Fixture::new("before.marker");
    let root = fixture.root();
    fixture.trust();
    let (worktree, session) = fixture.session("work");
    let verify = ["submit", "--session", &session, "--verify-only", "--json"];
    succeeded(&fixture.run(&worktree, &verify));
    assert!(fixture.marker("before.marker"));

    // The gate command changes on the base: a commit on main, which the
    // integration branch follows because nothing was promoted.
    fixture.write_gate("after.marker");
    fixture.git(&root, &["commit", "-qam", "change the gate command"]);
    fixture.commit_change(&worktree, "more");

    let refused = fixture.run(&worktree, &verify);
    assert_refused_with_hint(&refused, &root);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("changed since it was last trusted"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !fixture.marker("after.marker"),
        "the changed gate ran before it was trusted"
    );

    fixture.trust();
    succeeded(&fixture.run(&worktree, &verify));
    assert!(fixture.marker("after.marker"));
}

#[test]
fn a_repository_with_gate_history_is_grandfathered() {
    let fixture = Fixture::new("gate.marker");
    let root = fixture.root();
    // History from before trust existed: the escape runs without recording.
    let seeded = fixture
        .command(CLI, &root)
        .env(ESCAPE, "1")
        .args(["gates", "run", "--all", "--json"])
        .output()
        .unwrap();
    succeeded(&seeded);
    assert_eq!(fixture.trust_status()["trusted"], false);
    std::fs::remove_file(root.join("gate.marker")).unwrap();

    succeeded(&fixture.run(&root, &["gates", "run", "--all", "--no-cache", "--json"]));
    assert!(fixture.marker("gate.marker"));
    let status = fixture.trust_status();
    assert_eq!(status["trusted"], true, "{status}");
    assert_eq!(status["trusted_policies"][0]["source"], "grandfathered");
    let events = fixture.run(&root, &["events", "--json"]);
    succeeded(&events);
    assert!(String::from_utf8_lossy(&events.stdout).contains("gate.policy_trust_grandfathered"));
}

#[test]
fn trust_refuses_without_a_terminal() {
    let fixture = Fixture::new("gate.marker");
    let root = fixture.root();
    let refused = fixture.run(&root, &["trust", "--repo", root.to_str().unwrap()]);
    assert_eq!(refused.status.code(), Some(REFUSED));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("interactive terminal"));
    assert_eq!(fixture.trust_status()["trusted"], false);
    assert_refused_with_hint(&fixture.run(&root, &["gates", "run", "--all"]), &root);
}

#[test]
fn the_pre_commit_hook_refuses_an_untrusted_policy() {
    let fixture = Fixture::new("gate.marker");
    let root = fixture.root();
    hooks::install(&GitRepo::discover(&root).unwrap(), Path::new(CLI)).unwrap();
    std::fs::write(root.join("tracked.txt"), "second\n").unwrap();
    fixture.git(&root, &["add", "tracked.txt"]);
    let head = fixture.git(&root, &["rev-parse", "HEAD"]).stdout;

    let direct = fixture.run(&root, &["broker", "hooks", "pre-commit"]);
    assert_refused_with_hint(&direct, &root);
    let commit = fixture
        .command("git", &root)
        .args(["commit", "-qm", "untrusted"])
        .output()
        .unwrap();
    assert!(!commit.status.success(), "the commit went through");
    assert!(String::from_utf8_lossy(&commit.stderr).contains("aethyme broker trust"));
    assert!(!fixture.marker("gate.marker"), "the hook ran the gate");
    assert_eq!(fixture.git(&root, &["rev-parse", "HEAD"]).stdout, head);

    fixture.trust();
    fixture.git(&root, &["commit", "-qm", "trusted"]);
    assert!(fixture.marker("gate.marker"));
}
