use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use aethyme_testkit::repo_root;

fn collect_files(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "cannot read skill directory {}: {error}",
            directory.display()
        )
    }) {
        let entry = entry.unwrap();
        let path = entry.path();
        let metadata = entry.file_type().unwrap();
        if metadata.is_dir() {
            collect_files(root, &path, files);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("skill file is inside its root")
                .to_path_buf();
            files.insert(relative, fs::read(&path).unwrap());
        }
    }
}

fn skill_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_files(root, root, &mut files);
    files
}

#[test]
fn claude_and_codex_skill_trees_stay_byte_identical() {
    let repository = repo_root();
    let claude_root = repository.join(".claude/skills");
    let codex_root = repository.join(".codex/skills");
    assert!(claude_root.is_dir(), "missing {}", claude_root.display());
    assert!(codex_root.is_dir(), "missing {}", codex_root.display());

    let claude = skill_files(&claude_root);
    let codex = skill_files(&codex_root);
    let claude_paths: BTreeSet<_> = claude.keys().collect();
    let codex_paths: BTreeSet<_> = codex.keys().collect();
    let missing_from_claude: Vec<_> = codex_paths.difference(&claude_paths).collect();
    let missing_from_codex: Vec<_> = claude_paths.difference(&codex_paths).collect();
    let differing: Vec<_> = claude
        .iter()
        .filter_map(|(path, contents)| (codex.get(path) != Some(contents)).then_some(path))
        .collect();

    assert!(
        missing_from_claude.is_empty() && missing_from_codex.is_empty() && differing.is_empty(),
        "Claude/Codex skill trees differ: missing from Claude {missing_from_claude:?}, \
         missing from Codex {missing_from_codex:?}, different contents {differing:?}"
    );
}

#[test]
fn explore_wrapper_matches_the_package_template_and_mentions_only_native_fallback() {
    let repository = repo_root();
    let paths = [
        repository.join("packages/aethyme/skills/aethyme/aethyme-explore"),
        repository.join(".claude/skills/aethyme/aethyme-explore"),
        repository.join(".codex/skills/aethyme/aethyme-explore"),
    ];
    let contents: Vec<_> = paths
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        })
        .collect();

    assert_eq!(
        contents[0], contents[1],
        "Claude copy drifted from package template"
    );
    assert_eq!(
        contents[0], contents[2],
        "Codex copy drifted from package template"
    );
    assert!(contents[0].contains("starts the Rust daemon and retries"));
    assert!(!contents[0].contains("Python daemon"));
}
