//! Documented `aethyme` commands must exist.
//!
//! ## The failure this prevents
//!
//! The CLI reference documented eight commands that no longer exist —
//! `index`, `stats`, `search`, `ego`, `impact`, `eval explain-repo`,
//! `eval navigation-ctf` — plus a set of "Global Options" the router
//! never parsed (`--tenant-id`, `--verbose`). The repository's own
//! cross-process consumer registry records them as deleted with the
//! Gen-0 lineage, yet the reference still advertised them, and every
//! reader received `unknown subcommand` at runtime.
//!
//! Nothing caught it. `cli_reference_consistency.rs` validates broker
//! *flags* only; `deprecated_spelling_callers.rs` matches retired
//! `broker <verb>` spellings; `docs_hygiene.rs` checks links and code
//! fence syntax. No test resolved a documented command against the
//! router. A 2026-05-12 post-mortem
//! (`bug-skill-md-stale-explore-invocation.md`) proposed exactly this
//! check as "option 4"; it was never implemented.
//!
//! ## How it works
//!
//! The router itself is the oracle: every documented command is run
//! with `--help`, and anything that exits non-zero, or reports an
//! unknown subcommand, is a documentation defect. This avoids
//! maintaining a second list of valid spellings that could itself drift.
//!
//! Only *command paths* are checked — `aethyme graph callers` must be a
//! real command. Flags are out of scope here: flag surface is already
//! covered by the help snapshots, and free-form examples in prose are
//! too ambiguous to assert mechanically.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Documentation surfaces that state what commands exist *today*.
///
/// Deliberately an explicit list rather than every markdown file.
/// Design notes, historical plans and post-mortems legitimately mention
/// commands that no longer exist — `docs/code-review-…-analysis.md`
/// proposes `aethyme setup codex`, and `docs/project-plan.md` explains
/// that `aethyme eval` was removed. Asserting those against the router
/// would produce false positives and tempt someone into widening the
/// filters until the test passes vacuously.
///
/// The surfaces below are the ones a reader trusts for current usage:
/// the agent skill trees (which agents read verbatim), the CLI
/// reference, the getting-started guides, and the root orientation
/// files. These are exactly where the phantom commands lived.
const DOC_ROOTS: &[&str] = &[
    ".claude/skills",
    ".codex/skills",
    "packages/aethyme/skills",
    "packages/aethyme/docs/reference",
    "packages/aethyme/docs/getting-started",
    "README.md",
    "AGENTS.md",
    "CLAUDE.md",
];

/// Skip vendored, generated, or third-party trees.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    ".aethyme",
    ".claude-plugin",
    "node_modules",
    ".venv",
];

fn repo_root() -> PathBuf {
    // crates/aethyme-cli -> crates -> rust -> packages/aethyme -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .expect("workspace root")
        .to_path_buf()
}

fn collect_markdown(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_markdown(&path, out);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("md")
        ) {
            out.push(path);
        }
    }
}

/// Every documented file whose contents are scanned.
fn documented_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for entry in DOC_ROOTS {
        let path = root.join(entry);
        if path.is_dir() {
            collect_markdown(&path, &mut files);
        } else if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Reduce a documented line to the command words it invokes, or `None`
/// when the line is not an `aethyme` invocation.
///
/// Only the command path is returned — `aethyme graph node . src/a.py`
/// yields `["graph", "node"]`. Positional arguments are repository
/// paths, symbol names and placeholders, which are exactly the part of
/// an example that is legitimately illustrative rather than literal, so
/// they are not validated.
///
/// Handles fenced examples (`$ aethyme ...`), inline code
/// (`aethyme ...`), and list bullets.
fn command_words(line: &str) -> Option<Vec<String>> {
    // Strip an optional prompt marker, list bullet, or backtick.
    let mut rest = line.trim();
    for prefix in ["$ ", "> ", "- ", "* ", "`"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped.trim_start();
        }
    }
    let rest = rest.trim_start_matches('`');
    let rest = rest.strip_prefix("aethyme ")?;
    // A `<placeholder>` marks a metavariable, not a literal word:
    // `aethyme broker advanced <verb>` documents the shape of a command.
    // The router cannot answer for a placeholder, so the line is checked
    // only up to it.
    let rest = rest.split('<').next().unwrap_or(rest);
    if rest.trim().is_empty() {
        return None;
    }
    let words: Vec<String> = rest
        .split_whitespace()
        .map(|word| {
            // Strip code-span and quote punctuation first, so a
            // single-word command such as `` `aethyme explore` `` is not
            // rejected for carrying a trailing backtick.
            word.trim_matches(|c: char| c == '`' || c == '"' || c == '\'')
                .to_string()
        })
        // Stop at a flag, a path, or prose punctuation: only the literal
        // command path is validated.
        .take_while(|word| {
            !word.starts_with('-') && !word.contains('/') && !word.ends_with(['.', ',', ';', ':'])
        })
        .filter(|word| !word.is_empty() && *word != ".")
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(words)
    }
}

/// Ask the router whether a command path exists.
fn router_accepts(words: &[String]) -> bool {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aethyme"));
    for word in words {
        command.arg(word);
    }
    let output = command
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme");
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    !(stdout.contains("unknown subcommand") || stderr.contains("unknown subcommand"))
}

#[test]
fn documented_commands_exist_in_the_router() {
    let files = documented_files();
    assert!(
        files.len() > 10,
        "expected to scan the documentation tree, found {} files",
        files.len()
    );

    let mut checked = 0usize;
    let mut defects: Vec<String> = Vec::new();

    for file in &files {
        let Ok(contents) = std::fs::read_to_string(file) else {
            continue;
        };
        let relative = file
            .strip_prefix(repo_root())
            .unwrap_or(file)
            .display()
            .to_string();
        for (number, line) in contents.lines().enumerate() {
            // Skip prose that merely mentions the binary mid-sentence, such as
            // "`aethyme broker` exit codes name the outcome": the code
            // span closes on the first word, so there is no invocation to
            // run. Anything else starting the line, or following a
            // prompt or bullet marker, is treated as an invocation.
            let trimmed = line.trim_start();
            let code_span_closes_on_first_word = trimmed.starts_with('`')
                && trimmed[1..]
                    .split_once('`')
                    .is_some_and(|(_, after)| !after.trim().is_empty());
            let looks_like_invocation = trimmed.starts_with("aethyme ")
                || trimmed.starts_with("$ aethyme ")
                || trimmed.starts_with("> aethyme ")
                || trimmed.starts_with("- `aethyme ")
                || trimmed.starts_with("* `aethyme ")
                || trimmed.starts_with("`aethyme ");
            if !looks_like_invocation || code_span_closes_on_first_word {
                continue;
            }
            let Some(words) = command_words(line) else {
                continue;
            };
            // A documented word may contain shell interpolation or a
            // trailing period; keep it simple and skip anything that is
            // clearly not a literal command word.
            if words
                .iter()
                .any(|word| word.contains('$') || word.contains('{') || word.contains('|'))
            {
                continue;
            }
            checked += 1;
            if !router_accepts(&words) {
                defects.push(format!(
                    "{relative}:{}: `aethyme {}` is not a command",
                    number + 1,
                    words.join(" ")
                ));
            }
        }
    }

    assert!(
        checked > 50,
        "expected to validate a meaningful number of documented invocations, got {checked}"
    );
    assert!(
        defects.is_empty(),
        "{} documented command(s) do not exist in the router:\n  {}\n\n\
         Remove the stale example, or restore the command. Do not widen this test \
         to accept a phantom.",
        defects.len(),
        defects.join("\n  ")
    );
}

/// Guards the scanner itself: the patterns it must recognise, and the
/// prose it must ignore. Without this, an over-broad filter could make
/// the drift test vacuously pass.
#[test]
fn command_extraction_recognises_documented_forms() {
    let recognized: &[(&str, &[&str])] = &[
        ("aethyme graph node . src/a.py", &["graph", "node"]),
        ("$ aethyme graph callers . x", &["graph", "callers"]),
        ("> aethyme task pack --repo . --task x", &["task", "pack"]),
        ("`aethyme explore`", &["explore"]),
        ("- `aethyme broker status`", &["broker", "status"]),
        ("aethyme graph impact --repo .", &["graph", "impact"]),
    ];
    for (line, expected) in recognized {
        let expected_words: Vec<String> =
            expected.iter().map(|word| word.to_string()).collect();
        assert_eq!(
            command_words(line),
            Some(expected_words),
            "failed to recognise {line:?}"
        );
    }

    let ignored = [
        "the aethyne binary is not real",
        "see `aethyme --help` for the full list",
        "AETHYME_TENANT_ID is an environment variable",
        "no command here",
    ];
    for line in ignored {
        assert_eq!(
            command_words(line),
            None,
            "should not have extracted a command from {line:?}"
        );
    }
}