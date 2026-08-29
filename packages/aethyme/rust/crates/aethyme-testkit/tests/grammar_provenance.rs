//! Tracked tree-sitter grammar asset provenance.
//!
//! Successor to `scripts/verify-grammar-provenance.py` (python-retirement
//! Phase 7). The grammars belong to the Rust crates, so their licensing
//! and checksum audit is `packages/aethyme`'s own concern and ports here
//! rather than moving to `packages/aethyme-eval`.
//!
//! The script had two modes and NO test invoked either — both were
//! manual commands in `docs/third-party-license-audit.md` and
//! `rust/grammars/README.md`:
//!
//! * default — manifest shape plus `grammar.wasm` checksums. As a test it
//!   now runs on every `cargo test`, so a silently edited checksum or a
//!   non-MIT grammar can no longer reach a release unnoticed. That is a
//!   strict gain over a command someone had to remember to type.
//! * `--require-pinned` — additionally demands exact upstream source refs
//!   and tree-sitter CLI versions. Every grammar is still `UNPINNED`, so
//!   this is `#[ignore]`d and stays the release-time command it always
//!   was: `cargo test -p aethyme-testkit --test grammar_provenance --
//!   --ignored`. It is documented as expected to fail until the refs are
//!   pinned; marking it `#[ignore]` records that without pretending the
//!   check passes.

use std::path::PathBuf;

use aethyme_testkit::rust_workspace_root;
use sha2::{Digest, Sha256};

const UNPINNED_VALUES: [&str; 4] = ["", "UNPINNED", "UNKNOWN", "TODO"];

const REQUIRED_FIELDS: [&str; 8] = [
    "language",
    "wasm",
    "queries",
    "upstream",
    "license",
    "source_ref",
    "tree_sitter_cli_version",
    "sha256",
];

fn manifest_path() -> PathBuf {
    rust_workspace_root().join("grammars/manifest.toml")
}

fn sha256_of(path: &PathBuf) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    );
    format!("{:x}", hasher.finalize())
}

fn is_pinned(value: Option<&str>) -> bool {
    value.is_some_and(|value| !UNPINNED_VALUES.contains(&value.trim().to_uppercase().as_str()))
}

/// Returns the human-readable errors the Python verifier would print.
fn verify_manifest(require_pinned: bool) -> Vec<String> {
    let path = manifest_path();
    let mut errors = Vec::new();
    let data: toml::Value = toml::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .expect("grammar manifest is valid TOML");
    let grammar_root = path.parent().expect("manifest has a parent").to_path_buf();

    if data
        .get("manifest_version")
        .and_then(toml::Value::as_integer)
        != Some(1)
    {
        errors.push("manifest_version must be 1".to_string());
    }
    let Some(grammars) = data
        .get("grammar")
        .and_then(toml::Value::as_array)
        .filter(|entries| !entries.is_empty())
    else {
        errors.push("manifest must define at least one [[grammar]] entry".to_string());
        return errors;
    };

    let mut seen_languages: Vec<String> = Vec::new();
    for grammar in grammars {
        let Some(table) = grammar.as_table() else {
            errors.push("grammar entries must be TOML tables".to_string());
            continue;
        };

        let language = table
            .get("language")
            .and_then(toml::Value::as_str)
            .unwrap_or("<missing>")
            .to_string();

        let mut missing: Vec<&str> = REQUIRED_FIELDS
            .into_iter()
            .filter(|field| !table.contains_key(*field))
            .collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            errors.push(format!(
                "{language}: missing fields: {}",
                missing.join(", ")
            ));
            continue;
        }

        if seen_languages.contains(&language) {
            errors.push(format!("{language}: duplicate language entry"));
        }
        seen_languages.push(language.clone());

        let wasm_path = grammar_root.join(table["wasm"].as_str().unwrap_or_default());
        let query_path = grammar_root.join(table["queries"].as_str().unwrap_or_default());
        if !wasm_path.is_file() {
            errors.push(format!(
                "{language}: missing wasm file {}",
                wasm_path.display()
            ));
        } else {
            match table["sha256"].as_str() {
                Some(expected) if expected.len() == 64 => {
                    if sha256_of(&wasm_path) != expected {
                        errors.push(format!(
                            "{language}: sha256 mismatch for {}",
                            wasm_path.display()
                        ));
                    }
                }
                _ => errors.push(format!("{language}: invalid sha256 value")),
            }
        }

        if !query_path.is_file() {
            errors.push(format!(
                "{language}: missing query file {}",
                query_path.display()
            ));
        }

        if table["license"].as_str() != Some("MIT") {
            errors.push(format!(
                "{language}: expected MIT license, got {:?}",
                table["license"]
            ));
        }

        if require_pinned {
            if !is_pinned(table["source_ref"].as_str()) {
                errors.push(format!("{language}: source_ref is not pinned"));
            }
            if !is_pinned(table["tree_sitter_cli_version"].as_str()) {
                errors.push(format!("{language}: tree_sitter_cli_version is not pinned"));
            }
        }
    }

    errors
}

/// Development mode: manifest shape, file presence, licenses, checksums.
#[test]
fn grammar_provenance_verifies_in_development_mode() {
    let errors = verify_manifest(false);
    assert!(
        errors.is_empty(),
        "grammar provenance failed (development mode):\n{}",
        errors.join("\n")
    );
}

/// Release mode. Expected to fail until every grammar records a pinned
/// upstream ref and tree-sitter CLI version — run deliberately with
/// `cargo test -p aethyme-testkit --test grammar_provenance -- --ignored`.
#[test]
#[ignore = "release gate: every grammar is still UNPINNED (see rust/grammars/README.md)"]
fn grammar_provenance_verifies_in_release_mode() {
    let errors = verify_manifest(true);
    assert!(
        errors.is_empty(),
        "grammar provenance failed (release mode):\n{}",
        errors.join("\n")
    );
}
