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

    // Scaffold: drafts config + gitignore. No manifests here, so no
    // gates.toml is drafted (a file defining nothing is dishonest).
    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    assert_eq!(status_of(&report, "scaffold.gates-toml"), CheckStatus::Warn);
    for id in ["scaffold.config-toml", "scaffold.gitignore"] {
        assert_eq!(status_of(&report, id), CheckStatus::Created, "{id}");
    }
    let first = snapshot(tmp.path());
    assert!(!first.is_empty());

    // Determinism: a second scaffold changes nothing, byte for byte.
    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    for id in ["scaffold.config-toml", "scaffold.gitignore"] {
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

    init::scaffold(tmp.path()).unwrap();
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
