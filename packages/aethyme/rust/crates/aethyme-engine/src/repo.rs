use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".security-logs",
    ".venv",
    "venv",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "target",
    "coverage",
    ".turbo",
    ".cache",
    // Agent / IDE tooling directories — not part of repository structure
    ".claude",
    ".codex",
    ".chau7",
    ".cursor",
    ".chunk-history",
    ".vscode",
    ".idea",
    ".zed",
    ".windsurf",
    ".aethyme",
];

const EXCLUDED_DIR_PREFIXES: &[&str] = &[".venv", "__pycache__"];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RepoFile {
    pub path: String,
    pub language: Option<String>,
    pub line_count: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoSnapshot {
    pub root: String,
    pub files: Vec<RepoFile>,
    pub languages: Vec<String>,
    pub top_level_dirs: Vec<String>,
    pub readme_path: Option<String>,
}

impl RepoSnapshot {
    pub fn repo_name(&self) -> String {
        Path::new(&self.root)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("repo")
            .to_string()
    }
}

pub fn discover_repo(root: &Path) -> Result<RepoSnapshot, String> {
    if !root.exists() {
        return Err(format!(
            "Repository path does not exist: {}",
            root.display()
        ));
    }
    if !root.is_dir() {
        return Err(format!(
            "Repository path is not a directory: {}",
            root.display()
        ));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|err| format!("Failed to canonicalize repo path: {err}"))?;

    let mut files = Vec::new();
    let mut languages = BTreeSet::new();
    let mut top_level_dirs = BTreeSet::new();
    let mut readme_path = None;

    walk_dir(
        &canonical_root,
        &canonical_root,
        &mut files,
        &mut languages,
        &mut top_level_dirs,
        &mut readme_path,
    )?;

    files.par_iter_mut().for_each(|file| {
        let path = canonical_root.join(&file.path);
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if should_count_lines(&path, file_name, file.size_bytes) {
            file.line_count = fs::read_to_string(&path)
                .map(|contents| contents.lines().count())
                .unwrap_or(0);
        }
    });

    files.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(RepoSnapshot {
        root: canonical_root.to_string_lossy().to_string(),
        files,
        languages: languages.into_iter().collect(),
        top_level_dirs: top_level_dirs.into_iter().collect(),
        readme_path,
    })
}

fn walk_dir(
    root: &Path,
    current: &Path,
    files: &mut Vec<RepoFile>,
    languages: &mut BTreeSet<String>,
    top_level_dirs: &mut BTreeSet<String>,
    readme_path: &mut Option<String>,
) -> Result<(), String> {
    let entries = fs::read_dir(current)
        .map_err(|err| format!("Failed to read directory {}: {err}", current.display()))?;

    for entry in entries {
        let entry = entry.map_err(|err| format!("Failed to inspect directory entry: {err}"))?;
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();

        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if metadata.file_type().is_symlink() && fs::metadata(&path).is_err() {
            continue;
        }

        if metadata.is_dir() {
            if should_exclude_dir(file_name) {
                continue;
            }
            if let Ok(relative) = path.strip_prefix(root) {
                if let Some(first) = relative.components().next() {
                    top_level_dirs.insert(first.as_os_str().to_string_lossy().to_string());
                }
            }
            walk_dir(root, &path, files, languages, top_level_dirs, readme_path)?;
            continue;
        }

        let relative_path = relative_string(root, &path)?;
        let size_bytes = if metadata.file_type().is_symlink() {
            fs::metadata(&path)
                .map_err(|err| format!("Failed to stat file {}: {err}", path.display()))?
                .len()
        } else {
            metadata.len()
        };
        let language = detect_language(&path);
        if let Some(language_name) = &language {
            languages.insert(language_name.clone());
        }

        if readme_path.is_none() {
            let lowercase = file_name.to_ascii_lowercase();
            if lowercase == "readme.md" || lowercase == "readme" {
                *readme_path = Some(relative_path.clone());
            }
        }

        files.push(RepoFile {
            path: relative_path,
            language,
            line_count: 0,
            size_bytes,
        });
    }

    Ok(())
}

fn detect_language(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "py" => Some("python".to_string()),
        "ts" | "tsx" => Some("typescript".to_string()),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript".to_string()),
        "rs" => Some("rust".to_string()),
        "php" => Some("php".to_string()),
        "go" => Some("go".to_string()),
        "java" => Some("java".to_string()),
        "kt" | "kts" => Some("kotlin".to_string()),
        "rb" => Some("ruby".to_string()),
        "cs" => Some("c_sharp".to_string()),
        "c" | "h" => Some("c".to_string()),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp".to_string()),
        "swift" => Some("swift".to_string()),
        "scala" => Some("scala".to_string()),
        "ex" | "exs" => Some("elixir".to_string()),
        _ => None,
    }
}

fn should_exclude_dir(file_name: &str) -> bool {
    if EXCLUDED_DIRS.contains(&file_name) {
        return true;
    }

    EXCLUDED_DIR_PREFIXES
        .iter()
        .any(|prefix| file_name.starts_with(prefix))
}

fn should_count_lines(path: &Path, file_name: &str, size_bytes: u64) -> bool {
    const MAX_LINE_COUNT_BYTES: u64 = 1_048_576;
    if size_bytes > MAX_LINE_COUNT_BYTES {
        return false;
    }

    let lower_name = file_name.to_ascii_lowercase();
    if lower_name == "readme" || lower_name == "license" || lower_name == "makefile" {
        return true;
    }

    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "py" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "rs"
            | "go"
            | "md"
            | "txt"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "ini"
            | "cfg"
            | "conf"
            | "sh"
            | "sql"
            | "gd"
            | "tscn"
    )
}

pub fn relative_string(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(|relative| normalize_path(relative.to_path_buf()))
        .map_err(|err| format!("Failed to relativize {}: {err}", path.display()))
}

fn normalize_path(path: PathBuf) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{discover_repo, should_count_lines, should_exclude_dir};

    #[cfg(unix)]
    #[test]
    fn discover_repo_skips_broken_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("aethyme_repo_broken_symlink_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("src/main.py"), "def run():\n    return True\n").expect("write source");
        symlink(Path::new("missing-target"), root.join("broken-link"))
            .expect("create broken symlink");

        let snapshot = discover_repo(&root).expect("discover repo with broken symlink");

        assert!(snapshot.files.iter().any(|file| file.path == "src/main.py"));
        assert!(!snapshot.files.iter().any(|file| file.path == "broken-link"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn excludes_common_generated_directory_prefixes() {
        assert!(should_exclude_dir(".security-logs"));
        assert!(should_exclude_dir(".venv"));
        assert!(should_exclude_dir(".venv_py313"));
        assert!(should_exclude_dir("__pycache__"));
        assert!(should_exclude_dir("__pycache___nested"));
        assert!(should_exclude_dir("coverage"));
        assert!(should_exclude_dir(".turbo"));
        assert!(!should_exclude_dir("src"));
        assert!(!should_exclude_dir("documentation"));
    }

    #[test]
    fn excludes_agent_and_ide_tooling_directories() {
        assert!(should_exclude_dir(".claude"));
        assert!(should_exclude_dir(".codex"));
        assert!(should_exclude_dir(".chau7"));
        assert!(should_exclude_dir(".cursor"));
        assert!(should_exclude_dir(".chunk-history"));
        assert!(should_exclude_dir(".vscode"));
        assert!(should_exclude_dir(".idea"));
        assert!(should_exclude_dir(".zed"));
        assert!(should_exclude_dir(".windsurf"));
        // Real code directories must not be excluded
        assert!(!should_exclude_dir("src"));
        assert!(!should_exclude_dir("packages"));
        assert!(!should_exclude_dir("docs"));
    }

    #[test]
    fn only_counts_lines_for_likely_text_files() {
        assert!(should_count_lines(Path::new("README.md"), "README.md", 64));
        assert!(should_count_lines(Path::new("src/main.py"), "main.py", 64));
        assert!(should_count_lines(
            Path::new("config/app.toml"),
            "app.toml",
            64
        ));
        assert!(!should_count_lines(Path::new("image.png"), "image.png", 64));
        assert!(!should_count_lines(Path::new("video.mp4"), "video.mp4", 64));
        assert!(!should_count_lines(
            Path::new("docs/big.md"),
            "big.md",
            2_000_000
        ));
    }
}
