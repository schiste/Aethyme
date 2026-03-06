use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
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
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepoFile {
    pub path: String,
    pub language: Option<String>,
    pub line_count: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub root: String,
    pub files: Vec<RepoFile>,
    pub languages: Vec<String>,
    pub top_level_dirs: Vec<String>,
    pub readme_path: Option<String>,
}

pub fn discover_repo(root: &Path) -> Result<RepoSnapshot, String> {
    if !root.exists() {
        return Err(format!("Repository path does not exist: {}", root.display()));
    }
    if !root.is_dir() {
        return Err(format!("Repository path is not a directory: {}", root.display()));
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

    let mut child_paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("Failed to inspect directory entry: {err}"))?;
        child_paths.push(entry.path());
    }
    child_paths.sort();

    for path in child_paths {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();

        if path.is_dir() {
            if EXCLUDED_DIRS.contains(&file_name) {
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
        let metadata = fs::metadata(&path)
            .map_err(|err| format!("Failed to stat file {}: {err}", path.display()))?;
        let contents = fs::read_to_string(&path).unwrap_or_default();
        let line_count = contents.lines().count();
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
            line_count,
            size_bytes: metadata.len(),
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
        "go" => Some("go".to_string()),
        _ => None,
    }
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
