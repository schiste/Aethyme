//! Integration tests for filesystem layout helpers.

use std::path::Path;

use aethyme_graph_storage::{
    InvalidPath, cache_dir, engine_version_path, fragment_path, index_shard_path,
    validate_module_name, validate_source_path,
};

#[test]
fn fragment_path_mirrors_source_tree() {
    let repo = Path::new("/repo");
    let p = fragment_path(repo, "src/cli.py");
    assert_eq!(p.to_str().unwrap(), "/repo/.aethyme/graph/src/cli.py.bin");
}

#[test]
fn fragment_path_handles_nested_paths() {
    let repo = Path::new("/repo");
    let p = fragment_path(repo, "packages/auth/src/jwt.rs");
    assert_eq!(
        p.to_str().unwrap(),
        "/repo/.aethyme/graph/packages/auth/src/jwt.rs.bin"
    );
}

#[test]
fn index_shard_path_uses_dot_module_name_literally() {
    let repo = Path::new("/repo");
    let p = index_shard_path(repo, "src.cli");
    assert_eq!(
        p.to_str().unwrap(),
        "/repo/.aethyme/graph/_index/src.cli.ndjson"
    );
}

#[test]
fn engine_version_path_is_top_level() {
    let repo = Path::new("/repo");
    let p = engine_version_path(repo);
    assert_eq!(p.to_str().unwrap(), "/repo/.aethyme/engine-version");
}

#[test]
fn cache_dir_is_top_level_and_separate_from_graph() {
    let repo = Path::new("/repo");
    let p = cache_dir(repo);
    assert_eq!(p.to_str().unwrap(), "/repo/.aethyme/cache");
}

// ─── Validation ──────────────────────────────────────────────────────

#[test]
fn validate_source_path_accepts_normal_paths() {
    validate_source_path("src/cli.py").unwrap();
    validate_source_path("README.md").unwrap();
    validate_source_path("packages/auth/src/jwt.rs").unwrap();
}

#[test]
fn validate_source_path_rejects_empty() {
    assert_eq!(validate_source_path("").unwrap_err(), InvalidPath::Empty);
}

#[test]
fn validate_source_path_rejects_absolute() {
    assert!(matches!(
        validate_source_path("/etc/passwd"),
        Err(InvalidPath::Absolute { .. })
    ));
    assert!(matches!(
        validate_source_path(r"C:\Windows\System32"),
        Err(InvalidPath::Absolute { .. })
    ));
    assert!(matches!(
        validate_source_path(r"\\server\share"),
        Err(InvalidPath::Absolute { .. })
    ));
}

#[test]
fn validate_source_path_rejects_parent_traversal() {
    assert!(matches!(
        validate_source_path("../etc/passwd"),
        Err(InvalidPath::Traversal { .. })
    ));
    assert!(matches!(
        validate_source_path("src/../../etc/passwd"),
        Err(InvalidPath::Traversal { .. })
    ));
    assert!(matches!(
        validate_source_path(r"src\..\etc"),
        Err(InvalidPath::Traversal { .. })
    ));
}

#[test]
fn validate_module_name_accepts_normal_names() {
    validate_module_name("src.cli").unwrap();
    validate_module_name("aethyme_engine").unwrap();
    validate_module_name("packages.auth.src.jwt").unwrap();
}

#[test]
fn validate_module_name_rejects_path_chars() {
    assert!(matches!(
        validate_module_name("src/cli"),
        Err(InvalidPath::InvalidChar { .. })
    ));
    assert!(matches!(
        validate_module_name("src\\cli"),
        Err(InvalidPath::InvalidChar { .. })
    ));
    assert!(matches!(
        validate_module_name("module\nname"),
        Err(InvalidPath::InvalidChar { .. })
    ));
}

#[test]
fn validate_module_name_rejects_dot_traversal() {
    assert!(matches!(
        validate_module_name(".."),
        Err(InvalidPath::Traversal { .. })
    ));
    assert!(matches!(
        validate_module_name("."),
        Err(InvalidPath::Traversal { .. })
    ));
}
