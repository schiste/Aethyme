//! Documentation link and code-example validation.
//!
//! Ported from `tests/docs/test_links.py` (7 cases) and
//! `tests/docs/test_examples.py` (6 cases) in python-retirement Phase 7.
//! They belong to no product crate — they inspect `packages/aethyme/docs`
//! itself — so they live with the workspace's dev-support crate.
//!
//! **Advisory cases.** Four of the six example checks (bash, SQL, curl,
//! missing-language) never failed in the Python original: they collected
//! findings and printed a warning. That was deliberate — shellcheck may
//! not be installed, and "```text" blocks are a style nit, not a
//! contract. They are ported with that behaviour intact rather than
//! quietly promoted to failures, which would have changed what CI
//! rejects under cover of a migration. Run with `--nocapture` to read
//! the warnings.

use std::path::{Path, PathBuf};

use aethyme_testkit::package_root;

fn docs_dir() -> PathBuf {
    package_root().join("docs")
}

/// Every `*.md` under `dir`, recursively.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn is_generated_report(path: &Path) -> bool {
    path.strip_prefix(docs_dir())
        .expect("docs-relative path")
        .components()
        .any(|component| component.as_os_str() == "reports")
}

fn relative(path: &Path) -> String {
    path.strip_prefix(docs_dir())
        .expect("docs-relative path")
        .display()
        .to_string()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Markdown links as `(target, line_number)`, matching the Python
/// `\[([^\]]+)\]\(([^\)]+)\)` scan line by line.
fn extract_links(text: &str) -> Vec<(String, usize)> {
    let mut links = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let bytes: Vec<char> = line.chars().collect();
        let mut position = 0;
        while position < bytes.len() {
            if bytes[position] != '[' {
                position += 1;
                continue;
            }
            let Some(label_end) = (position + 1..bytes.len()).find(|&i| bytes[i] == ']') else {
                break;
            };
            // The label must be non-empty and `](` must be adjacent.
            if label_end == position + 1
                || label_end + 1 >= bytes.len()
                || bytes[label_end + 1] != '('
            {
                position += 1;
                continue;
            }
            let Some(target_end) = (label_end + 2..bytes.len()).find(|&i| bytes[i] == ')') else {
                position += 1;
                continue;
            };
            let target: String = bytes[label_end + 2..target_end].iter().collect();
            links.push((target, index + 1));
            position = target_end + 1;
        }
    }
    links
}

fn is_external(link: &str) -> bool {
    link.starts_with("http://") || link.starts_with("https://")
}

fn is_anchor(link: &str) -> bool {
    link.starts_with('#')
}

// ── tests/docs/test_links.py ────────────────────────────────────────────

#[test]
fn docs_directory_exists() {
    let dir = docs_dir();
    assert!(dir.exists(), "Docs directory not found: {}", dir.display());
    assert!(dir.is_dir(), "Docs path is not a directory: {}", dir.display());
}

#[test]
fn markdown_files_exist() {
    assert!(!markdown_files(&docs_dir()).is_empty(), "No markdown files found in docs/");
}

/// Internal links in human-curated docs must resolve.
///
/// Auto-generated eval reports (`docs/reports/`) are excluded: they embed
/// paths to transient runtime artifacts (e.g.
/// `/private/tmp/aethyme-eval-demo-xxx/`) that don't exist outside the
/// run, by design. Subjecting them to the same contract as hand-written
/// documentation conflates human authoring discipline with runtime
/// capture.
#[test]
fn internal_links_valid() {
    let mut broken = Vec::new();
    for md_file in markdown_files(&docs_dir()) {
        if is_generated_report(&md_file) {
            continue;
        }
        for (link, line) in extract_links(&read(&md_file)) {
            if is_external(&link) || is_anchor(&link) {
                continue;
            }
            let target = md_file.parent().expect("markdown file has a parent").join(&link);
            if !target.exists() {
                broken.push(format!("  {}:{line} -> {link}", relative(&md_file)));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "Found broken internal links:\n{}",
        broken.join("\n")
    );
}

#[test]
fn no_absolute_github_links() {
    let mut absolute = Vec::new();
    for md_file in markdown_files(&docs_dir()) {
        for (link, line) in extract_links(&read(&md_file)) {
            if link.contains("github.com/aeptus/aethyme") && link.contains("/blob/") {
                absolute.push(format!("  {}:{line} -> {link}", relative(&md_file)));
            }
        }
    }
    assert!(
        absolute.is_empty(),
        "Found absolute GitHub links (use relative links instead):\n{}",
        absolute.join("\n")
    );
}

#[test]
fn required_documentation_exists() {
    let missing: Vec<&str> = [
        "getting-started/quickstart.md",
        "getting-started/onboarding.md",
        "reference/cli.md",
        "guides/testing.md",
        "guides/troubleshooting.md",
    ]
    .into_iter()
    .filter(|relative| !docs_dir().join(relative).exists())
    .collect();
    assert!(
        missing.is_empty(),
        "Missing required documentation:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn runbooks_have_standard_sections() {
    let runbooks = docs_dir().join("runbooks");
    if !runbooks.exists() {
        // The Python original called pytest.skip here; there is no
        // runbooks directory today, which is why the pytest run reported
        // exactly one skip.
        return;
    }
    for runbook in markdown_files(&runbooks) {
        let content = read(&runbook);
        let name = runbook.file_name().unwrap().to_string_lossy().into_owned();
        for section in ["## Overview", "## Symptoms"] {
            assert!(content.contains(section), "Runbook {name} missing section: {section}");
        }
        assert!(
            ["## Diagnostic", "## Detection"]
                .iter()
                .any(|option| content.contains(option)),
            "Runbook {name} missing one of: ## Diagnostic, ## Detection"
        );
    }
}

/// Hand-curated docs must carry "Last Updated:" or "Last Reviewed:".
///
/// Auto-generated eval reports (`docs/reports/`) are excluded: they're
/// timestamped by filename (e.g. `20260507-191139-...md`), and forcing
/// every auto-generated report through a "must have a stamp" contract is
/// a category mistake. The filename is the stamp.
#[test]
fn documentation_has_last_updated() {
    let mut missing = Vec::new();
    for md_file in markdown_files(&docs_dir()) {
        let name = md_file.file_name().unwrap().to_string_lossy().into_owned();
        if name == "README.md" || name == "INDEX.md" || is_generated_report(&md_file) {
            continue;
        }
        let content = read(&md_file);
        if !content.contains("Last Updated:") && !content.contains("Last Reviewed:") {
            missing.push(relative(&md_file));
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "Missing last-updated metadata:\n  {}",
        missing.join("\n  ")
    );
}

// ── tests/docs/test_examples.py ─────────────────────────────────────────

/// Markdown code fences as `(language, code, start_line)`.
fn find_code_blocks(text: &str) -> Vec<(String, String, usize)> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<String>> = None;
    let mut language = String::new();
    let mut start_line = 0;

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if current.is_none() && line.starts_with("```") {
            let tag: String = line[3..]
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            language = if tag.is_empty() { "text".to_string() } else { tag };
            current = Some(Vec::new());
            start_line = line_number;
        } else if line.trim() == "```" && current.is_some() {
            blocks.push((
                std::mem::take(&mut language),
                current.take().unwrap().join("\n"),
                start_line,
            ));
        } else if let Some(block) = current.as_mut() {
            block.push(line.trim_end().to_string());
        }
    }
    blocks
}

fn all_code_blocks() -> Vec<(PathBuf, String, String, usize)> {
    markdown_files(&docs_dir())
        .into_iter()
        .flat_map(|md_file| {
            find_code_blocks(&read(&md_file))
                .into_iter()
                .map(move |(language, code, line)| (md_file.clone(), language, code, line))
        })
        .collect()
}

/// The Python original compiled every `python` fence with CPython's
/// `compile()`. That check cannot be ported: `packages/aethyme` retired
/// its interpreter, so there is nothing left to compile with — and the
/// package's own docs, being documentation for a Rust-only package, hold
/// zero Python fences today.
///
/// Rather than drop the guard, it becomes the check that still means
/// something for a Python-free package: a Python example here would be
/// unverifiable by construction, so there must not be one. If a genuine
/// need for one appears (documenting `packages/aethyme-eval`, say), this
/// test is the deliberate conversation about where that example belongs.
#[test]
fn no_unverifiable_python_examples() {
    let offenders: Vec<String> = all_code_blocks()
        .into_iter()
        .filter(|(md_file, language, _, _)| {
            !is_generated_report(md_file) && matches!(language.as_str(), "python" | "py")
        })
        .map(|(md_file, _, _, line)| format!("  {}:{line}", relative(&md_file)))
        .collect();
    assert!(
        offenders.is_empty(),
        "packages/aethyme carries no Python interpreter, so a Python example \
         here cannot be syntax-checked. Move it to packages/aethyme-eval, or \
         show the native command instead:\n{}",
        offenders.join("\n")
    );
}

/// Bash code examples should pass shellcheck when available. Advisory:
/// the Python original printed and never failed, because shellcheck is
/// not guaranteed to be installed.
#[test]
fn bash_examples_syntax() {
    let mut issues = 0;
    for (md_file, language, code, line) in all_code_blocks() {
        if !matches!(language.as_str(), "bash" | "sh" | "shell") {
            continue;
        }
        if ["<", ">", "{", "}", "$1", "$2", "..."]
            .iter()
            .any(|placeholder| code.contains(placeholder))
        {
            continue;
        }
        let Ok(mut child) = std::process::Command::new("shellcheck")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        else {
            return; // shellcheck unavailable — same as the Python FileNotFoundError branch
        };
        use std::io::Write;
        let _ = child.stdin.as_mut().unwrap().write_all(code.as_bytes());
        let output = child.wait_with_output().expect("shellcheck");
        if !output.status.success() && String::from_utf8_lossy(&output.stdout).contains("error:") {
            issues += 1;
            eprintln!("shellcheck: {}:{line}", relative(&md_file));
        }
    }
    if issues > 0 {
        eprintln!("\nWarning: Found {issues} bash syntax issues");
    }
}

/// SQL code examples should contain at least one SQL keyword. Advisory.
#[test]
fn sql_examples_basic_syntax() {
    let mut issues = 0;
    for (md_file, language, code, line) in all_code_blocks() {
        if !matches!(language.as_str(), "sql" | "postgresql" | "postgres") {
            continue;
        }
        if ["{", "}", "...", "'...'"]
            .iter()
            .any(|placeholder| code.contains(placeholder))
        {
            continue;
        }
        let upper = code.to_uppercase();
        if !["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP"]
            .iter()
            .any(|keyword| upper.contains(keyword))
        {
            issues += 1;
            eprintln!("SQL block missing SQL keywords: {}:{line}", relative(&md_file));
        }
    }
    if issues > 0 {
        eprintln!("\nWarning: Found {issues} suspicious SQL blocks");
    }
}

/// curl examples should include a URL on command lines. Advisory.
#[test]
fn curl_examples_valid() {
    let mut issues = 0;
    for (md_file, language, code, line) in all_code_blocks() {
        if !matches!(language.as_str(), "bash" | "sh" | "shell") || !code.contains("curl") {
            continue;
        }
        let lines: Vec<&str> = code.split('\n').collect();
        for (index, entry) in lines.iter().enumerate() {
            if entry.contains("curl") && !entry.contains("http") && index == lines.len() - 1 {
                issues += 1;
                eprintln!(
                    "curl command missing URL: {}:{}",
                    relative(&md_file),
                    line + index
                );
            }
        }
    }
    if issues > 0 {
        eprintln!("\nWarning: Found {issues} potential curl issues");
    }
}

/// JSON code examples must parse when not placeholder snippets.
#[test]
fn json_examples_valid() {
    let mut errors = Vec::new();
    for (md_file, language, code, line) in all_code_blocks() {
        if !matches!(language.as_str(), "json" | "jsonc") {
            continue;
        }
        if ["...", "//", "/*", "{...}", "$", "<"]
            .iter()
            .any(|marker| code.contains(marker))
        {
            continue;
        }
        if let Err(error) = serde_json::from_str::<serde_json::Value>(&code) {
            errors.push(format!("  {}:{line} - {error}", relative(&md_file)));
        }
    }
    assert!(
        errors.is_empty(),
        "Found invalid JSON in documentation:\n{}",
        errors.join("\n")
    );
}

/// Code blocks should declare a language when non-trivial. Advisory.
#[test]
fn code_blocks_have_language() {
    let missing: Vec<String> = all_code_blocks()
        .into_iter()
        .filter(|(_, language, code, _)| language == "text" && code.len() > 10)
        .map(|(md_file, _, _, line)| format!("{}:{line}", relative(&md_file)))
        .collect();
    if !missing.is_empty() {
        eprintln!("\nWarning: Found {} code blocks without language", missing.len());
    }
}
