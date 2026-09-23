//! Engine source must not name eval playground repositories.
//!
//! Walks every `packages/aethyme/rust/crates/*/src/**/*.rs` file, including
//! inline `#[cfg(test)]` modules, and fails with `file:line` for each
//! case-insensitive occurrence of an identifier from [`BANNED_IDENTIFIERS`].
//! Files under a crate's `tests/` directory are not walked, so integration
//! test fixtures (and this file) are exempt.

use std::path::{Path, PathBuf};

use aethyme_testkit::rust_workspace_root;

/// Identifiers that belong to eval playground repositories, lowercase.
///
/// Cardinal Rule 2 (`CLAUDE.md`, `packages/aethyme/docs/guides/eval-protocol.md`)
/// forbids tuning the engine to eval scenarios: evals are diagnostics, not
/// targets. A ranking rule, path pattern, or scoring bonus that names a file,
/// directory, provider, or symbol from a playground app is exactly that kind
/// of tuning, so these identifiers must never appear in engine source. That
/// includes inline unit-test fixtures under `src/`, because fixtures written
/// around a playground's layout keep the tuned behavior pinned. Integration
/// tests under a crate's `tests/` directory are exempt; eval ground truth in
/// `packages/aethyme-eval` is outside this walk entirely.
const BANNED_IDENTIFIERS: &[&str] = &[
    "gcp-run-proxy",
    "auth0",
    "audit_jws",
    "profile_integrity",
    "domain_verification",
    "sts.googleapis",
    "iamcredentials",
    "publishablekey",
    "backend/api_keys",
    "backend.api_keys",
];

fn rust_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files_under(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

fn crate_source_files() -> Vec<PathBuf> {
    let crates = rust_workspace_root().join("crates");
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&crates)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", crates.display()));
    for entry in entries {
        let src = entry.expect("crate entry").path().join("src");
        if src.is_dir() {
            rust_files_under(&src, &mut files);
        }
    }
    files.sort();
    files
}

#[test]
fn engine_source_names_no_eval_playground_identifiers() {
    let files = crate_source_files();
    assert!(
        files.len() > 50,
        "expected to walk the workspace crate sources, found {} files",
        files.len()
    );

    let workspace = rust_workspace_root();
    let mut violations = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        for (index, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            for banned in BANNED_IDENTIFIERS {
                if lower.contains(banned) {
                    violations.push(format!(
                        "{}:{}: `{banned}`",
                        file.strip_prefix(&workspace).unwrap_or(file).display(),
                        index + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "eval playground identifiers found in engine source (Cardinal Rule 2):\n{}",
        violations.join("\n")
    );
}
