//! Agent prompt generation with pre-computed navigation context.
//!
//! Generates complete, paste-ready prompts that include task description
//! plus curated file excerpts derived from the repository graph.
//! The agent receives pre-digested context — not raw graph data.

use std::fs;
use std::path::Path;

use crate::graph::overview::build_repo_overview;
use crate::map::RepositoryMap;
use crate::model::edge::EdgeKind;
use crate::model::file::FileRole;

/// Maximum lines to read from each file header.
const HEADER_LINES: usize = 40;

/// Generate a complete agent-ready prompt with navigation context.
pub fn generate_prompt(
    repo_root: &Path,
    map: &RepositoryMap,
    task: &str,
    focus: Option<&str>,
) -> String {
    let focus = focus.unwrap_or("overview");

    let context = match focus {
        "overview" => build_overview_context(repo_root, map),
        "testing" => build_testing_context(repo_root, map),
        "docs" | "documentation" => build_docs_context(repo_root, map),
        "configs" | "configuration" => build_configs_context(repo_root, map),
        // Treat any other focus as an area name
        area => build_area_context(repo_root, map, area),
    };

    let framing = match focus {
        "overview" => "\
The navigation context below is a pre-computed map of the repository structure \
derived from graph analysis. Use it as your starting point — validate and deepen \
by reading the actual source files listed. Focus on architecture, not surface \
descriptions.",
        "testing" => "\
The navigation context below maps the testing infrastructure derived from graph \
analysis. Use it to locate test frameworks, configurations, and directories, then \
read the actual test files to understand the testing strategy.",
        _ => "\
The navigation context below is a pre-computed map of this subsystem derived from \
graph analysis. Use it as your starting point — validate by reading the source files \
listed. Trace connections to understand how this area fits into the larger codebase.",
    };

    format!(
        "{task}\n\n\
         Repository path: {root}\n\n\
         {framing}\n\n\
         ## Navigation Context\n\n\
         {context}",
        task = task,
        root = repo_root.display(),
        framing = framing,
        context = context,
    )
}

// ── Overview context ────────────────────────────────────────────────

fn build_overview_context(root: &Path, map: &RepositoryMap) -> String {
    let mut sections = Vec::new();

    // Languages + size
    let lang_str = if map.snapshot.languages.is_empty() {
        "unknown".to_string()
    } else {
        map.snapshot.languages.join(", ")
    };
    sections.push(format!(
        "**Repository:** {} files, languages: {}\n",
        map.files.len(),
        lang_str,
    ));

    // README excerpt — prefer root README.md over nested READMEs
    let readme_path = find_root_readme(map);
    if let Some(path) = &readme_path {
        if let Some(header) = read_file_header(root, path, 25) {
            sections.push(format!("### README (`{path}`)\n```\n{header}\n```\n"));
        }
    }

    // Root-level entry points (source files at repo root)
    let root_sources: Vec<&str> = map
        .files
        .iter()
        .filter(|f| {
            matches!(f.role, FileRole::Source)
                && !f.path.contains('/')
        })
        .map(|f| f.path.as_str())
        .collect();

    if !root_sources.is_empty() {
        let mut entry_section = String::from("### Entry Points (root-level source files)\n");
        for path in root_sources.iter().take(8) {
            let description = describe_entry_point(root, path);
            entry_section.push_str(&format!("- `{path}` — {description}\n"));
        }
        sections.push(entry_section);
    }

    // Bootstrap chain — trace from entry points into includes/
    let bootstrap_files = find_bootstrap_chain(root, map, &root_sources);
    if !bootstrap_files.is_empty() {
        let mut section = String::from("### Bootstrap Chain\n");
        section.push_str(&bootstrap_files.join(" → "));
        section.push('\n');
        sections.push(section);
    }

    // Manifests
    let manifests: Vec<&str> = map
        .configs
        .iter()
        .filter(|c| {
            !c.path.contains('/')
                && matches!(
                    c.config_type.as_str(),
                    "package_json" | "composer" | "cargo" | "pyproject" | "gemfile" | "go_mod"
                )
        })
        .map(|c| c.path.as_str())
        .collect();

    if !manifests.is_empty() {
        let mut section = String::from("### Manifests\n");
        for path in manifests.iter().take(4) {
            let desc = describe_manifest(root, path);
            section.push_str(&format!("- `{path}` — {desc}\n"));
        }
        sections.push(section);
    }

    // Top areas by file count (excluding vendor/generated)
    let excluded = super::snippets::EXCLUDED_AREAS;
    let mut area_counts: Vec<(&str, usize)> = map
        .areas
        .iter()
        .filter(|a| {
            !a.inferred
                && !a.path_prefix.contains('/')
                && !a.name.starts_with('.')
                && !excluded.contains(&a.path_prefix.as_str())
        })
        .map(|a| {
            let count = map
                .files
                .iter()
                .filter(|f| f.path.starts_with(&format!("{}/", a.path_prefix)))
                .count();
            (a.path_prefix.as_str(), count)
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    area_counts.sort_by(|a, b| b.1.cmp(&a.1));

    if !area_counts.is_empty() {
        let mut section = String::from("### Structure (top directories by file count)\n");
        for (name, count) in area_counts.iter().take(10) {
            // Add sub-area summary for the largest areas
            let sub_areas = top_sub_areas(map, name, 5);
            if sub_areas.is_empty() {
                section.push_str(&format!("- `{name}/` ({count} files)\n"));
            } else {
                let subs = sub_areas.join(", ");
                section.push_str(&format!("- `{name}/` ({count} files) — key: {subs}\n"));
            }
        }
        sections.push(section);
    }

    // Suggested starting files for exploration
    let starting_files = suggest_starting_files(root, map);
    if !starting_files.is_empty() {
        let mut section = String::from("### Suggested Starting Files\n");
        for (path, reason) in &starting_files {
            section.push_str(&format!("- `{path}` — {reason}\n"));
        }
        sections.push(section);
    }

    sections.join("\n")
}

/// Generate detailed subsystem context: all public functions with their files.
/// This is appended to the main prompt when --subsystem is specified.
pub fn generate_subsystem_context(
    _repo_root: &Path,
    map: &RepositoryMap,
    subsystem: &str,
) -> String {
    let mut sections = Vec::new();
    let subsystem = subsystem.trim_end_matches('/');
    let subsystem_prefix = format!("{subsystem}/");

    let area_files: Vec<&str> = map
        .files
        .iter()
        .filter(|f| f.path.starts_with(&subsystem_prefix))
        .map(|f| f.path.as_str())
        .collect();

    sections.push(format!(
        "## Subsystem Detail: `{subsystem}/`\n\n\
         **Files:** {}\n",
        area_files.len()
    ));

    // List all classes in the subsystem
    let classes: Vec<String> = map
        .classes
        .iter()
        .filter(|c| c.file_path.starts_with(&subsystem_prefix))
        .map(|c| format!("- class `{}` in `{}`", c.name, c.file_path))
        .collect();

    if !classes.is_empty() {
        let mut section = String::from("### Classes\n");
        for c in &classes {
            section.push_str(&format!("{c}\n"));
        }
        sections.push(section);
    }

    // List public-like methods plus top-level functions with their files.
    let functions: Vec<String> = map
        .functions
        .iter()
        .filter(|f| {
            f.file_path.starts_with(&subsystem_prefix)
                && (f.parent_class_id.is_none() || f.signature.trim_start().starts_with("public "))
                && f.name != "__construct"
                && f.name != "__destruct"
                && f.name != "__init__"
        })
        .map(|f| format!("- `{}()` in `{}`", f.name, f.file_path))
        .collect();

    if !functions.is_empty() {
        let mut section = format!("### Public Functions ({} total)\n", functions.len());
        for f in &functions {
            section.push_str(&format!("{f}\n"));
        }
        sections.push(section);
    }

    // Files outside subsystem that import files from it
    let external_importers: Vec<String> = map
        .edges
        .iter()
        .filter(|e| {
            matches!(e.kind, EdgeKind::Imports)
                && !e.from.starts_with(&subsystem_prefix)
                && e.to.starts_with(&subsystem_prefix)
        })
        .map(|e| format!("- `{}` imports `{}`", e.from, e.to))
        .take(20)
        .collect();

    if !external_importers.is_empty() {
        let mut section = format!("### External Consumers ({} files import from this subsystem)\n", external_importers.len());
        for imp in &external_importers {
            section.push_str(&format!("{imp}\n"));
        }
        sections.push(section);
    }

    sections.join("\n")
}

// ── Area-specific context ───────────────────────────────────────────

fn build_area_context(root: &Path, map: &RepositoryMap, area_name: &str) -> String {
    let mut sections = Vec::new();

    // Find files in this area
    let area_files: Vec<&str> = map
        .files
        .iter()
        .filter(|f| f.path.starts_with(&format!("{area_name}/")))
        .map(|f| f.path.as_str())
        .collect();

    sections.push(format!(
        "**Area:** `{area_name}/` ({} files)\n",
        area_files.len()
    ));

    // Sub-directories
    let mut subdirs: Vec<(&str, usize)> = map
        .areas
        .iter()
        .filter(|a| {
            a.path_prefix.starts_with(&format!("{area_name}/"))
                && a.path_prefix.matches('/').count() == area_name.matches('/').count() + 1
        })
        .map(|a| {
            let count = area_files
                .iter()
                .filter(|f| f.starts_with(&format!("{}/", a.path_prefix)))
                .count();
            (a.path_prefix.as_str(), count)
        })
        .filter(|(_, c)| *c > 0)
        .collect();
    subdirs.sort_by(|a, b| b.1.cmp(&a.1));

    if !subdirs.is_empty() {
        let mut section = String::from("### Sub-directories\n");
        for (name, count) in subdirs.iter().take(10) {
            section.push_str(&format!("- `{name}/` ({count} files)\n"));
        }
        sections.push(section);
    }

    // Key files: look for index/main/init/mod files, README, configs
    let key_file_patterns = ["index.", "main.", "__init__.", "mod.rs", "README", "package.json"];
    let key_files: Vec<&str> = area_files
        .iter()
        .filter(|f| {
            let name = f.rsplit('/').next().unwrap_or(f);
            key_file_patterns.iter().any(|p| name.starts_with(p) || name == *p)
        })
        .copied()
        .take(8)
        .collect();

    if !key_files.is_empty() {
        let mut section = String::from("### Key Files\n");
        for path in &key_files {
            if let Some(header) = read_file_header(root, path, 15) {
                let first_line = header.lines().next().unwrap_or("");
                section.push_str(&format!("- `{path}`: {first_line}\n"));
            }
        }
        sections.push(section);
    }

    // Symbols in this area (classes and top-level functions)
    let area_symbols: Vec<String> = map
        .classes
        .iter()
        .filter(|c| c.file_path.starts_with(&format!("{area_name}/")))
        .map(|c| format!("class `{}` in `{}`", c.name, c.file_path))
        .chain(
            map.functions
                .iter()
                .filter(|f| {
                    f.file_path.starts_with(&format!("{area_name}/"))
                        && f.name != "__construct"
                        && f.name != "__init__"
                })
                .take(15)
                .map(|f| format!("`{}()` in `{}`", f.name, f.file_path)),
        )
        .take(15)
        .collect();

    if !area_symbols.is_empty() {
        let mut section = String::from("### Key Symbols\n");
        for sym in &area_symbols {
            section.push_str(&format!("- {sym}\n"));
        }
        sections.push(section);
    }

    // Imports into/from this area
    let imports_from: Vec<String> = map
        .edges
        .iter()
        .filter(|e| {
            matches!(e.kind, EdgeKind::Imports)
                && e.from.contains(&format!("{area_name}/"))
                && !e.to.contains(&format!("{area_name}/"))
        })
        .map(|e| map.display_for(&e.to))
        .take(10)
        .collect();

    if !imports_from.is_empty() {
        let mut section = String::from("### External Dependencies (imports from outside this area)\n");
        let mut seen = std::collections::HashSet::new();
        for dep in &imports_from {
            if seen.insert(dep.clone()) {
                section.push_str(&format!("- `{dep}`\n"));
            }
        }
        sections.push(section);
    }

    sections.join("\n")
}

// ── Testing context ─────────────────────────────────────────────────

fn build_testing_context(root: &Path, map: &RepositoryMap) -> String {
    let mut sections = Vec::new();

    let test_files: Vec<&str> = map
        .files
        .iter()
        .filter(|f| matches!(f.role, FileRole::Test))
        .map(|f| f.path.as_str())
        .collect();

    sections.push(format!("**Test files:** {}\n", test_files.len()));

    // Test configs
    let test_configs: Vec<&str> = map
        .configs
        .iter()
        .filter(|c| {
            let name = c.path.rsplit('/').next().unwrap_or(&c.path);
            name.contains("test")
                || name.contains("jest")
                || name.contains("vitest")
                || name.contains("phpunit")
                || name.contains("pytest")
                || name.contains("mocha")
        })
        .map(|c| c.path.as_str())
        .collect();

    if !test_configs.is_empty() {
        let mut section = String::from("### Test Configuration\n");
        for path in test_configs.iter().take(5) {
            if let Some(header) = read_file_header(root, path, 25) {
                section.push_str(&format!("**{path}:**\n```\n{header}\n```\n"));
            }
        }
        sections.push(section);
    }

    // Test directories
    let mut test_dirs: Vec<(&str, usize)> = map
        .areas
        .iter()
        .filter(|a| {
            let name = a.path_prefix.rsplit('/').next().unwrap_or(&a.path_prefix);
            name.contains("test") || name.contains("spec") || name == "__tests__"
        })
        .map(|a| {
            let count = test_files
                .iter()
                .filter(|f| f.starts_with(&format!("{}/", a.path_prefix)))
                .count();
            (a.path_prefix.as_str(), count)
        })
        .filter(|(_, c)| *c > 0)
        .collect();
    test_dirs.sort_by(|a, b| b.1.cmp(&a.1));

    if !test_dirs.is_empty() {
        let mut section = String::from("### Test Directories\n");
        for (name, count) in test_dirs.iter().take(8) {
            section.push_str(&format!("- `{name}/` ({count} test files)\n"));
        }
        sections.push(section);
    }

    sections.join("\n")
}

// ── Docs context ────────────────────────────────────────────────────

fn build_docs_context(root: &Path, map: &RepositoryMap) -> String {
    let mut sections = Vec::new();

    sections.push(format!("**Documentation files:** {}\n", map.docs.len()));

    // Group by type
    let readmes: Vec<&str> = map.docs.iter().filter(|d| d.doc_type == "readme").map(|d| d.path.as_str()).collect();
    let architecture: Vec<&str> = map.docs.iter().filter(|d| d.doc_type == "architecture").map(|d| d.path.as_str()).collect();
    let guides: Vec<&str> = map.docs.iter().filter(|d| d.doc_type != "readme" && d.doc_type != "architecture").map(|d| d.path.as_str()).take(10).collect();

    if !readmes.is_empty() {
        let mut section = String::from("### READMEs\n");
        for path in readmes.iter().take(5) {
            section.push_str(&format!("- `{path}`\n"));
        }
        sections.push(section);
    }
    if !architecture.is_empty() {
        let mut section = String::from("### Architecture Docs\n");
        for path in &architecture {
            section.push_str(&format!("- `{path}`\n"));
        }
        sections.push(section);
    }
    if !guides.is_empty() {
        let mut section = String::from("### Other Documentation\n");
        for path in &guides {
            section.push_str(&format!("- `{path}`\n"));
        }
        sections.push(section);
    }

    sections.join("\n")
}

// ── Configs context ─────────────────────────────────────────────────

fn build_configs_context(root: &Path, map: &RepositoryMap) -> String {
    let mut sections = Vec::new();

    sections.push(format!("**Configuration files:** {}\n", map.configs.len()));

    // Root-level configs with excerpts
    let root_configs: Vec<&str> = map
        .configs
        .iter()
        .filter(|c| !c.path.contains('/'))
        .map(|c| c.path.as_str())
        .collect();

    if !root_configs.is_empty() {
        let mut section = String::from("### Root Configuration\n");
        for path in root_configs.iter().take(8) {
            if let Some(header) = read_file_header(root, path, 15) {
                section.push_str(&format!("**{path}:**\n```\n{header}\n```\n"));
            }
        }
        sections.push(section);
    }

    sections.join("\n")
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Read the first N lines of a file. Returns None if the file can't be read.
fn read_file_header(root: &Path, relative_path: &str, max_lines: usize) -> Option<String> {
    let content = fs::read_to_string(root.join(relative_path)).ok()?;
    let lines: Vec<&str> = content.lines().take(max_lines).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// Find the root README file, preferring README.md over nested READMEs.
fn find_root_readme(map: &RepositoryMap) -> Option<String> {
    // Prefer root-level README.md
    let root_readme = map.files.iter().find(|f| {
        !f.path.contains('/')
            && (f.path.eq_ignore_ascii_case("README.md")
                || f.path.eq_ignore_ascii_case("README")
                || f.path.eq_ignore_ascii_case("README.txt")
                || f.path.eq_ignore_ascii_case("README.rst"))
    });
    if let Some(f) = root_readme {
        return Some(f.path.clone());
    }
    // Fall back to snapshot readme_path
    map.snapshot.readme_path.clone()
}

/// Language-agnostic boilerplate lines to skip when describing entry points.
const BOILERPLATE: &[&str] = &[
    // PHP
    "<?php", "<?", "declare(strict_types",
    // JS/TS
    "'use strict'", "\"use strict\"", "Object.defineProperty(exports",
    // Python
    "#!/usr/bin/env python", "# -*- coding:",
    // Ruby
    "# frozen_string_literal:",
    // Rust
    "#![allow(", "#![deny(", "#![cfg(",
    // Go
    "//go:build",
    // Generic
    "/*", "*/",
];

/// Is this line boilerplate that should be skipped?
fn is_boilerplate(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return true;
    }
    // Direct prefix matches
    for pattern in BOILERPLATE {
        if trimmed.starts_with(pattern) {
            return true;
        }
    }
    // Comment-only lines (all languages)
    if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') {
        return true;
    }
    // Bare braces/parens
    if trimmed == "{" || trimmed == "}" || trimmed == "(" || trimmed == ")" {
        return true;
    }
    // Package/module declarations (Go, Java, Rust)
    if trimmed.starts_with("package ") || trimmed.starts_with("module ") {
        return true;
    }
    false
}

/// Describe a root-level entry point by reading past boilerplate.
/// Works across PHP, JS/TS, Python, Ruby, Rust, Go, Java.
fn describe_entry_point(root: &Path, path: &str) -> String {
    let content = match fs::read_to_string(root.join(path)) {
        Ok(c) => c,
        Err(_) => return "source file".to_string(),
    };

    // Strategy 1: Look for doc comments (/** ... */, """ ... """, /// ..., # ...)
    if let Some(desc) = extract_doc_comment(&content) {
        return desc;
    }

    // Strategy 2: Look for Python/Ruby module docstring
    if let Some(desc) = extract_module_docstring(&content) {
        return desc;
    }

    // Strategy 3: First meaningful (non-boilerplate) line
    let meaningful = content
        .lines()
        .map(|l| l.trim())
        .find(|l| !is_boilerplate(l));

    if let Some(line) = meaningful {
        return line.chars().take(100).collect();
    }

    "source file".to_string()
}

/// Extract description from C-style doc comments (/** ... */), Rust (///), or # comments.
fn extract_doc_comment(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    // Skip leading boilerplate to find the first doc comment
    let start = lines.iter().position(|l| {
        let t = l.trim();
        t.starts_with("/**") || t.starts_with("///") || t.starts_with("\"\"\"")
    })?;

    let mut desc_lines = Vec::new();

    if lines[start].trim().starts_with("///") {
        // Rust-style /// comments
        for line in &lines[start..] {
            let t = line.trim();
            if t.starts_with("///") {
                let text = t.trim_start_matches('/').trim();
                if !text.is_empty() {
                    desc_lines.push(text);
                }
            } else {
                break;
            }
            if desc_lines.len() >= 2 { break; }
        }
    } else {
        // C-style /** ... */ or /* ... */
        for line in &lines[start..] {
            let t = line.trim();
            if t == "/**" || t == "/*" { continue; }
            if t.starts_with("*/") { break; }
            let text = t.trim_start_matches('*').trim();
            if !text.is_empty() && !text.starts_with('@') && !text.starts_with("\\") {
                desc_lines.push(text);
            }
            if desc_lines.len() >= 2 { break; }
        }
    }

    if desc_lines.is_empty() {
        return None;
    }
    Some(desc_lines.join(" "))
}

/// Extract Python/Ruby module-level docstring (triple-quoted or =begin).
fn extract_module_docstring(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    // Find triple-quote docstring after boilerplate
    let start = lines.iter().position(|l| {
        let t = l.trim();
        t.starts_with("\"\"\"") || t.starts_with("'''")
    })?;

    let quote = if lines[start].trim().starts_with("\"\"\"") { "\"\"\"" } else { "'''" };
    let mut desc_lines = Vec::new();

    // Check for single-line docstring: """text"""
    let first = lines[start].trim().trim_start_matches(quote);
    if first.ends_with(quote) {
        return Some(first.trim_end_matches(quote).trim().to_string());
    }
    if !first.is_empty() {
        desc_lines.push(first);
    }

    for line in &lines[start + 1..] {
        let t = line.trim();
        if t.contains(quote) { break; }
        if !t.is_empty() {
            desc_lines.push(t);
        }
        if desc_lines.len() >= 2 { break; }
    }

    if desc_lines.is_empty() { return None; }
    Some(desc_lines.join(" "))
}

/// Generic bootstrap file name patterns (language-agnostic).
const BOOTSTRAP_NAMES: &[&str] = &[
    // Generic
    "Bootstrap", "bootstrap", "Setup", "setup", "startup", "Startup",
    "init", "Init", "Application", "application",
    "Kernel", "kernel", "Server", "server",
    // DI / service containers
    "Services", "services", "Container", "container",
    // App entry orchestration
    "App", "Main",
];

/// Import/require keywords across languages.
const IMPORT_KEYWORDS: &[&str] = &[
    // PHP
    "require", "include", "require_once", "include_once",
    // JS/TS
    "import ", "require(",
    // Python
    "from ", "import ",
    // Go
    "import ",
    // Rust
    "use ", "mod ",
    // Ruby
    "require ", "require_relative ",
    // Java/Kotlin
    "import ",
];

/// Find bootstrap/initialization files by tracing imports from entry points.
/// Language-agnostic: works across PHP, JS, Python, Go, Rust, Ruby.
fn find_bootstrap_chain(root: &Path, map: &RepositoryMap, root_sources: &[&str]) -> Vec<String> {
    let mut chain = Vec::new();

    // Check root entry points for references to bootstrap-named files
    for src in root_sources.iter().take(4) {
        if let Ok(content) = fs::read_to_string(root.join(src)) {
            for line in content.lines() {
                let trimmed = line.trim();
                // Does this line contain an import/require keyword?
                let has_import = IMPORT_KEYWORDS.iter().any(|kw| trimmed.contains(kw));
                if !has_import { continue; }

                // Does it reference a bootstrap-named file?
                for pattern in BOOTSTRAP_NAMES {
                    if trimmed.contains(pattern) {
                        if let Some(file) = extract_import_path(trimmed) {
                            if !chain.contains(&file) {
                                chain.push(file);
                            }
                        }
                    }
                }
            }
        }
        if !chain.is_empty() { break; }
    }

    // Also scan for well-known bootstrap files in the repo (depth <= 1)
    for file in &map.files {
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let stem = name.split('.').next().unwrap_or(name);
        let is_bootstrap = BOOTSTRAP_NAMES.iter().any(|p| stem == *p || stem.contains(p));
        let shallow = file.path.matches('/').count() <= 1;
        let not_test = !file.path.contains("test") && !file.path.contains("spec")
            && !file.path.contains("mock") && !file.path.contains("fixture");

        if is_bootstrap && shallow && not_test {
            let entry = format!("`{}`", file.path);
            if !chain.iter().any(|c| c.contains(&file.path)) {
                chain.push(entry);
            }
        }
    }

    chain.truncate(5);
    chain
}

/// Extract a file path from an import/require line (language-agnostic).
fn extract_import_path(line: &str) -> Option<String> {
    // Single-quoted path: require '/path/to/file'
    if let Some(path) = extract_between(line, "'", "'") {
        let clean = path.trim_start_matches("./").trim_start_matches('/');
        if clean.contains('/') || clean.contains('.') {
            return Some(format!("`{clean}`"));
        }
    }
    // Double-quoted path: require "/path/to/file"
    if let Some(path) = extract_between(line, "\"", "\"") {
        let clean = path.trim_start_matches("./").trim_start_matches('/');
        if clean.contains('/') || clean.contains('.') {
            return Some(format!("`{clean}`"));
        }
    }
    // Python: from x.y.z import ... → x/y/z
    if line.trim().starts_with("from ") {
        let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
        if parts.len() >= 2 {
            let module = parts[1].replace('.', "/");
            if module.contains('/') {
                return Some(format!("`{module}`"));
            }
        }
    }
    None
}

/// Extract text between two delimiters (first occurrence).
fn extract_between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let rest = &text[start..];
    let end = rest.find(close)?;
    Some(&rest[..end])
}

/// Describe a manifest file with a one-line summary.
fn describe_manifest(root: &Path, path: &str) -> String {
    let content = match fs::read_to_string(root.join(path)) {
        Ok(c) => c,
        Err(_) => return "project manifest".to_string(),
    };

    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "composer.json" => {
            let desc = extract_json_field(&content, "description")
                .unwrap_or_else(|| "PHP dependencies".to_string());
            format!("{desc}")
        }
        "package.json" => {
            let desc = extract_json_field(&content, "description")
                .unwrap_or_else(|| "JS/Node dependencies".to_string());
            format!("{desc}")
        }
        "Cargo.toml" => "Rust project manifest".to_string(),
        "pyproject.toml" | "setup.py" | "setup.cfg" => "Python project manifest".to_string(),
        "Gemfile" => "Ruby dependencies".to_string(),
        "go.mod" => "Go module definition".to_string(),
        _ => "project manifest".to_string(),
    }
}

/// Extract a string field from a JSON file (quick and dirty, no full parse).
fn extract_json_field(content: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\"", field);
    let pos = content.find(&pattern)?;
    let rest = &content[pos + pattern.len()..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let value_start = 1; // skip opening quote
    let value_end = after_colon[value_start..].find('"')?;
    Some(after_colon[value_start..value_start + value_end].to_string())
}

/// Get the top sub-areas (by file count) within a parent area.
fn top_sub_areas(map: &RepositoryMap, parent: &str, limit: usize) -> Vec<String> {
    let mut subs: Vec<(&str, usize)> = map
        .areas
        .iter()
        .filter(|a| {
            a.path_prefix.starts_with(&format!("{parent}/"))
                && a.path_prefix[parent.len() + 1..].matches('/').count() == 0
        })
        .map(|a| {
            let name = &a.path_prefix[parent.len() + 1..];
            let count = map
                .files
                .iter()
                .filter(|f| f.path.starts_with(&format!("{}/", a.path_prefix)))
                .count();
            (name, count)
        })
        .filter(|(_, c)| *c >= 10)
        .collect();

    subs.sort_by(|a, b| b.1.cmp(&a.1));
    subs.into_iter()
        .take(limit)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect()
}

/// Patterns for files that are structurally interesting starting points.
const STARTING_FILE_PATTERNS: &[(&str, &str)] = &[
    // DI / service containers (any language)
    ("services.yaml", "service/DI configuration"),
    ("services.xml", "service/DI configuration"),
    ("container.ts", "dependency injection container"),
    ("container.js", "dependency injection container"),
    ("injector.ts", "dependency injection"),
    ("providers.ts", "dependency providers"),
    // Route/API definitions
    ("routes.ts", "route definitions"),
    ("routes.js", "route definitions"),
    ("routes.py", "route definitions"),
    ("urls.py", "URL routing"),
    ("router.ts", "request routing"),
    ("router.js", "request routing"),
    // Database / models
    ("schema.prisma", "database schema"),
    ("models.py", "data models"),
    ("schema.ts", "schema definitions"),
    ("migrations", "database migrations"),
];

/// Suggest key starting files for exploration based on structural heuristics.
/// Language-agnostic: looks for architecture docs, DI wiring, hooks/events,
/// route definitions, and other structurally significant files.
fn suggest_starting_files(_root: &Path, map: &RepositoryMap) -> Vec<(String, String)> {
    let mut suggestions: Vec<(String, String)> = Vec::new();

    // Architecture/design docs
    for doc in &map.docs {
        if doc.doc_type == "architecture" {
            suggestions.push((doc.path.clone(), "architecture documentation".to_string()));
        }
    }

    // DI / service wiring / container files (by name pattern, any language)
    for file in &map.files {
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let stem = name.split('.').next().unwrap_or(name).to_lowercase();

        // Service wiring / container patterns
        if (stem.contains("service") && (stem.contains("wir") || stem.contains("container") || stem.contains("provider")))
            || stem == "services"
            || stem == "container"
            || stem == "injector"
        {
            let depth = file.path.matches('/').count();
            let not_test = !file.path.contains("test") && !file.path.contains("mock");
            if depth <= 2 && not_test {
                suggestions.push((file.path.clone(), "service/DI wiring".to_string()));
            }
        }
    }

    // Named pattern matches (routes, schemas, etc.)
    for (pattern, reason) in STARTING_FILE_PATTERNS {
        for file in &map.files {
            let name = file.path.rsplit('/').next().unwrap_or(&file.path);
            if name == *pattern && file.path.matches('/').count() <= 1 {
                if !suggestions.iter().any(|(p, _)| p == &file.path) {
                    suggestions.push((file.path.clone(), reason.to_string()));
                }
            }
        }
    }

    // Hooks / event / plugin system docs (any framework)
    for doc in &map.docs {
        let name_lower = doc.path.to_lowercase();
        if name_lower.contains("hook") || name_lower.contains("event")
            || name_lower.contains("plugin") || name_lower.contains("middleware")
        {
            if !suggestions.iter().any(|(p, _)| p == &doc.path) {
                suggestions.push((doc.path.clone(), "extensibility documentation".to_string()));
            }
        }
    }

    suggestions.truncate(6);
    suggestions
}
