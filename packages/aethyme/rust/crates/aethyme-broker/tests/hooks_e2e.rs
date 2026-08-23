//! End-to-end hook tests: real repos, real `git commit` runs through the
//! installed shims. The shims embed `broker-cli-shim` (this crate's
//! router stand-in) as the aethyme binary, so the full path —
//! hook script → binary → gate/radar logic — is exercised.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, GitRepo, HookState, hooks};

const SHIM: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git_output(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

fn sh(cwd: &Path, args: &[&str]) {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/app.py"), "ok\n").unwrap();
    std::fs::write(root.join("docs.md"), "docs\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn write_gates(root: &Path, body: &str) {
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(root.join(".aethyme/gates.toml"), body).unwrap();
}

fn add_worktree(root: &Path, name: &str) -> PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn head_commit(root: &Path) -> String {
    let output = git_output(root, &["rev-parse", "HEAD"]);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn install_status_uninstall_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    let dir = hooks::hooks_dir(&repo).unwrap();

    // Before install: both absent.
    let before = hooks::status(&repo).unwrap();
    assert!(before.iter().all(|r| r.state == HookState::Absent));

    // Install writes both hooks: marker-delimited, executable, embedding
    // the binary path captured at install time.
    let reports = hooks::install(&repo, Path::new(SHIM)).unwrap();
    assert_eq!(reports.len(), 2);
    assert!(reports.iter().all(|r| r.state == HookState::Installed));
    for hook in hooks::MANAGED_HOOKS {
        let path = dir.join(hook);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(hooks::MARKER_BEGIN), "{hook} has marker");
        assert!(content.contains(hooks::MARKER_END));
        assert!(content.contains(SHIM), "{hook} embeds the binary path");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "{hook} is executable");
        }
    }
    let installed = hooks::status(&repo).unwrap();
    assert!(installed.iter().all(|r| r.state == HookState::Installed));

    // Re-install replaces only the marker block (idempotent refresh).
    let again = hooks::install(&repo, Path::new(SHIM)).unwrap();
    assert!(again.iter().all(|r| r.state == HookState::Updated));

    // User content outside the markers survives uninstall; a file that
    // was nothing but our shim is deleted outright.
    let pre_commit = dir.join("pre-commit");
    let mut content = std::fs::read_to_string(&pre_commit).unwrap();
    content.push_str("echo user-owned\n");
    std::fs::write(&pre_commit, &content).unwrap();

    let removed = hooks::uninstall(&repo).unwrap();
    assert_eq!(removed[0].state, HookState::Removed, "user content kept");
    assert_eq!(removed[1].state, HookState::Deleted, "pure shim deleted");
    let remainder = std::fs::read_to_string(&pre_commit).unwrap();
    assert!(remainder.contains("echo user-owned"));
    assert!(!remainder.contains(hooks::MARKER_BEGIN));
    assert!(!dir.join("post-commit").exists());

    let after = hooks::status(&repo).unwrap();
    assert_eq!(after[0].state, HookState::Foreign, "leftover user hook");
    assert_eq!(after[1].state, HookState::Absent);
}

#[test]
fn foreign_hook_file_is_refused_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    let dir = hooks::hooks_dir(&repo).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let user_hook = "#!/bin/sh\necho mine\n";
    std::fs::write(dir.join("pre-commit"), user_hook).unwrap();

    let err = hooks::install(&repo, Path::new(SHIM)).unwrap_err();
    assert!(
        err.to_string().contains("without the aethyme marker"),
        "clear refusal: {err}"
    );
    // All-or-nothing: the user hook is untouched AND the other hook was
    // not written either.
    assert_eq!(
        std::fs::read_to_string(dir.join("pre-commit")).unwrap(),
        user_hook
    );
    assert!(!dir.join("post-commit").exists());
}

#[test]
fn core_hooks_path_override_refuses_install() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    // A husky-style repo: git resolves hooks ONLY through core.hooksPath,
    // so anything written to <common>/hooks would silently never run.
    sh(tmp.path(), &["config", "core.hooksPath", ".husky"]);
    let repo = GitRepo::discover(tmp.path()).unwrap();

    let err = hooks::install(&repo, Path::new(SHIM)).unwrap_err();
    assert!(
        err.to_string().contains("core.hooksPath"),
        "names the override: {err}"
    );
    let dir = hooks::hooks_dir(&repo).unwrap();
    assert!(
        !dir.join("pre-commit").exists() && !dir.join("post-commit").exists(),
        "nothing written where git would ignore it"
    );

    // Pointing core.hooksPath explicitly AT the default dir is fine.
    sh(
        tmp.path(),
        &["config", "core.hooksPath", dir.to_str().unwrap()],
    );
    let reports = hooks::install(&repo, Path::new(SHIM)).unwrap();
    assert!(reports.iter().all(|r| r.state == HookState::Installed));
}

#[test]
fn pre_commit_gate_blocks_failing_commit_and_passes_clean_one() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "py-content"
command = "printf 'gate stdout one\\ngate stdout two\\n'; printf 'gate stderr one\\ngate stderr two\\n' >&2; grep -q ok src/app.py || exit 7"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "expensive-always-fails"
command = "exit 9"
cost = 2
triggers = ["**/*.py"]
"#,
    );
    let repo = GitRepo::discover(tmp.path()).unwrap();
    hooks::install(&repo, Path::new(SHIM)).unwrap();

    // Staged .py file failing the cost-1 gate → commit blocked, gate
    // named, HEAD unchanged.
    let head_before = head_commit(tmp.path());
    std::fs::write(tmp.path().join("src/app.py"), "bad\n").unwrap();
    sh(tmp.path(), &["add", "src/app.py"]);

    // The internal hook command preserves the gate's exit code and
    // replays both captured streams completely before its diagnosis.
    let direct = Command::new(SHIM)
        .args(["broker", "hooks", "pre-commit"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(direct.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&direct.stdout),
        "gate stdout one\ngate stdout two\n"
    );
    let direct_stderr = String::from_utf8_lossy(&direct.stderr);
    assert!(direct_stderr.contains("gate stderr one\ngate stderr two\n"));
    assert!(direct_stderr.contains("gate py-content failed (exit 7)"));
    assert!(direct_stderr.contains("git commit --no-verify"));

    let blocked = git_output(tmp.path(), &["commit", "-qm", "broken"]);
    let stdout = String::from_utf8_lossy(&blocked.stdout);
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(!blocked.status.success(), "failing gate blocks the commit");
    assert!(combined.contains("gate stdout one\ngate stdout two\n"));
    assert!(combined.contains("gate stderr one\ngate stderr two\n"));
    assert!(stderr.contains("py-content"), "names the gate: {stderr}");
    assert!(stderr.contains("exit 7"), "preserves diagnosis: {stderr}");
    assert_eq!(head_commit(tmp.path()), head_before, "nothing committed");

    // Docs-only staged change → no matching gate → instant pass, even
    // with the broken .py file still sitting unstaged in the worktree.
    sh(tmp.path(), &["restore", "--staged", "src/app.py"]);
    std::fs::write(tmp.path().join("docs.md"), "更新 docs\n").unwrap();
    sh(tmp.path(), &["add", "docs.md"]);
    let docs = git_output(tmp.path(), &["commit", "-qm", "docs"]);
    assert!(docs.status.success(), "docs-only commit passes instantly");
    assert!(
        !String::from_utf8_lossy(&docs.stderr).contains("aethyme pre-commit"),
        "no gate ran for a docs-only diff"
    );

    // Fixed .py content → the cost-1 gate passes and the failing cost-2
    // gate is never selected (cost <= 1 only at commit time).
    std::fs::write(tmp.path().join("src/app.py"), "ok fixed\n").unwrap();
    sh(tmp.path(), &["add", "src/app.py"]);
    let fixed = git_output(tmp.path(), &["commit", "-qm", "fixed"]);
    assert!(
        fixed.status.success(),
        "passing cost-1 gate allows the commit (cost-2 excluded): {}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    for output in [&fixed.stdout, &fixed.stderr] {
        let output = String::from_utf8_lossy(output);
        assert!(
            !output.contains("gate stdout"),
            "success stdout leaked: {output}"
        );
        assert!(
            !output.contains("gate stderr"),
            "success stderr leaked: {output}"
        );
        assert!(
            !output.contains("aethyme pre-commit"),
            "success progress leaked: {output}"
        );
    }

    // Hooks live in the common git dir: a linked worktree gets the same
    // policy with zero per-worktree setup.
    let wt = add_worktree(tmp.path(), "shared");
    std::fs::write(wt.join("src/app.py"), "bad again\n").unwrap();
    sh(&wt, &["add", "src/app.py"]);
    let wt_blocked = git_output(&wt, &["commit", "-qm", "broken in worktree"]);
    assert!(
        !wt_blocked.status.success(),
        "worktree commit goes through the shared hook"
    );
    assert!(String::from_utf8_lossy(&wt_blocked.stderr).contains("py-content"));
}

#[test]
fn post_commit_radar_warns_on_overlap_and_never_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    hooks::install(&repo, Path::new(SHIM)).unwrap();

    // Session A holds an implicit lease on src/app.py (uncommitted edit).
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_a = add_worktree(tmp.path(), "radar-a");
    let a = broker.adopt(&wt_a, Some("radar target")).unwrap();
    std::fs::write(wt_a.join("src/app.py"), "a's edit\n").unwrap();

    // Session B is the committer; its own lease on the same file must
    // not trigger a self-warning.
    let wt_b = add_worktree(tmp.path(), "radar-b");
    let b = broker.adopt(&wt_b, Some("committer")).unwrap();
    std::fs::write(wt_b.join("src/app.py"), "b's edit\n").unwrap();
    broker.refresh_leases().unwrap();

    sh(&wt_b, &["add", "src/app.py"]);
    let commit = git_output(&wt_b, &["commit", "-qm", "b commits the contested file"]);
    let stderr = String::from_utf8_lossy(&commit.stderr);
    assert!(commit.status.success(), "the radar never blocks: {stderr}");
    assert!(
        stderr.contains("conflict radar") && stderr.contains(&format!("session {}", a.id)),
        "warns naming the overlapping session: {stderr}"
    );
    assert!(stderr.contains("src/app.py"), "names the file: {stderr}");
    assert!(
        !stderr.contains(&format!("session {}", b.id)),
        "no self-warning for the committer's own session: {stderr}"
    );

    // A commit touching nothing leased by others stays silent.
    std::fs::write(wt_b.join("docs.md"), "quiet\n").unwrap();
    sh(&wt_b, &["add", "docs.md"]);
    let quiet = git_output(&wt_b, &["commit", "-qm", "docs only"]);
    assert!(quiet.status.success());
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("conflict radar"),
        "silent when nothing overlaps"
    );
}

#[test]
fn hooks_are_silent_without_broker_db_or_gates_config() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    hooks::install(&repo, Path::new(SHIM)).unwrap();
    // No .aethyme/gates.toml and no .aethyme/broker.db: pre-commit is an
    // instant pass, post-commit says nothing.
    std::fs::write(tmp.path().join("docs.md"), "plain repo\n").unwrap();
    sh(tmp.path(), &["add", "docs.md"]);
    let commit = git_output(tmp.path(), &["commit", "-qm", "no broker state"]);
    let stderr = String::from_utf8_lossy(&commit.stderr);
    assert!(commit.status.success(), "{stderr}");
    assert!(!stderr.contains("aethyme"), "fully silent: {stderr}");
}
