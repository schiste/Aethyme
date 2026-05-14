//! Filesystem-only indexer: walks a repo's source tree and produces
//! one `IndexedFile` per file with just the top-level node (`File`
//! or `NonCodeFile`). Language-specific indexers (commits 3.2+)
//! consume these and append symbol-level nodes + edges.
//!
//! The walker honors a default ignore list (`.git/`, `.aethyme/`,
//! `node_modules/`, `target/`, etc.) which can be extended via
//! [`WalkOptions`].

use std::path::Path;

use blake3::Hasher;
use walkdir::WalkDir;

use aethyme_graph_schema::{File, FileConstructionError, Node, NonCodeFile, NonCodeFileConstructionError};

use crate::context::IndexerContext;
use crate::language_map::{classify_file, FileClassification};

/// One file's contribution to the per-file fragment: the top-level
/// File or NonCodeFile node, ready to be the seed of a Fragment.
///
/// Language indexers (commits 3.2+) extend this with symbol-level
/// nodes + edges before the Fragment is finalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    /// Repo-relative path (e.g. "src/cli.py"). Same form as the
    /// `source_path` argument used by the storage crate.
    pub source_path: Box<str>,
    /// The top-level node for this file (File or NonCodeFile).
    pub top_node: Node,
    /// Detected language for code files; the format's canonical
    /// name for non-code files. Empty for ignored files (which
    /// are filtered out before reaching the result).
    pub language: Box<str>,
}

/// Options controlling the filesystem walk.
///
/// `extra_ignore_dirs` extends the default ignore list (relative
/// to the repo root, no leading slash, e.g. "build" or
/// "vendor/cache"). The defaults catch the universally-ignored
/// directories; consumers add project-specific entries as needed.
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    pub extra_ignore_dirs: Vec<String>,
    pub max_file_size_bytes: Option<u64>,
}

/// Default ignore list: directories that universally never
/// contain content we want to index. Kept lowercase for
/// case-insensitive matching on case-insensitive filesystems.
const DEFAULT_IGNORE_DIRS: &[&str] = &[
    ".git",
    ".aethyme",
    "node_modules",
    "target",
    ".venv",
    "venv",
    ".tox",
    "__pycache__",
    "dist",
    "build",
    ".cache",
    ".pnpm-store",
    ".pytest_cache",
    ".ruff_cache",
    ".mypy_cache",
    ".next",
    ".nuxt",
    ".idea",
    ".vscode",
    ".DS_Store",
];

/// Default max file size — files larger than this are skipped. 5 MiB.
/// Hand-edited code rarely exceeds this; generated files and binary
/// blobs do. Caller can override via [`WalkOptions::max_file_size_bytes`].
const DEFAULT_MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// Result of walking a source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemIndexResult {
    /// One `IndexedFile` per recognized source file.
    pub files: Vec<IndexedFile>,
    /// Files the walker encountered but skipped (size limit,
    /// extension not recognized, etc.). Caller can log these to
    /// audit coverage.
    pub skipped: Vec<SkippedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub source_path: Box<str>,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    UnrecognizedExtension,
    OverSizeLimit,
    ReadError,
}

/// Walk the repo's source tree and produce one `IndexedFile` per
/// recognized file.
pub fn walk_source_tree(
    ctx: &IndexerContext,
    options: &WalkOptions,
) -> Result<FilesystemIndexResult, FilesystemIndexerError> {
    let max_size =
        options.max_file_size_bytes.unwrap_or(DEFAULT_MAX_FILE_SIZE);
    let mut files = Vec::new();
    let mut skipped = Vec::new();

    let root = ctx.repo_root().to_path_buf();
    let walker = WalkDir::new(&root).follow_links(false).into_iter();
    let filtered = walker.filter_entry(|e| {
        if e.path() == root {
            return true;
        }
        !is_ignored_dir(e.path(), &root, &options.extra_ignore_dirs)
    });

    for entry in filtered {
        let entry = entry.map_err(|e| FilesystemIndexerError::Walk {
            message: e.to_string(),
        })?;
        if !entry.file_type().is_file() {
            continue;
        }

        let Some(source_path) = relative_path(entry.path(), &root) else {
            continue;
        };

        let file_name = entry
            .file_name()
            .to_string_lossy()
            .into_owned();
        match classify_file(&file_name) {
            FileClassification::Ignored => {
                skipped.push(SkippedFile {
                    source_path: source_path.into(),
                    reason: SkipReason::UnrecognizedExtension,
                });
                continue;
            }
            FileClassification::Code { language } => {
                let read = read_metadata(entry.path(), max_size);
                match read {
                    Ok(Some(meta)) => {
                        let node = File::new(
                            ctx.repo_name(),
                            &source_path,
                            language,
                            meta.byte_size,
                            &meta.content_hash,
                        )
                        .map_err(FilesystemIndexerError::FileNode)?;
                        files.push(IndexedFile {
                            source_path: source_path.into(),
                            top_node: Node::File(node),
                            language: language.into(),
                        });
                    }
                    Ok(None) => skipped.push(SkippedFile {
                        source_path: source_path.into(),
                        reason: SkipReason::OverSizeLimit,
                    }),
                    Err(_) => skipped.push(SkippedFile {
                        source_path: source_path.into(),
                        reason: SkipReason::ReadError,
                    }),
                }
            }
            FileClassification::NonCode { format } => {
                let lang_label = non_code_format_label(&format);
                let node = NonCodeFile::new(
                    ctx.repo_name(),
                    &source_path,
                    format,
                )
                .map_err(FilesystemIndexerError::NonCodeNode)?;
                files.push(IndexedFile {
                    source_path: source_path.into(),
                    top_node: Node::NonCodeFile(node),
                    language: lang_label.into(),
                });
            }
        }
    }

    // Canonical order for deterministic output.
    files.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    skipped.sort_by(|a, b| a.source_path.cmp(&b.source_path));

    Ok(FilesystemIndexResult { files, skipped })
}

fn is_ignored_dir(
    path: &Path,
    root: &Path,
    extra: &[String],
) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if DEFAULT_IGNORE_DIRS.iter().any(|ig| ig.eq_ignore_ascii_case(name)) {
        return true;
    }
    if let Ok(rel) = path.strip_prefix(root) {
        let rel_str = rel.to_string_lossy();
        if extra.iter().any(|ig| ig.as_str() == rel_str) {
            return true;
        }
    }
    false
}

fn relative_path(p: &Path, root: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    // Normalize to forward slashes for cross-platform stability.
    // Schema NodeIds use the path verbatim, so / is the canonical
    // separator on disk too.
    let mut out = String::with_capacity(rel.as_os_str().len());
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    Some(out)
}

struct FileMeta {
    byte_size: u64,
    content_hash: String,
}

fn read_metadata(
    path: &Path,
    max_size: u64,
) -> std::io::Result<Option<FileMeta>> {
    let metadata = std::fs::metadata(path)?;
    let byte_size = metadata.len();
    if byte_size > max_size {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    let hash = hasher.finalize();
    // 32 hex chars = 16 bytes = 128 bits. Enough for collision
    // resistance in the indexer's contexts (content-duplicate
    // detection across paths).
    let content_hash = hex_encode_first_n(hash.as_bytes(), 16);
    Ok(Some(FileMeta {
        byte_size,
        content_hash,
    }))
}

fn hex_encode_first_n(bytes: &[u8], n: usize) -> String {
    let mut out = String::with_capacity(n * 2);
    for b in bytes.iter().take(n) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn non_code_format_label(format: &aethyme_graph_schema::NonCodeFormat) -> &'static str {
    use aethyme_graph_schema::NonCodeFormat::*;
    match format {
        Markdown => "markdown",
        Yaml => "yaml",
        Json => "json",
        Toml => "toml",
        Plain => "plain",
        Other(_) => "other",
    }
}

#[derive(Debug)]
pub enum FilesystemIndexerError {
    Walk { message: String },
    FileNode(FileConstructionError),
    NonCodeNode(NonCodeFileConstructionError),
}

impl std::fmt::Display for FilesystemIndexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Walk { message } => {
                write!(f, "filesystem walk failed: {message}")
            }
            Self::FileNode(e) => write!(f, "File node construction: {e}"),
            Self::NonCodeNode(e) => {
                write!(f, "NonCodeFile node construction: {e}")
            }
        }
    }
}

impl std::error::Error for FilesystemIndexerError {}
