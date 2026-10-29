//! Every read of a human-authored `.aethyme` configuration file goes through
//! the shared loader (#1223). A direct read of `.aethyme/<stem>.toml` would
//! silently ignore a `<stem>.yaml` twin, or worse, read a stale twin while the
//! rest of the broker reads the other one.
//!
//! This scans non-test production sources in the workspace for a
//! configuration path literal (or one of the path constants) on the same line
//! as a read or an existence probe. Paths in messages, write sites and test
//! fixtures are not reads and are not flagged.

use std::path::{Path, PathBuf};

/// Stems covered by the rule. `environments` (#1209) adopts the loader when
/// its module lands; until then it is TOML-first by design.
const STEMS: &[&str] = &["config", "gates", "prepare", "broker"];

/// Constants that name one of the files above.
const PATH_CONSTANTS: &[&str] = &[
    "GATES_CONFIG_RELPATH",
    "PREPARATION_CONFIG_RELPATH",
    "BROKER_CONFIG_RELPATH",
    "GRAPH_CONFIG_RELPATH",
    "PREPARATION_CONFIG_PATH",
];

const READS: &[&str] = &[
    "read_to_string(",
    "fs::read(",
    "file_at_commit(",
    "raw_file_at_commit(",
    "raw_text(",
    ".exists()",
    ".is_file()",
    "path_exists_at(",
    "committed_regular_file(",
];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "tests" || name == "target" || name == "fixtures" || name == "benches" {
                continue;
            }
            rust_sources(&path, out);
        } else if name.ends_with(".rs")
            && name != "tests.rs"
            && !name.ends_with("_tests.rs")
            && !name.starts_with("tests_")
        {
            out.push(path);
        }
    }
}

/// The production part of a source file: everything before its first
/// `#[cfg(test)]` module.
fn production_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim() == "#[cfg(test)]"
            && lines
                .get(index + 1)
                .is_some_and(|next| next.trim_start().starts_with("mod "))
        {
            break;
        }
        out.push((index + 1, *line));
    }
    out
}

fn names_config_file(line: &str) -> bool {
    STEMS
        .iter()
        .any(|stem| line.contains(&format!(".aethyme/{stem}.toml")))
        || PATH_CONSTANTS
            .iter()
            .any(|constant| line.contains(constant))
}

#[test]
fn no_production_source_reads_aethyme_config_directly() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .to_path_buf();
    let mut sources = Vec::new();
    rust_sources(&crates, &mut sources);
    assert!(
        sources.len() > 50,
        "scanned too few sources: {}",
        sources.len()
    );
    let mut offenders = Vec::new();
    for source in sources {
        if source.starts_with(crates.join("aethyme-config")) {
            continue;
        }
        let text = std::fs::read_to_string(&source).unwrap();
        for (number, line) in production_lines(&text) {
            let code = line.split("//").next().unwrap_or("");
            if names_config_file(code) && READS.iter().any(|read| code.contains(read)) {
                offenders.push(format!(
                    "{}:{number}: {}",
                    source.strip_prefix(&crates).unwrap_or(&source).display(),
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "read .aethyme configuration through aethyme_config (load_config, \
         read_config_toml, GitRepo::config_toml_at_commit, config_exists) so a \
         YAML twin is honoured:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_rule_catches_a_direct_read() {
    let text = "fn f(root: &Path) {\n    std::fs::read_to_string(root.join(\".aethyme/gates.toml\"));\n}\n";
    let flagged: Vec<_> = production_lines(text)
        .into_iter()
        .filter(|(_, line)| names_config_file(line) && READS.iter().any(|read| line.contains(read)))
        .collect();
    assert_eq!(flagged.len(), 1);
}
