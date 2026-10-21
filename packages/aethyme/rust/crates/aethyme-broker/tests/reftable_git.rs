//! Reftable fixtures for the first-party Git boundary.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::GitRepo;

fn is_null_device(value: &OsStr) -> bool {
    let value = value.to_string_lossy();
    value == "/dev/null" || value.eq_ignore_ascii_case("NUL")
}

fn assert_git_environment_isolated() {
    const ROUTING_VARS: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_SHALLOW_FILE",
        "GIT_REPLACE_REF_BASE",
        "GIT_QUARANTINE_PATH",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ];
    for name in ROUTING_VARS {
        assert!(
            std::env::var_os(name).is_none(),
            "reftable Git boundary tests require an unredirected Git environment; unset {name}"
        );
    }

    for (key, value) in std::env::vars_os() {
        let name = key.to_string_lossy();
        let upper_name = name.to_ascii_uppercase();
        let invalid_config = upper_name == "GIT_CONFIG_COUNT"
            || upper_name == "GIT_CONFIG_PARAMETERS"
            || upper_name.starts_with("GIT_CONFIG_KEY_")
            || upper_name.starts_with("GIT_CONFIG_VALUE_");
        let invalid_config_source = match upper_name.as_str() {
            "GIT_CONFIG_GLOBAL" | "GIT_CONFIG_SYSTEM" | "GIT_ATTR_GLOBAL" => {
                !is_null_device(&value)
            }
            "GIT_CONFIG_NOSYSTEM" | "GIT_ATTR_NOSYSTEM" => value.to_string_lossy() != "1",
            _ => false,
        };
        assert!(
            !invalid_config && !invalid_config_source,
            "reftable Git boundary tests require no command-scope config injection; unset {name} or set config files to /dev/null"
        );
    }
}

fn git_output(root: &Path, args: &[&str]) -> Output {
    let parent = root.parent().expect("fixture repo parent");
    let empty_config = parent.join("empty-global-config");
    let hooks = parent.join("empty-hooks");
    let mut command = Command::new("git");
    command.env_clear();
    if let Some(system_root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", system_root);
    }

    command
        .env("PATH", std::env::var_os("PATH").expect("test PATH"))
        .env("HOME", parent)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", &empty_config)
        .env("GIT_CONFIG_GLOBAL", &empty_config)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_ATTR_GLOBAL", &empty_config)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "reftable fixture")
        .env("GIT_AUTHOR_EMAIL", "reftable@example.test")
        .env("GIT_COMMITTER_NAME", "reftable fixture")
        .env("GIT_COMMITTER_EMAIL", "reftable@example.test")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "init.templateDir=",
        ])
        .arg("-c")
        .arg(format!("core.attributesFile={}", empty_config.display()))
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git")
}

fn run_git(root: &Path, args: &[&str]) {
    let output = git_output(root, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_reftable_repo(root: &Path) {
    let template = root
        .parent()
        .expect("fixture repo parent")
        .join("empty-template");
    let template_arg = format!("--template={}", template.display());
    let output = git_output(
        root,
        &[
            "init",
            "-q",
            template_arg.as_str(),
            "--ref-format=reftable",
            "-b",
            "main",
        ],
    );
    assert!(
        output.status.success(),
        "this fixture requires Git with reftable support: `git init --ref-format=reftable -b main` failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let storage = git_output(root, &["config", "--local", "--get", "extensions.refStorage"]);
    assert!(
        storage.status.success(),
        "Git accepted --ref-format=reftable but did not record extensions.refStorage: {}",
        String::from_utf8_lossy(&storage.stderr).trim()
    );
    assert_eq!(
        String::from_utf8_lossy(&storage.stdout).trim(),
        "reftable",
        "the fixture must use Git's reftable ref store"
    );

    let hooks = root
        .parent()
        .expect("fixture repo parent")
        .join("empty-hooks");
    run_git(
        root,
        &[
            "config",
            "--local",
            "core.hooksPath",
            hooks.to_str().expect("hooks path"),
        ],
    );
    run_git(root, &["config", "--local", "commit.gpgsign", "false"]);
}

fn reftable_repo() -> (tempfile::TempDir, GitRepo, String, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir(&root).expect("create fixture repo");
    std::fs::create_dir(tmp.path().join("empty-template")).expect("create empty template");
    std::fs::create_dir(tmp.path().join("empty-hooks")).expect("create empty hooks directory");
    std::fs::write(tmp.path().join("empty-global-config"), "")
        .expect("create empty global config");
    init_reftable_repo(&root);

    std::fs::write(root.join("README.md"), "first\n").expect("write first commit");
    run_git(&root, &["add", "README.md"]);
    run_git(&root, &["commit", "-qm", "first"]);
    let repo = GitRepo::discover(&root).expect("discover reftable repo");
    assert_eq!(
        repo.root().canonicalize().expect("canonical GitRepo root"),
        root.canonicalize().expect("canonical fixture root"),
        "GitRepo discovery must stay inside the explicit fixture"
    );
    let first = repo
        .resolve_ref("refs/heads/main")
        .expect("first commit ref");
    run_git(&root, &["branch", "agent/kept"]);

    std::fs::write(root.join("README.md"), "second\n").expect("write second commit");
    run_git(&root, &["commit", "-qam", "second"]);
    run_git(&root, &["branch", "agent/moved"]);
    let second = repo
        .resolve_ref("refs/heads/main")
        .expect("second commit ref");

    (tmp, repo, first, second)
}

#[test]
#[ignore = "requires Git reftable support; run explicitly for #904 assessment"]
fn reftable_local_branch_tips_matches_every_ref_resolve_reports() {
    assert_git_environment_isolated();
    let (_tmp, repo, first, second) = reftable_repo();

    let tips = repo.local_branch_tips().expect("list local branch tips");
    for name in [
        "refs/heads/main",
        "refs/heads/agent/kept",
        "refs/heads/agent/moved",
    ] {
        assert_eq!(
            tips.get(name),
            repo.resolve_ref(name).as_ref(),
            "the boundary inventory should match Git's exact ref resolution for {name}"
        );
    }
    assert_eq!(tips["refs/heads/main"], second);
    assert_eq!(tips["refs/heads/agent/kept"], first);
    assert_eq!(tips["refs/heads/agent/moved"], second);
    assert_eq!(tips.len(), 3, "unexpected refs in inventory: {tips:?}");
    assert!(!tips.contains_key("refs/heads/agent/gone"));
    assert!(repo.resolve_ref("refs/heads/agent/gone").is_none());
}

#[test]
#[ignore = "requires Git reftable support; run explicitly for #904 assessment"]
fn reftable_checked_branch_update_rejects_stale_expected_old_and_can_restore() {
    assert_git_environment_isolated();
    let (_tmp, repo, first, second) = reftable_repo();
    let branch = "agent/kept";
    let reference = format!("refs/heads/{branch}");

    repo.update_branch_ref_checked(branch, &second, &first)
        .expect("advance branch from its exact old tip");
    assert_eq!(repo.resolve_ref(&reference).as_deref(), Some(second.as_str()));

    assert!(
        repo.update_branch_ref_checked(branch, &first, &first)
            .is_err(),
        "a stale expected old value must not overwrite the branch"
    );
    assert_eq!(
        repo.resolve_ref(&reference).as_deref(),
        Some(second.as_str()),
        "the rejected compare-and-swap must leave the branch untouched"
    );

    repo.update_branch_ref_checked(branch, &first, &second)
        .expect("restore branch from its exact updated tip");
    assert_eq!(repo.resolve_ref(&reference).as_deref(), Some(first.as_str()));

    let tips = repo.local_branch_tips().expect("list restored branch tips");
    for name in [
        "refs/heads/main",
        "refs/heads/agent/kept",
        "refs/heads/agent/moved",
    ] {
        assert_eq!(
            tips.get(name),
            repo.resolve_ref(name).as_ref(),
            "the post-rollback inventory should match Git's exact ref resolution for {name}"
        );
    }
    assert_eq!(tips[&reference], first);
    assert_eq!(tips.len(), 3, "unexpected refs after rollback: {tips:?}");
}
