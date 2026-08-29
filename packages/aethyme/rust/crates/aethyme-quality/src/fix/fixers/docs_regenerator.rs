//! Port of `src/autofixers/fixers/docs_regenerator.py`.
//!
//! Unlike the other four fixers this one CREATES files rather than
//! rewriting them, so it bypasses the `Fixer::fix` path entirely
//! (`can_fix` is `false` and `fix` is `None` in the Python too) and
//! exposes `create_folder_docs` instead.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::fix::fixers::base::{FixProposal, Fixer};
use crate::fix::pystr;
use crate::walk;

pub const FOLDER_DOC_NAME: &str = "FOLDER.md";

/// The Python `skip_dirs` set — note this is a DIFFERENT list from the
/// safety engine's `BUILD_DIRS` (it adds `.git` and `vendor`, drops
/// `env`/`.coverage`), and it is matched against the FULL path's parts,
/// so a repository checked out under e.g. `build/` finds nothing.
const SKIP_DIRS: [&str; 15] = [
    "node_modules",
    "__pycache__",
    ".git",
    "venv",
    ".venv",
    "dist",
    "build",
    ".pytest_cache",
    ".mypy_cache",
    "coverage",
    ".next",
    ".nuxt",
    "out",
    "target",
    "vendor",
];

const CODE_SUFFIXES: [&str; 8] = [".py", ".js", ".ts", ".tsx", ".jsx", ".go", ".rs", ".java"];

pub struct DocsRegenerator {
    repo_path: PathBuf,
    py_docstring: Regex,
    py_docstring_single: Regex,
    js_docblock: Regex,
}

impl DocsRegenerator {
    pub fn new(repo_path: &Path) -> Self {
        DocsRegenerator {
            repo_path: repo_path.to_path_buf(),
            py_docstring: Regex::new(r#"(?s)"""(.*?)""""#).unwrap(),
            py_docstring_single: Regex::new(r"(?s)'''(.*?)'''").unwrap(),
            js_docblock: Regex::new(r"(?s)/\*\*(.*?)\*/").unwrap(),
        }
    }

    /// Port of `find_directories_missing_folder_doc`.
    pub fn find_directories_missing_folder_doc(&self) -> Vec<PathBuf> {
        let mut missing = Vec::new();
        for entry in walk::rglob_all(&self.repo_path) {
            if !entry.is_dir {
                continue;
            }
            let dirpath = &entry.path;
            let parts = pystr::named_parts(dirpath);
            if SKIP_DIRS
                .iter()
                .any(|skip| parts.iter().any(|part| part == skip))
            {
                continue;
            }

            let has_code = read_dir_sorted(dirpath, false).iter().any(|item| {
                item.is_file() && CODE_SUFFIXES.contains(&walk::py_suffix(item).as_str())
            });
            if !has_code {
                continue;
            }

            if !dirpath.join(FOLDER_DOC_NAME).exists() {
                missing.push(dirpath.clone());
            }
        }
        missing
    }

    /// Port of `generate_folder_doc`.
    pub fn generate_folder_doc(&self, directory: &Path) -> String {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut subdirs: Vec<PathBuf> = Vec::new();
        // `sorted(directory.iterdir())` — Path ordering on POSIX is
        // ordering of the full path string.
        for item in read_dir_sorted(directory, true) {
            if pystr::file_name(&item).starts_with('.') {
                continue;
            }
            if item.is_file() {
                files.push(item);
            } else if item.is_dir() {
                subdirs.push(item);
            }
        }

        let rel_path = match directory.strip_prefix(&self.repo_path) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            Ok(_) => PathBuf::from("."),
            Err(_) => directory.to_path_buf(),
        };

        let mut lines: Vec<String> = vec![
            format!("# {}", pystr::file_name(directory)),
            String::new(),
            format!("**Location:** `{}`", pystr::as_posix(&rel_path)),
            String::new(),
            "## Overview".to_string(),
            String::new(),
            format!(
                "This directory contains {} files and {} subdirectories.",
                files.len(),
                subdirs.len()
            ),
            String::new(),
        ];

        if !files.is_empty() {
            // Grouping by extension; the keys are then sorted, so the
            // grouping's own insertion order does not leak.
            let mut files_by_ext: Vec<(String, Vec<PathBuf>)> = Vec::new();
            for f in &files {
                let suffix = walk::py_suffix(f);
                let ext = if suffix.is_empty() {
                    "no extension".to_string()
                } else {
                    suffix
                };
                match files_by_ext.iter_mut().find(|(k, _)| *k == ext) {
                    Some((_, bucket)) => bucket.push(f.clone()),
                    None => files_by_ext.push((ext, vec![f.clone()])),
                }
            }
            files_by_ext.sort_by(|a, b| a.0.cmp(&b.0));

            lines.push("## Files".to_string());
            lines.push(String::new());
            for (ext, bucket) in &files_by_ext {
                lines.push(format!("### {ext}"));
                lines.push(String::new());
                let mut sorted_bucket = bucket.clone();
                sorted_bucket.sort();
                for f in &sorted_bucket {
                    let name = pystr::file_name(f);
                    match self.extract_file_purpose(f) {
                        Some(purpose) => lines.push(format!("- `{name}` - {purpose}")),
                        None => lines.push(format!("- `{name}`")),
                    }
                }
                lines.push(String::new());
            }
        }

        if !subdirs.is_empty() {
            lines.push("## Subdirectories".to_string());
            lines.push(String::new());
            for d in &subdirs {
                lines.push(format!("- `{}/`", pystr::file_name(d)));
            }
            lines.push(String::new());
        }

        lines.push("---".to_string());
        lines.push(String::new());
        lines.push("*This file was auto-generated by Aethyme Autofixer.*".to_string());
        lines.join("\n")
    }

    /// Port of `_extract_file_purpose`: the first 20 lines, decoded
    /// with `errors="ignore"`, searched for a module docstring (Python)
    /// or a `/** ... */` block (JS/TS), first line truncated to 100
    /// characters.
    ///
    /// Because only 20 lines are read, a docstring whose opening and
    /// closing delimiters straddle the window produces no match — the
    /// regex simply fails on the truncated text.
    fn extract_file_purpose(&self, file_path: &Path) -> Option<String> {
        let content = read_first_lines(file_path, 20)?;
        let suffix = walk::py_suffix(file_path);

        if suffix == ".py" {
            if let Some(caps) = self.py_docstring.captures(&content) {
                let doc = crate::util::py_strip(caps.get(1).unwrap().as_str());
                return Some(first_line_truncated(&doc));
            }
            if let Some(caps) = self.py_docstring_single.captures(&content) {
                let doc = crate::util::py_strip(caps.get(1).unwrap().as_str());
                return Some(first_line_truncated(&doc));
            }
        }

        if [".js", ".ts", ".tsx", ".jsx"].contains(&suffix.as_str()) {
            if let Some(caps) = self.js_docblock.captures(&content) {
                let doc = crate::util::py_strip(caps.get(1).unwrap().as_str());
                // Strip a leading `*` from each line, then take the
                // first. `split('\n')` here, not splitlines.
                let stripped: Vec<String> = doc
                    .split('\n')
                    .map(|line| {
                        crate::util::py_strip(crate::util::py_strip(line).trim_start_matches('*'))
                            .to_string()
                    })
                    .collect();
                return Some(first_line_truncated(&stripped.join("\n")));
            }
        }

        None
    }

    /// Port of `create_folder_docs`. Every proposal has an EMPTY
    /// original, which is what trips the safety engine's doubling check
    /// and lands these patches at medium risk.
    pub fn create_folder_docs(&self) -> Vec<FixProposal> {
        self.find_directories_missing_folder_doc()
            .into_iter()
            .map(|directory| {
                let content = self.generate_folder_doc(&directory);
                FixProposal {
                    file_path: directory.join(FOLDER_DOC_NAME),
                    original_content: String::new(),
                    new_content: content,
                    fix_type: self.fix_type().to_string(),
                }
            })
            .collect()
    }
}

impl Fixer for DocsRegenerator {
    fn fix_type(&self) -> &'static str {
        "docs_regen"
    }

    /// Always false: this fixer creates new files rather than editing
    /// existing ones, so it is never driven through `process_directory`.
    fn can_fix(&self, _file_path: &Path) -> bool {
        false
    }

    fn fix(&self, _file_path: &Path, _content: &str) -> Option<String> {
        None
    }
}

/// `Path.iterdir()`, optionally sorted the way `sorted(...)` orders
/// `Path` objects on POSIX (by the full path string).
fn read_dir_sorted(directory: &Path, sorted: bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut items: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    if sorted {
        items.sort();
    }
    items
}

/// `"".join(f.readline() for _ in range(n))` with `errors="ignore"`.
fn read_first_lines(path: &Path, max_lines: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = pystr::translate_newlines(&pystr::decode_utf8_ignore(&bytes));
    let lines = pystr::first_lines(&text, max_lines);
    Some(lines)
}

/// `doc.split('\n')[0][:100]`.
fn first_line_truncated(doc: &str) -> String {
    let first = doc.split('\n').next().unwrap_or("");
    crate::util::py_slice(first, 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::tmpdir;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn identifies_directories_missing_folder_doc() {
        let tmp = tmpdir("docs-missing");
        write(&tmp, "src/module.py", "def func(): pass");
        let fixer = DocsRegenerator::new(&tmp);
        assert_eq!(
            fixer.find_directories_missing_folder_doc(),
            vec![tmp.join("src")]
        );
    }

    #[test]
    fn skips_directories_with_folder_doc() {
        let tmp = tmpdir("docs-present");
        write(&tmp, "src/module.py", "def func(): pass");
        write(&tmp, "src/FOLDER.md", "# Docs");
        let fixer = DocsRegenerator::new(&tmp);
        assert!(fixer.find_directories_missing_folder_doc().is_empty());
    }

    #[test]
    fn skips_directories_without_code_and_the_skip_list() {
        let tmp = tmpdir("docs-nocode");
        write(&tmp, "docs/guide.md", "# guide");
        write(&tmp, "node_modules/pkg/index.js", "1");
        write(&tmp, "target/debug/build.rs", "fn main() {}");
        write(&tmp, "vendor/lib/a.go", "package a");
        let fixer = DocsRegenerator::new(&tmp);
        assert!(fixer.find_directories_missing_folder_doc().is_empty());
    }

    #[test]
    fn code_detection_is_not_recursive() {
        // The check is on the directory's OWN files, so a parent whose
        // code lives only in a child is not flagged.
        let tmp = tmpdir("docs-nonrecursive");
        write(&tmp, "a/b/module.py", "x = 1");
        let fixer = DocsRegenerator::new(&tmp);
        assert_eq!(
            fixer.find_directories_missing_folder_doc(),
            vec![tmp.join("a/b")]
        );
    }

    #[test]
    fn generates_folder_doc() {
        let tmp = tmpdir("docs-generate");
        write(&tmp, "components/Button.tsx", "export function Button() {}");
        write(&tmp, "components/Input.tsx", "export function Input() {}");
        let fixer = DocsRegenerator::new(&tmp);
        let content = fixer.generate_folder_doc(&tmp.join("components"));
        assert_eq!(
            content,
            "# components\n\
             \n\
             **Location:** `components`\n\
             \n\
             ## Overview\n\
             \n\
             This directory contains 2 files and 0 subdirectories.\n\
             \n\
             ## Files\n\
             \n\
             ### .tsx\n\
             \n\
             - `Button.tsx`\n\
             - `Input.tsx`\n\
             \n\
             ---\n\
             \n\
             *This file was auto-generated by Aethyme Autofixer.*"
        );
    }

    #[test]
    fn groups_by_extension_sorted_and_lists_subdirectories() {
        let tmp = tmpdir("docs-groups");
        write(&tmp, "pkg/z.py", "");
        write(&tmp, "pkg/a.py", "");
        write(&tmp, "pkg/b.ts", "");
        write(&tmp, "pkg/Makefile", "");
        write(&tmp, "pkg/.hidden.py", "");
        write(&tmp, "pkg/sub2/x.py", "");
        write(&tmp, "pkg/sub1/x.py", "");
        let fixer = DocsRegenerator::new(&tmp);
        let content = fixer.generate_folder_doc(&tmp.join("pkg"));
        assert!(content.contains("This directory contains 4 files and 2 subdirectories."));
        // Extension headings sorted as strings: ".py" < ".ts" < "no extension".
        let order: Vec<&str> = content.lines().filter(|l| l.starts_with("### ")).collect();
        assert_eq!(order, vec!["### .py", "### .ts", "### no extension"]);
        assert!(content.contains("- `a.py`\n- `z.py`"));
        assert!(content.contains("## Subdirectories\n\n- `sub1/`\n- `sub2/`"));
        // Dotfiles are excluded from both the listing and the count.
        assert!(!content.contains(".hidden.py"));
    }

    #[test]
    fn extracts_python_docstrings_and_js_docblocks() {
        let tmp = tmpdir("docs-purpose");
        write(
            &tmp,
            "p/mod.py",
            "\"\"\"Module purpose here.\n\nMore.\n\"\"\"\nx = 1\n",
        );
        write(&tmp, "p/single.py", "'''Single quoted purpose.'''\nx = 1\n");
        write(
            &tmp,
            "p/comp.ts",
            "/**\n * The component purpose.\n * More.\n */\nexport const a = 1;\n",
        );
        write(&tmp, "p/plain.py", "x = 1\n");
        let fixer = DocsRegenerator::new(&tmp);
        let content = fixer.generate_folder_doc(&tmp.join("p"));
        assert!(
            content.contains("- `mod.py` - Module purpose here."),
            "{content}"
        );
        assert!(
            content.contains("- `single.py` - Single quoted purpose."),
            "{content}"
        );
        assert!(
            content.contains("- `comp.ts` - The component purpose."),
            "{content}"
        );
        assert!(content.contains("- `plain.py`\n"), "{content}");
    }

    #[test]
    fn purpose_is_truncated_at_a_hundred_characters() {
        let tmp = tmpdir("docs-truncate");
        let long = "L".repeat(150);
        write(&tmp, "p/mod.py", &format!("\"\"\"{long}\"\"\"\nx = 1\n"));
        let fixer = DocsRegenerator::new(&tmp);
        let content = fixer.generate_folder_doc(&tmp.join("p"));
        assert!(content.contains(&format!("- `mod.py` - {}", "L".repeat(100))));
        assert!(!content.contains(&"L".repeat(101)));
    }

    #[test]
    fn purpose_window_is_twenty_lines() {
        let tmp = tmpdir("docs-window");
        let padding = "# pad\n".repeat(20);
        write(
            &tmp,
            "p/late.py",
            &format!("{padding}\"\"\"Too late.\"\"\"\nx = 1\n"),
        );
        let fixer = DocsRegenerator::new(&tmp);
        let content = fixer.generate_folder_doc(&tmp.join("p"));
        assert!(content.contains("- `late.py`\n"), "{content}");
    }

    #[test]
    fn creates_folder_docs_as_empty_original_proposals() {
        let tmp = tmpdir("docs-create");
        write(&tmp, "utils/helpers.py", "def helper(): pass");
        let fixer = DocsRegenerator::new(&tmp);
        let created = fixer.create_folder_docs();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].fix_type, "docs_regen");
        assert_eq!(created[0].file_path, tmp.join("utils/FOLDER.md"));
        assert_eq!(created[0].original_content, "");
        assert!(created[0].new_content.starts_with("# utils\n"));
    }

    #[test]
    fn fixer_trait_surface_is_inert() {
        let tmp = tmpdir("docs-inert");
        let fixer = DocsRegenerator::new(&tmp);
        assert_eq!(fixer.fix_type(), "docs_regen");
        assert!(!fixer.can_fix(Path::new("anything.py")));
        assert_eq!(fixer.fix(Path::new("anything.py"), "x"), None);
    }
}
