//! Integration tests for the container node kinds.

use aethyme_graph_schema::{
    Directory, DirectoryConstructionError, File, FileConstructionError,
    Module, ModuleConstructionError, NodeKind, NonCodeFile,
    NonCodeFileConstructionError, NonCodeFormat, Package,
    PackageConstructionError, Repository, RepositoryConstructionError,
};

// ─── Repository ──────────────────────────────────────────────────────

#[test]
fn repository_construction_validates_required_fields() {
    let r = Repository::new("aethyme", "/path/to/repo", "git").unwrap();
    assert_eq!(r.name(), "aethyme");
    assert_eq!(r.root_path(), "/path/to/repo");
    assert_eq!(r.vcs(), "git");
    assert_eq!(r.id().kind(), NodeKind::Repository);

    assert!(matches!(
        Repository::new("", "/path", "git"),
        Err(RepositoryConstructionError::EmptyName)
    ));
    assert!(matches!(
        Repository::new("aethyme", "", "git"),
        Err(RepositoryConstructionError::EmptyRootPath)
    ));
    assert!(matches!(
        Repository::new("aethyme", "/path", ""),
        Err(RepositoryConstructionError::EmptyVcs)
    ));
}

#[test]
fn repository_serde_round_trip() {
    let r = Repository::new("aethyme", "/path", "git").unwrap();
    let json = serde_json::to_string(&r).unwrap();
    let back: Repository = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

// ─── Directory ───────────────────────────────────────────────────────

#[test]
fn directory_construction_works() {
    let d = Directory::new("aethyme", "src/cli").unwrap();
    assert_eq!(d.path(), "src/cli");
    assert_eq!(d.id().kind(), NodeKind::Directory);

    assert!(matches!(
        Directory::new("aethyme", ""),
        Err(DirectoryConstructionError::EmptyPath)
    ));
}

#[test]
fn directory_serde_round_trip() {
    let d = Directory::new("aethyme", "src").unwrap();
    let json = serde_json::to_string(&d).unwrap();
    let back: Directory = serde_json::from_str(&json).unwrap();
    assert_eq!(back, d);
}

// ─── Module ──────────────────────────────────────────────────────────

#[test]
fn module_construction_validates_required_fields() {
    let m = Module::new("aethyme", "src/cli", "cli", "python").unwrap();
    assert_eq!(m.path(), "src/cli");
    assert_eq!(m.name(), "cli");
    assert_eq!(m.language(), "python");
    assert_eq!(m.id().kind(), NodeKind::Module);

    assert!(matches!(
        Module::new("aethyme", "", "cli", "python"),
        Err(ModuleConstructionError::EmptyPath)
    ));
    assert!(matches!(
        Module::new("aethyme", "src/cli", "", "python"),
        Err(ModuleConstructionError::EmptyName)
    ));
    assert!(matches!(
        Module::new("aethyme", "src/cli", "cli", ""),
        Err(ModuleConstructionError::EmptyLanguage)
    ));
}

#[test]
fn module_serde_round_trip() {
    let m = Module::new("aethyme", "src/cli", "cli", "python").unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back: Module = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

// ─── File ────────────────────────────────────────────────────────────

#[test]
fn file_construction_validates_required_fields() {
    let f = File::new("aethyme", "src/cli.py", "python", 12345, "abc123")
        .unwrap();
    assert_eq!(f.path(), "src/cli.py");
    assert_eq!(f.language(), "python");
    assert_eq!(f.byte_size(), 12345);
    assert_eq!(f.content_hash(), "abc123");
    assert_eq!(f.id().kind(), NodeKind::File);

    assert!(matches!(
        File::new("aethyme", "", "python", 100, "h"),
        Err(FileConstructionError::EmptyPath)
    ));
    assert!(matches!(
        File::new("aethyme", "x.py", "", 100, "h"),
        Err(FileConstructionError::EmptyLanguage)
    ));
    assert!(matches!(
        File::new("aethyme", "x.py", "python", 100, ""),
        Err(FileConstructionError::EmptyContentHash)
    ));
}

#[test]
fn file_serde_round_trip() {
    let f =
        File::new("aethyme", "src/cli.py", "python", 12345, "abc123").unwrap();
    let json = serde_json::to_string(&f).unwrap();
    let back: File = serde_json::from_str(&json).unwrap();
    assert_eq!(back, f);
}

// ─── NonCodeFile ─────────────────────────────────────────────────────

#[test]
fn non_code_file_construction_works() {
    let n = NonCodeFile::new("aethyme", "README.md", NonCodeFormat::Markdown)
        .unwrap();
    assert_eq!(n.path(), "README.md");
    assert_eq!(n.format(), &NonCodeFormat::Markdown);
    assert_eq!(n.id().kind(), NodeKind::NonCodeFile);

    assert!(matches!(
        NonCodeFile::new("aethyme", "", NonCodeFormat::Plain),
        Err(NonCodeFileConstructionError::EmptyPath)
    ));
}

#[test]
fn non_code_format_other_round_trips() {
    let f = NonCodeFormat::Other("xml".into());
    let json = serde_json::to_string(&f).unwrap();
    let back: NonCodeFormat = serde_json::from_str(&json).unwrap();
    assert_eq!(back, f);
}

#[test]
fn non_code_file_serde_round_trip() {
    for format in [
        NonCodeFormat::Markdown,
        NonCodeFormat::Yaml,
        NonCodeFormat::Json,
        NonCodeFormat::Toml,
        NonCodeFormat::Plain,
        NonCodeFormat::Other("rst".into()),
    ] {
        let n = NonCodeFile::new("aethyme", "doc.md", format).unwrap();
        let json = serde_json::to_string(&n).unwrap();
        let back: NonCodeFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, n);
    }
}

// ─── Package ─────────────────────────────────────────────────────────

#[test]
fn package_construction_validates_required_fields() {
    let p = Package::new(
        "aethyme",
        "rust",
        "aethyme-engine",
        "rust/Cargo.toml",
        "cargo",
    )
    .unwrap();
    assert_eq!(p.path(), "rust");
    assert_eq!(p.name(), "aethyme-engine");
    assert_eq!(p.manifest_path(), "rust/Cargo.toml");
    assert_eq!(p.manifest_kind(), "cargo");
    assert_eq!(p.id().kind(), NodeKind::Package);

    assert!(matches!(
        Package::new("aethyme", "", "n", "m", "k"),
        Err(PackageConstructionError::EmptyPath)
    ));
    assert!(matches!(
        Package::new("aethyme", "p", "", "m", "k"),
        Err(PackageConstructionError::EmptyName)
    ));
    assert!(matches!(
        Package::new("aethyme", "p", "n", "", "k"),
        Err(PackageConstructionError::EmptyManifestPath)
    ));
    assert!(matches!(
        Package::new("aethyme", "p", "n", "m", ""),
        Err(PackageConstructionError::EmptyManifestKind)
    ));
}

#[test]
fn package_serde_round_trip() {
    let p = Package::new(
        "aethyme",
        "rust",
        "aethyme-engine",
        "rust/Cargo.toml",
        "cargo",
    )
    .unwrap();
    let json = serde_json::to_string(&p).unwrap();
    let back: Package = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

// ─── Cross-kind: same inputs give same NodeId ────────────────────────

#[test]
fn same_logical_inputs_give_same_node_id_across_constructions() {
    // Smoke check that the per-kind constructors thread through to
    // the same NodeId::new path deterministically.
    let f1 =
        File::new("aethyme", "src/cli.py", "python", 100, "abc").unwrap();
    let f2 =
        File::new("aethyme", "src/cli.py", "python", 999, "xyz").unwrap();
    // Same kind, repo, path → same ID regardless of size/content_hash
    // (those aren't in the ID's hash inputs).
    assert_eq!(f1.id(), f2.id());
}
