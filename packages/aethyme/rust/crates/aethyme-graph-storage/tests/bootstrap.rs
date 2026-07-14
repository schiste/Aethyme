//! Integration tests for repo bootstrap.

use aethyme_graph_storage::{
    BootstrapError, EngineVersionReadError, GITATTRIBUTES_CONTENT, bootstrap_repo,
    read_engine_version,
};

#[test]
fn bootstrap_creates_expected_directory_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = bootstrap_repo(tmp.path(), "0.1.0").unwrap();

    assert!(paths.graph_dir.is_dir());
    assert!(paths.index_dir.is_dir());
    assert!(paths.gitattributes.is_file());
    assert!(paths.engine_version_file.is_file());

    // The paths should match the documented layout.
    assert_eq!(paths.graph_dir, tmp.path().join(".aethyme/graph"));
    assert_eq!(paths.index_dir, tmp.path().join(".aethyme/graph/_index"));
    assert_eq!(
        paths.gitattributes,
        tmp.path().join(".aethyme/graph/.gitattributes")
    );
    assert_eq!(
        paths.engine_version_file,
        tmp.path().join(".aethyme/engine-version")
    );
}

#[test]
fn bootstrap_writes_canonical_gitattributes() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = bootstrap_repo(tmp.path(), "0.1.0").unwrap();
    let content = std::fs::read_to_string(&paths.gitattributes).unwrap();
    assert_eq!(content, GITATTRIBUTES_CONTENT);
    // Sanity-check the actual lines present.
    assert!(content.contains("**/*.bin linguist-generated=true binary"));
    assert!(content.contains("_index/**/*.ndjson linguist-generated=true merge=union"));
}

#[test]
fn bootstrap_pins_engine_version() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap_repo(tmp.path(), "0.1.0").unwrap();
    let v = read_engine_version(tmp.path()).unwrap();
    assert_eq!(v, "0.1.0");
}

#[test]
fn bootstrap_rejects_empty_engine_version() {
    let tmp = tempfile::tempdir().unwrap();
    let err = bootstrap_repo(tmp.path(), "").unwrap_err();
    assert!(matches!(err, BootstrapError::EmptyEngineVersion));
}

#[test]
fn bootstrap_is_idempotent_with_same_version() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap_repo(tmp.path(), "0.1.0").unwrap();
    bootstrap_repo(tmp.path(), "0.1.0").unwrap();
    bootstrap_repo(tmp.path(), "0.1.0").unwrap();
    assert_eq!(read_engine_version(tmp.path()).unwrap(), "0.1.0");
}

#[test]
fn bootstrap_overwrites_version_on_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap_repo(tmp.path(), "0.1.0").unwrap();
    assert_eq!(read_engine_version(tmp.path()).unwrap(), "0.1.0");
    bootstrap_repo(tmp.path(), "0.2.0").unwrap();
    assert_eq!(read_engine_version(tmp.path()).unwrap(), "0.2.0");
}

#[test]
fn read_engine_version_reports_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let err = read_engine_version(tmp.path()).unwrap_err();
    assert!(matches!(err, EngineVersionReadError::Io(_)));
}

#[test]
fn read_engine_version_rejects_empty_after_trim() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join(".aethyme/engine-version"), "\n\n").unwrap();
    let err = read_engine_version(tmp.path()).unwrap_err();
    assert!(matches!(err, EngineVersionReadError::Empty));
}

#[test]
fn read_engine_version_trims_trailing_newline() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join(".aethyme/engine-version"), "1.2.3\n").unwrap();
    assert_eq!(read_engine_version(tmp.path()).unwrap(), "1.2.3");
}

#[test]
fn bootstrap_works_in_existing_repo_with_other_files() {
    // Confirm bootstrap doesn't fail or wipe pre-existing repo
    // contents. Create some "source" files first, then bootstrap.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/cli.py"), b"# source").unwrap();
    std::fs::write(tmp.path().join("README.md"), b"# readme").unwrap();

    bootstrap_repo(tmp.path(), "0.1.0").unwrap();

    // Existing files untouched.
    assert_eq!(
        std::fs::read(tmp.path().join("src/cli.py")).unwrap(),
        b"# source"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("README.md")).unwrap(),
        b"# readme"
    );
    // Aethyme layout in place.
    assert!(tmp.path().join(".aethyme/graph/_index").is_dir());
    assert!(tmp.path().join(".aethyme/engine-version").is_file());
}
