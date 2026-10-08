//! Keeps every caller on the six-verb broker surface.
//!
//! v0.8.4 moved the broker to six public verbs plus `advanced`, and
//! `aethyme-broker/src/cli/surface.rs` keeps the older spellings working
//! with a warning until `DEPRECATED_SPELLING_REMOVAL_RELEASE`. v0.8.7
//! migrated every caller that ships from this repository: generated agent
//! guidance and skills, broker-emitted next actions, adapter and hook
//! scripts, CI, and documentation. This suite fails when an old spelling is
//! reintroduced as a command, so removing the aliases cannot strand a
//! caller.
//!
//! What counts as a caller is a command in command position: the text after
//! the `aethyme` binary (or a shell variable holding it), or a
//! `"broker", "<verb>"` argument vector. Rust comments and `#[cfg(test)]`
//! modules are not callers. Tests under `tests/` exercise the aliases on
//! purpose and are not scanned.
//!
//! Permanent machine entry points (`quick-test`, `check-contract`,
//! `hooks pre-commit|post-commit|pre-push`) never change; see
//! `is_machine_entry_point` in `surface.rs`.

use std::path::{Path, PathBuf};

use aethyme_testkit::repo_root;

/// Internal verbs whose bare `aethyme broker <verb>` spelling is deprecated.
/// Mirrors the replacement table in `surface.rs` (`MERGED_FORMS`,
/// `ADVANCED_VERBS` and the special cases in `old_spelling_replacement`),
/// minus the machine entry points.
const OLD_BROKER_VERBS: &[&str] = &[
    // start
    "adopt",
    "start-agent",
    // status / submit / finish / unblock / gc sub-forms
    "readiness",
    "doctor",
    "prepare",
    "promote",
    "promotion-record",
    "close",
    "cleanup",
    "blockers",
    "reclaim",
    "storage",
    // moved to top level
    "init",
    "certify",
    "e2e",
    // `advanced` verbs (`hooks` is handled separately; `quick-test` and
    // `check-contract` are machine entry points)
    "leases",
    "git",
    "gh",
    "operations",
    "exec",
    "ship",
    "review",
    "gates",
    "trust",
    "agents",
    "handoff",
    "queue",
    "integration",
    "main",
    "representation",
    "checkpoint",
    "repair",
    "resources",
    "console",
    "advisories",
    "exposures",
    "note",
    "watch",
    "deliveries",
    "pr",
    "report",
    "quality-report",
    "external-events",
    "events",
    "metrics",
    "worktree-root",
    "worktrees",
    "scaffold",
    "verify-loop",
];

/// `hooks <word>` stays permanent for these words: installed hook shims call
/// them.
const MACHINE_HOOKS: &[&str] = &["pre-commit", "post-commit", "pre-push"];

/// Trees and files that ship callers: generated guidance and its templates,
/// skills, scripts, CI, configuration and documentation, and product source.
const SCAN_ROOTS: &[&str] = &[
    ".aethyme/config.toml",
    ".aethyme/overrides",
    ".cargo",
    ".claude/skills",
    ".codex/skills",
    ".github",
    "AGENTS.md",
    "CLAUDE.md",
    "CONTRIBUTING.md",
    "README.md",
    "SECURITY.md",
    "UPGRADING.md",
    "docs",
    "install.sh",
    "packaging",
    "scripts",
    "packages/aethyme/CONTRIBUTING.md",
    "packages/aethyme/README.md",
    "packages/aethyme/docs",
    "packages/aethyme/scripts",
    "packages/aethyme/skills",
    "packages/aethyme/rust/crates",
    "packages/aethyme-eval/benchmarks",
];

/// Whole files or trees that are history, or that define the aliases.
/// Each entry is a repository-relative path prefix.
const EXCLUDED: &[(&str, &str)] = &[
    (
        "CHANGELOG.md",
        "release history records the spellings of its time",
    ),
    ("docs/dogfood.md", "frozen historical playbook (issue #33)"),
    ("docs/dogfood-friction.md", "frozen historical friction log"),
    ("packages/aethyme/docs/reports/", "dated historical reports"),
    (
        "packages/aethyme/rust/crates/aethyme-broker/src/cli/surface.rs",
        "the alias table itself",
    ),
];

/// Individual lines allowed to keep an old spelling. `(path, text the line
/// contains, why)`. Keep each entry narrow; delete it when its reason ends.
const ALLOWED_LINES: &[(&str, &str, &str)] = &[
    (
        "packages/aethyme/rust/crates/aethyme-cli/src/help.rs",
        "const BROKER_HELP",
        "top-level words that route --help to canonical broker internals, not an invocation",
    ),
    (
        "packages/aethyme/rust/crates/aethyme-cli/src/main_unix.rs",
        "\"aethyme readiness\",",
        "retired spelling named only in its exit-2 migration hint",
    ),
    (
        "packages/aethyme/rust/crates/aethyme-cli/src/main_unix.rs",
        "\"aethyme enhance deploy\",",
        "retired spelling named only in its exit-2 migration hint",
    ),
    (
        "packages/aethyme/rust/crates/aethyme-cli/src/main_unix.rs",
        "\"aethyme enhance verify\",",
        "retired spelling named only in its exit-2 migration hint",
    ),
    // A persisted value, not only a message: v0.8.7 readiness repairs journal
    // this key. Recovery accepts it while new journals use the canonical command.
    (
        "packages/aethyme/rust/crates/aethyme-cli/src/repository_upgrade.rs",
        "\"aethyme broker readiness recover\"",
        "legacy v0.8.7 recovery key remains readable for interrupted transactions",
    ),
    // Pilots may run a pre-v0.8.4 binary, where the new spellings do not exist.
    (
        "packages/aethyme/docs/pilot/export-metrics.sh",
        "elif b=$(aethyme broker blockers --json",
        "fallback for pilots still on v0.8.3",
    ),
    (
        "packages/aethyme/docs/pilot/install.md",
        "On v0.8.3 the spelling is `aethyme broker trust`",
        "v0.8.3 pilot guidance",
    ),
    (
        "packages/aethyme/docs/pilot/install.md",
        "(v0.8.3: `aethyme broker adopt",
        "v0.8.3 pilot guidance",
    ),
    (
        "packages/aethyme/docs/pilot/six-verbs.md",
        "(v0.8.3: list with `aethyme broker blockers`",
        "v0.8.3 pilot guidance",
    ),
];

/// Markdown regions between these markers name old spellings on purpose,
/// for example the deprecation mapping table in the CLI reference.
const ALLOW_BEGIN: &str = "<!-- deprecated-spellings: begin";
const ALLOW_END: &str = "<!-- deprecated-spellings: end -->";

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The word starting at `text[at..]`, if any.
fn word_at(text: &str, at: usize) -> &str {
    let rest = &text[at..];
    let end = rest.find(|c: char| !is_word_char(c)).unwrap_or(rest.len());
    &rest[..end]
}

/// Whether the token immediately before byte `at` (which must be preceded by
/// one space) names the aethyme binary or a shell variable that holds it.
fn binary_before(text: &str, at: usize) -> bool {
    let Some(head) = text[..at].strip_suffix(' ') else {
        return false;
    };
    let start = head
        .rfind(|c: char| c.is_whitespace() || matches!(c, '`' | '(' | '[' | '\'' | '='))
        .map_or(0, |index| index + 1);
    let token = head[start..].trim_matches('"');
    token == "aethyme" || token.ends_with("/aethyme") || (token.starts_with('$') && token.len() > 1)
}

fn old_broker_verb(text: &str, at: usize) -> Option<String> {
    let verb = word_at(text, at);
    if verb == "hooks" {
        let after = at + verb.len();
        let next = text[after..]
            .strip_prefix(' ')
            .map(|rest| word_at(rest, 0))
            .unwrap_or("");
        return (!MACHINE_HOOKS.contains(&next)).then(|| format!("broker hooks {next}"));
    }
    OLD_BROKER_VERBS
        .contains(&verb)
        .then(|| format!("broker {verb}"))
}

/// Every deprecated spelling used as a command in `line`.
fn old_spellings(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    // `aethyme broker <verb>`, `"$BROKER" broker <verb>`, `.../aethyme broker <verb>`
    for (index, _) in line.match_indices("broker ") {
        if index > 0 && binary_before(line, index) {
            let at = index + "broker ".len();
            if let Some(spelling) = old_broker_verb(line, at) {
                found.push(spelling);
            }
        }
    }
    // `"broker", "<verb>"` argument vectors (Rust, Python)
    for quote in ['"', '\''] {
        let needle = format!("{quote}broker{quote},");
        for (index, _) in line.match_indices(&needle) {
            let rest = line[index + needle.len()..].trim_start();
            if let Some(rest) = rest.strip_prefix(quote)
                && let Some(end) = rest.find(quote)
            {
                let verb = &rest[..end];
                if verb != "hooks" && OLD_BROKER_VERBS.contains(&verb) {
                    found.push(format!("broker {verb}"));
                }
            }
        }
    }
    // Top-level spellings: `aethyme readiness`, `aethyme enhance deploy|verify`
    for word in ["readiness", "enhance"] {
        for (index, _) in line.match_indices(word) {
            if index == 0 || !binary_before(line, index) {
                continue;
            }
            if word_at(line, index) != word {
                continue;
            }
            if word == "readiness" {
                found.push("aethyme readiness".into());
            } else {
                let rest = &line[index + word.len()..];
                if let Some(sub) = rest.strip_prefix(' ').map(|rest| word_at(rest, 0))
                    && (sub == "deploy" || sub == "verify")
                {
                    found.push(format!("aethyme enhance {sub}"));
                }
            }
        }
    }
    found
}

/// The lines of `text` that are callers, with their 1-based line numbers.
/// Rust sources drop comments, `#[cfg(test)]` modules, the broker's internal
/// `USAGE` table (whose usage lines are rewritten to the public spelling at
/// help time), and join `\`-continued string lines.
fn caller_lines(relative: &str, text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let rust = relative.ends_with(".rs");
    let mut in_usage = false;
    let mut in_allowed_region = false;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let number = index + 1;
        index += 1;
        if line.contains(ALLOW_BEGIN) {
            in_allowed_region = true;
            continue;
        }
        if line.contains(ALLOW_END) {
            in_allowed_region = false;
            continue;
        }
        if in_allowed_region {
            continue;
        }
        if !rust {
            out.push((number, line.to_string()));
            continue;
        }
        if line.starts_with("#[cfg(test)]") {
            break;
        }
        if line.starts_with("const USAGE: &str") {
            in_usage = true;
        }
        if in_usage {
            if line == "\";" {
                in_usage = false;
            }
            continue;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut joined = line.to_string();
        while joined.ends_with('\\') && index < lines.len() {
            joined.pop();
            joined.push_str(lines[index].trim_start());
            index += 1;
        }
        out.push((number, joined));
    }
    out
}

fn is_test_path(relative: &str) -> bool {
    relative.contains("/tests/") || relative.ends_with("/tests.rs")
}

fn is_scanned_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    matches!(
        extension,
        "rs" | "md" | "sh" | "py" | "yml" | "yaml" | "toml" | "json" | "txt" | "template" | "jq"
    ) || name == "install.sh"
}

fn walk(root: &Path, relative: &str, found: &mut Vec<(String, PathBuf)>) {
    let path = root.join(relative);
    if path.is_file() {
        if is_scanned_file(&path) {
            found.push((relative.to_string(), path));
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(&path) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(
            name.as_str(),
            "target" | "node_modules" | ".venv" | "__pycache__" | ".git"
        ) {
            continue;
        }
        walk(root, &format!("{relative}/{name}"), found);
    }
}

#[test]
fn no_shipped_caller_uses_a_deprecated_broker_spelling() {
    let root = repo_root();
    let mut files = Vec::new();
    for scan_root in SCAN_ROOTS {
        walk(&root, scan_root, &mut files);
    }
    files.sort();
    assert!(
        files.len() > 100,
        "scan found only {} files under {}; the roots moved",
        files.len(),
        root.display()
    );

    let mut used_allowances = vec![false; ALLOWED_LINES.len()];
    let mut violations = Vec::new();
    for (relative, path) in &files {
        if is_test_path(relative)
            || EXCLUDED
                .iter()
                .any(|(prefix, _)| relative.starts_with(prefix))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (number, line) in caller_lines(relative, &text) {
            let spellings = old_spellings(&line);
            if spellings.is_empty() {
                continue;
            }
            if let Some(position) = ALLOWED_LINES
                .iter()
                .position(|(file, needle, _)| file == relative && line.contains(needle))
            {
                used_allowances[position] = true;
                continue;
            }
            violations.push(format!(
                "{relative}:{number}: {} in: {}",
                spellings.join(", "),
                line.trim()
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "deprecated broker spellings used as commands (see the mapping in \
         packages/aethyme/docs/reference/cli.md and surface.rs):\n{}",
        violations.join("\n")
    );
    let stale: Vec<String> = ALLOWED_LINES
        .iter()
        .zip(&used_allowances)
        .filter(|(_, used)| !**used)
        .map(|((file, needle, _), _)| format!("{file}: {needle}"))
        .collect();
    assert!(
        stale.is_empty(),
        "allowlist entries no longer match anything; delete them:\n{}",
        stale.join("\n")
    );
}

#[test]
fn the_matcher_finds_old_spellings_in_command_position() {
    for (line, expected) in [
        ("aethyme broker adopt --task x", "broker adopt"),
        (
            "run `aethyme broker leases claim a --session 1`",
            "broker leases",
        ),
        ("aethyme broker \\\n", ""),
        (
            "run: packages/aethyme/rust/target/release/aethyme broker gates run --all",
            "broker gates",
        ),
        ("\"$BROKER\" broker watch pr tick", "broker watch"),
        ("[broker, \"broker\", \"gh\", \"--session\"]", "broker gh"),
        (
            "&[\"broker\", \"readiness\", \"--json\"]",
            "broker readiness",
        ),
        ("aethyme broker hooks install", "broker hooks install"),
        ("aethyme readiness --json", "aethyme readiness"),
        ("\"$BIN\" enhance deploy --repo .", "aethyme enhance deploy"),
        ("aethyme enhance verify --repo .", "aethyme enhance verify"),
    ] {
        let found = old_spellings(line);
        if expected.is_empty() {
            assert!(found.is_empty(), "{line:?} -> {found:?}");
        } else {
            assert_eq!(found, [expected], "{line:?}");
        }
    }
}

#[test]
fn the_matcher_ignores_current_spellings_and_machine_entry_points() {
    for line in [
        "aethyme broker advanced leases claim a --session 1",
        "aethyme broker start --adopt --task x",
        "aethyme broker status readiness --json",
        "aethyme broker submit promote --session 1",
        "aethyme broker finish cleanup 7",
        "aethyme broker gc storage plan",
        "aethyme broker unblock --json",
        "aethyme broker quick-test",
        "aethyme broker check-contract",
        "\"$AETHYME\" broker hooks pre-commit || true",
        "aethyme broker hooks post-commit",
        "aethyme broker hooks pre-push \"$@\"",
        "aethyme deploy verify --repo .",
        "the broker gates run in a detached worktree",
        "operational readiness report",
        "`broker submit` promotes to the local integration branch",
    ] {
        assert!(old_spellings(line).is_empty(), "{line:?}");
    }
}

#[test]
fn rust_comments_tests_and_the_usage_table_are_not_callers() {
    let source = "\
// aethyme broker adopt in a comment
const USAGE: &str = \"\\
  aethyme broker adopt [<path>]
\";
let s = \"close it: aethyme broker \\
         close --session {}\";
#[cfg(test)]
mod tests { const X: &str = \"aethyme broker adopt\"; }
";
    let lines = caller_lines("x.rs", source);
    let flagged: Vec<_> = lines
        .iter()
        .flat_map(|(number, line)| old_spellings(line).into_iter().map(move |s| (*number, s)))
        .collect();
    assert_eq!(flagged, [(5, "broker close".to_string())]);
}
