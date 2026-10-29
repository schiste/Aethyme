//! Routine status observes fresh coordination state without repairing it.
use aethyme_broker::{
    NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance, OperationProvider,
    OperationStatus,
};
use std::path::Path;
use std::process::{Command, Output};
mod common;
const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn isolated(command: &mut Command, fixture: &common::FixtureRepository) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", fixture.owner_path())
        .env("GIT_AUTHOR_NAME", "status-observation")
        .env("GIT_AUTHOR_EMAIL", "status@example.invalid")
        .env("GIT_COMMITTER_NAME", "status-observation")
        .env("GIT_COMMITTER_EMAIL", "status@example.invalid");
}
fn git(f: &common::FixtureRepository, args: &[&str]) -> String {
    let mut command = Command::new("git");
    isolated(&mut command, f);
    let result = command.args(args).current_dir(f.path()).output().unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_string()
}
fn fixture() -> common::FixtureRepository {
    let f = common::FixtureRepository::new();
    git(&f, &["init", "-q", "-b", "main"]);
    std::fs::write(
        f.path().join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/config.toml\n",
    )
    .unwrap();
    std::fs::write(f.path().join("README.md"), "fixture\n").unwrap();
    git(&f, &["add", "README.md", ".gitignore"]);
    git(&f, &["commit", "-qm", "fixture"]);
    git(&f, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &f,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    f
}
fn command(f: &common::FixtureRepository, args: &[&str]) -> Command {
    let mut command = common::broker_cli_for_fixture(CLI, args, f.path());
    isolated(&mut command, f);
    command
        .env_remove("AETHYME_MEASURE_OUTPUT")
        .current_dir(f.path());
    command
}
fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn routine_status_does_not_create_database_or_move_primary_checkout() {
    let f = fixture();
    let initial = git(&f, &["rev-parse", "HEAD"]);
    std::fs::write(f.path().join("README.md"), "advance\n").unwrap();
    git(&f, &["commit", "-qam", "advance"]);
    git(&f, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&f, &["reset", "--hard", &initial]);
    let output = command(&f, &["status", "--summary", "--json"])
        .output()
        .unwrap();
    let payload = json(&output);
    assert!(
        payload["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "startup_recovery")
    );
    assert_eq!(
        git(&f, &["rev-parse", "HEAD"]),
        initial,
        "status moved the checkout"
    );
    assert!(
        !f.path().join(".aethyme/broker.db").exists(),
        "status created broker state"
    );
    assert!(
        !common::fixture_host_state_dir(f.path()).exists(),
        "status created host state"
    );
}

#[test]
fn routine_status_keeps_abandoned_prepared_operation_visible_without_reaping_it() {
    let f = fixture();
    let mut broker = common::open_fixture_repository_broker(&f).unwrap();
    let worktree = f.worktree_root().join("agent");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &f,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent",
            worktree.to_str().unwrap(),
        ],
    );
    let session = broker.adopt(&worktree, Some("agent")).unwrap();
    let row = broker
        .store()
        .create_coordinated_operation(&NewCoordinatedOperation {
            session_id: session.id,
            provider: OperationProvider::Git,
            repository: format!("local:{}", f.path().canonicalize().unwrap().display()),
            scope: "repository".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("test abandoned preparation".into()),
            command_json: r#"["git","push"]"#.into(),
            pid: 999_999,
            host_operation_id: None,
            identity_provenance: OperationIdentityProvenance::LocalRepository,
        })
        .unwrap();
    let mut child = command(&f, &["status", "--compact", "--json"]);
    broker
        .configure_storage_context_for_child(&mut child)
        .unwrap();
    let payload = json(&child.output().unwrap());
    assert_eq!(
        broker
            .store()
            .coordinated_operation(row.id)
            .unwrap()
            .expect("prepared operation retained")
            .status,
        OperationStatus::Prepared,
        "routine status performed startup recovery"
    );
    assert_eq!(payload["coordinated_operations"]["count"], 1);
    assert_eq!(
        payload["coordinated_operations"]["records"][0]["id"],
        row.id
    );
    assert_eq!(
        payload["coordinated_operations"]["records"][0]["status"],
        "prepared"
    );
}

#[test]
fn summary_status_has_bounded_git_processes_and_no_checkout_scan() {
    let f = fixture();
    let _broker = common::open_fixture_repository_broker(&f).unwrap();
    std::fs::create_dir_all(f.path().join(".aethyme")).unwrap();
    std::fs::write(
        f.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"verify-only\"\n",
    )
    .unwrap();
    let trace = f.owner_path().join("trace.jsonl");
    let output = command(&f, &["status", "--summary", "--json"])
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
    json(&output);
    let starts: Vec<Vec<String>> = std::fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event"] == "start")
        .map(|event| {
            event["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        })
        .collect();
    assert!(
        starts.len() <= 12,
        "routine status repeated setup subprocesses: {starts:?}"
    );
    let committed_config_tree_reads = starts
        .iter()
        .filter(|args| {
            args.get(1).is_some_and(|arg| arg == "ls-tree")
                && args.iter().any(|arg| arg.ends_with("config.toml"))
        })
        .count();
    assert_eq!(
        committed_config_tree_reads, 1,
        "routine status performs one checked lookup for all committed config twins: {starts:?}"
    );
    for args in &starts {
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "status" | "ls-files" | "fetch" | "merge")),
            "routine status scanned or changed the checkout: {args:?}"
        );
    }
    for flag in ["--show-toplevel", "--git-common-dir"] {
        assert_eq!(
            starts
                .iter()
                .filter(|args| args.iter().any(|arg| arg == flag))
                .count(),
            1,
            "repository identity was rediscovered: {starts:?}"
        );
    }
}

#[test]
fn committed_config_changes_are_visible_on_the_next_observation() {
    let f = fixture();
    let _broker = common::open_fixture_repository_broker(&f).unwrap();
    for mode in ["invalid-first", "invalid-second"] {
        std::fs::create_dir_all(f.path().join(".aethyme")).unwrap();
        std::fs::write(
            f.path().join(".aethyme/config.toml"),
            format!("[promote]\nmode = \"{mode}\"\n"),
        )
        .unwrap();
        git(&f, &["add", ".aethyme/config.toml"]);
        git(&f, &["commit", "-qm", "change committed policy"]);
        git(&f, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let payload = json(
            &command(&f, &["status", "--summary", "--json"])
                .output()
                .unwrap(),
        );
        assert!(
            payload.to_string().contains(mode),
            "stale config: {payload}"
        );
    }
}

#[test]
fn status_context_is_scoped_to_one_invocation_even_in_one_process() {
    let args: Vec<String> = ["status", "--summary", "--json"]
        .into_iter()
        .map(String::from)
        .collect();
    let f = fixture();
    let observed = || {
        aethyme_broker::cli::with_status_read_context(&args, || {
            let repo = aethyme_broker::GitRepo::discover(f.path()).unwrap();
            repo.git_common_dir().unwrap()
        })
    };
    let before = observed();
    let moved = f.owner_path().join("moved-metadata");
    std::fs::rename(f.path().join(".git"), &moved).unwrap();
    std::fs::write(
        f.path().join(".git"),
        format!("gitdir: {}\n", moved.display()),
    )
    .unwrap();
    assert_ne!(
        before,
        observed(),
        "identity leaked between status invocations"
    );
    assert!(Path::new(&moved).is_dir());
}

#[test]
fn observation_does_not_record_events_and_timing_requires_explicit_opt_in() {
    let f = fixture();
    let mut broker = common::open_fixture_repository_broker(&f).unwrap();
    let before = broker.store().events_after(0, 1000).unwrap().len();
    let metrics = f.path().join(".aethyme/logs/command-metrics.jsonl");
    let before_metrics = std::fs::read(&metrics).unwrap_or_default();
    // --json-output is the supported alias, including on the observation path.
    json(
        &command(&f, &["status", "--summary", "--json-output"])
            .output()
            .unwrap(),
    );
    assert_eq!(broker.store().events_after(0, 1000).unwrap().len(), before);
    assert_eq!(std::fs::read(&metrics).unwrap_or_default(), before_metrics);
    json(
        &command(&f, &["status", "--summary", "--json-output"])
            .env("AETHYME_MEASURE_OUTPUT", "1")
            .output()
            .unwrap(),
    );
    let after_metrics = std::fs::read_to_string(&metrics).unwrap();
    assert_eq!(
        after_metrics.lines().count(),
        String::from_utf8(before_metrics).unwrap().lines().count() + 1
    );
    assert_eq!(broker.store().events_after(0, 1000).unwrap().len(), before);
}
