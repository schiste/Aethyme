//! A `core.hooksPath` that names a missing, non-directory, or hook-less
//! directory makes git skip every hook without a word. `doctor` reports it
//! with the scope that set it and a suggested fix, and never changes the
//! config.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;
use aethyme_broker::hooks::{HooksPathProblem, inspect_hooks_path};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "README.md"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

fn ship_husky_dir(repo: &Path) {
    std::fs::create_dir_all(repo.join(".husky")).unwrap();
    std::fs::write(repo.join(".husky/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn host_state_dir(repo: &Path) -> std::path::PathBuf {
    repo.join(".aethyme/test-host-state")
}

fn doctor_json(repo: &Path, global_config: &Path) -> serde_json::Value {
    drop(
        Broker::open(repo)
            .unwrap()
            .with_host_operation_database(host_state_dir(repo).join("host-operations.db")),
    );
    let output = run(repo, global_config, &["status", "doctor", "--json"]);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor --json: {error}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn run(repo: &Path, global_config: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", host_state_dir(repo))
        .env("GIT_CONFIG_GLOBAL", global_config)
        .output()
        .unwrap()
}

#[test]
fn unset_hooks_path_is_not_a_finding() {
    let tmp = fixture();
    ship_husky_dir(tmp.path());
    assert_eq!(inspect_hooks_path(tmp.path()), None);
}

#[test]
fn missing_relative_hooks_path_resolves_against_the_checkout_root() {
    let tmp = fixture();
    git(tmp.path(), &["config", "core.hooksPath", "gone/hooks"]);
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();

    // Asked from a subdirectory, git still resolves hooksPath against the
    // top of the working tree.
    let finding = inspect_hooks_path(&tmp.path().join("sub")).expect("a finding");
    assert_eq!(finding.problem, HooksPathProblem::Missing);
    assert_eq!(finding.configured, "gone/hooks");
    assert_eq!(finding.scope, "local");
    let root = tmp.path().canonicalize().unwrap();
    assert_eq!(
        Path::new(&finding.resolved).parent().unwrap(),
        root.join("gone"),
        "{finding:?}"
    );
    assert_eq!(finding.fix, "git config --unset core.hooksPath");
}

#[test]
fn hooks_path_naming_a_file_is_not_a_directory() {
    let tmp = fixture();
    std::fs::write(tmp.path().join("hooks-file"), "x").unwrap();
    git(tmp.path(), &["config", "core.hooksPath", "hooks-file"]);
    let finding = inspect_hooks_path(tmp.path()).expect("a finding");
    assert_eq!(finding.problem, HooksPathProblem::NotADirectory);
}

#[test]
fn shipped_install_script_is_the_suggested_fix() {
    let tmp = fixture();
    ship_husky_dir(tmp.path());
    std::fs::create_dir_all(tmp.path().join("scripts")).unwrap();
    std::fs::write(
        tmp.path().join("scripts/install-git-hooks.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    git(
        tmp.path(),
        &["config", "core.hooksPath", "/nonexistent/old-clone/.husky"],
    );
    let finding = inspect_hooks_path(tmp.path()).expect("a finding");
    assert_eq!(finding.problem, HooksPathProblem::Missing);
    assert_eq!(finding.fix, "./scripts/install-git-hooks.sh");
}

#[test]
fn shipped_hooks_dir_is_suggested_without_an_install_script() {
    let tmp = fixture();
    ship_husky_dir(tmp.path());
    git(
        tmp.path(),
        &["config", "core.hooksPath", "/nonexistent/old-clone/.husky"],
    );
    let finding = inspect_hooks_path(tmp.path()).expect("a finding");
    assert_eq!(finding.fix, "git config core.hooksPath .husky");
    assert!(finding.shipped_hooks_dir.is_some(), "{finding:?}");
}

#[cfg(unix)]
#[test]
fn hook_less_directory_is_a_finding_only_while_the_repo_ships_hooks() {
    let tmp = fixture();
    std::fs::create_dir_all(tmp.path().join("empty-hooks")).unwrap();
    git(tmp.path(), &["config", "core.hooksPath", "empty-hooks"]);
    // No shipped hooks: an empty hooks dir is a deliberate "no hooks".
    assert_eq!(inspect_hooks_path(tmp.path()), None);

    ship_husky_dir(tmp.path());
    let finding = inspect_hooks_path(tmp.path()).expect("a finding");
    assert_eq!(finding.problem, HooksPathProblem::NoExecutableHooks);

    // A non-executable hook does not run either.
    let hook = tmp.path().join("empty-hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    assert_eq!(
        inspect_hooks_path(tmp.path()).map(|finding| finding.problem),
        Some(HooksPathProblem::NoExecutableHooks)
    );

    make_executable(&hook);
    assert_eq!(inspect_hooks_path(tmp.path()), None);
}

#[test]
fn doctor_reports_a_global_hooks_path_with_its_scope_and_fix() {
    let tmp = fixture();
    ship_husky_dir(tmp.path());
    let global = tmp.path().join("global.gitconfig");
    std::fs::write(
        &global,
        "[core]\n\thooksPath = /nonexistent/Downloads/old-clone/.husky\n",
    )
    .unwrap();

    let report = doctor_json(tmp.path(), &global);
    let finding = &report["hooks_path"];
    assert_eq!(finding["problem"], "missing", "{report}");
    assert_eq!(finding["scope"], "global", "{report}");
    assert_eq!(
        finding["fix"], "git config --global core.hooksPath .husky",
        "{report}"
    );

    let text = String::from_utf8_lossy(&run(tmp.path(), &global, &["status", "doctor"]).stdout)
        .into_owned();
    assert!(
        text.contains("git hooks: core.hooksPath")
            && text.contains("does not exist")
            && text.contains("fix: git config --global core.hooksPath .husky"),
        "{text}"
    );

    // Warn only: the config is untouched.
    assert!(
        std::fs::read_to_string(&global)
            .unwrap()
            .contains("/nonexistent/Downloads/old-clone/.husky")
    );
}

#[test]
fn doctor_omits_the_section_when_hooks_path_is_healthy() {
    let tmp = fixture();
    let global = tmp.path().join("global.gitconfig");
    std::fs::write(&global, "").unwrap();
    let report = doctor_json(tmp.path(), &global);
    assert!(report.get("hooks_path").is_none(), "{report}");
}
