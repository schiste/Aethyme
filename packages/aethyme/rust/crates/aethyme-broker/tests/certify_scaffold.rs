//! Certify/scaffold split tests: certify is ALWAYS read-only and
//! deterministic; scaffold generates drafts (byte-identical on re-run,
//! never overwriting); manifest detection.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use aethyme_broker::init::{self, CheckStatus};

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hi\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

/// Snapshot every file under `.aethyme` + `.gitignore` (path → bytes),
/// excluding the runtime db (its WAL bytes legitimately change).
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for rel in [".aethyme/gates.toml", ".aethyme/config.toml", ".gitignore"] {
        if let Ok(bytes) = std::fs::read(root.join(rel)) {
            out.insert(rel.to_string(), bytes);
        }
    }
    out
}

fn status_of(report: &init::InitReport, id: &str) -> CheckStatus {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("missing check {id}"))
        .status
}

#[test]
fn certify_is_always_read_only_and_scaffold_rerun_is_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    // Certify on a virgin repo: warns about what is missing, writes
    // NOTHING — including never creating the database.
    let before = snapshot(tmp.path());
    let report = init::certify(tmp.path()).unwrap();
    assert!(report.certified(), "missing config is warn, not fail");
    assert_eq!(status_of(&report, "certify.gates"), CheckStatus::Warn);
    assert_eq!(status_of(&report, "certify.config"), CheckStatus::Warn);
    assert_eq!(snapshot(tmp.path()), before, "certify wrote nothing");
    assert!(
        !tmp.path().join(".aethyme/broker.db").exists(),
        "certify must never create the database"
    );

    // Scaffold: only the invariant broker artifacts — config skeleton,
    // gitignore block, database. Gate drafting is NOT scaffolding.
    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(
        report.checks.iter().all(|c| !c.id.contains("gates")),
        "scaffold never touches gates"
    );
    for id in [
        "scaffold.config-toml",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(&report, id), CheckStatus::Created, "{id}");
    }
    assert!(
        !tmp.path().join(".aethyme/gates.toml").exists(),
        "no gates.toml from scaffold"
    );
    let first = snapshot(tmp.path());
    assert!(!first.is_empty());

    // Determinism: a second scaffold changes nothing, byte for byte.
    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    for id in [
        "scaffold.config-toml",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(&report, id), CheckStatus::Pass, "{id}");
    }
    assert_eq!(snapshot(tmp.path()), first, "re-run is byte-identical");

    // Certify agrees with the scaffolded state and is still read-only.
    let report = init::certify(tmp.path()).unwrap();
    assert!(report.certified());
    assert_eq!(status_of(&report, "certify.config"), CheckStatus::Pass);
    assert_eq!(status_of(&report, "certify.gitignore"), CheckStatus::Pass);
    assert_eq!(snapshot(tmp.path()), first, "certify still wrote nothing");
}

#[test]
fn gates_draft_detects_manifests_deterministically() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"name":"x","scripts":{"test":"vitest run","lint":"eslint ."}}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("pyproject.toml"),
        "[tool.ruff]\nline-length = 100\n[tool.pytest.ini_options]\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    init_repo(tmp.path());

    // Gate drafting is its own adaptive command, not scaffold.
    let report = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&report, "gates.draft"), CheckStatus::Created);
    // Re-run: never overwritten.
    let report = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&report, "gates.draft"), CheckStatus::Pass);
    let gates = std::fs::read_to_string(tmp.path().join(".aethyme/gates.toml")).unwrap();
    for expected in [
        "name = \"cargo-test\"",
        "name = \"npm-lint\"",
        "name = \"npm-test\"",
        "name = \"ruff\"",
        "name = \"pytest\"",
    ] {
        assert!(gates.contains(expected), "missing {expected} in:\n{gates}");
    }
    // The draft parses as a valid gate config.
    let loaded = aethyme_broker::load_gates(tmp.path()).unwrap();
    assert_eq!(loaded.len(), 5);
    // No timestamps / absolute paths (determinism across machines & time).
    assert!(!gates.contains("202"), "no dates in generated files");
    assert!(!gates.contains(&tmp.path().to_string_lossy().into_owned()));
}

#[test]
fn guided_init_sets_up_a_fresh_repo_and_second_run_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    init_repo(tmp.path());

    // First run: all three phases execute; config + db are scaffolded and
    // (a manifest exists) a gates draft is written.
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(report.changed, "first run creates the missing artifacts");
    let scaffold = report.scaffold.as_ref().expect("certification passed");
    for id in [
        "scaffold.config-toml",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(scaffold, id), CheckStatus::Created, "{id}");
    }
    let gates = report.gates.as_ref().expect("no gates.toml before init");
    assert_eq!(status_of(gates, "gates.draft"), CheckStatus::Created);
    assert!(tmp.path().join(".aethyme/config.toml").exists());
    assert!(tmp.path().join(".aethyme/broker.db").exists());
    assert!(tmp.path().join(".aethyme/gates.toml").exists());

    // Second run: nothing changes (byte for byte), and the report says so.
    let first = snapshot(tmp.path());
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(!report.changed, "second run must be a no-op");
    let scaffold = report.scaffold.as_ref().unwrap();
    for id in [
        "scaffold.config-toml",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(scaffold, id), CheckStatus::Pass, "{id}");
    }
    assert!(
        report.gates.is_none(),
        "drafting is skipped once gates.toml exists"
    );
    assert_eq!(snapshot(tmp.path()), first, "re-run is byte-identical");
}

#[test]
fn guided_init_without_manifests_drafts_nothing_and_stays_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let report = init::guided_init(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(report.changed, "config + db are still scaffolded");
    let gates = report.gates.as_ref().expect("gates phase ran");
    assert_eq!(status_of(gates, "gates.draft"), CheckStatus::Warn);
    assert!(!tmp.path().join(".aethyme/gates.toml").exists());

    // No gates.toml means the draft phase runs again — and still writes
    // nothing, so the run as a whole reports no changes.
    let first = snapshot(tmp.path());
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(!report.changed);
    assert_eq!(
        status_of(report.gates.as_ref().unwrap(), "gates.draft"),
        CheckStatus::Warn
    );
    assert_eq!(snapshot(tmp.path()), first);
}

#[test]
fn guided_init_stops_before_writing_when_certification_fails() {
    // Not a git repository: certification fails, so init must not write.
    let tmp = tempfile::tempdir().unwrap();
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(!report.certified());
    assert!(report.scaffold.is_none(), "scaffold never ran");
    assert!(report.gates.is_none(), "gate drafting never ran");
    assert!(!report.changed);
    assert!(!tmp.path().join(".aethyme").exists(), "nothing was written");
}

#[test]
fn existing_files_are_never_touched_and_gitignore_appends_preserving_content() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"mine\"\ncommand = \"true\"\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "node_modules/\n").unwrap();

    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    let draft = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&draft, "gates.draft"), CheckStatus::Pass);
    let gates = std::fs::read_to_string(tmp.path().join(".aethyme/gates.toml")).unwrap();
    assert!(gates.contains("mine"), "user's gates.toml untouched");
    assert!(!gates.contains("Draft generated"));
    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.starts_with("node_modules/\n"),
        "content preserved"
    );
    assert!(gitignore.contains("aethyme-broker:begin"));
    assert!(gitignore.contains(".aethyme/worktrees/"));
}
