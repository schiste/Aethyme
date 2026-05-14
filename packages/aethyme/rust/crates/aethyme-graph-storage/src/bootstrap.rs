//! Repo bootstrap: gitattributes template, engine-version pin,
//! one-shot `bootstrap_repo` to initialize the layout.
//!
//! A fresh repo onboarding Aethyme needs three setup actions:
//!
//! 1. Create `.aethyme/graph/` and `.aethyme/graph/_index/` so
//!    the indexer can start writing immediately.
//! 2. Write `.aethyme/engine-version` pinning the engine version
//!    that produced the current fragments. CI uses this to detect
//!    parser-version drift.
//! 3. Write `.aethyme/graph/.gitattributes` so git collapses the
//!    generated diffs in PRs (`linguist-generated=true`) and
//!    auto-unions index shards (`merge=union`).
//!
//! The `bootstrap_repo` helper does all three.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::layout::{engine_version_path, AETHYME_DIR, GRAPH_SUBDIR, INDEX_SUBDIR};

/// The exact `.gitattributes` content the storage layer expects to
/// live at `<repo>/.aethyme/graph/.gitattributes`. Three lines:
///
/// 1. `**/*.bin linguist-generated=true binary` — fragments are
///    binary blobs; mark as generated so GitHub collapses them in
///    PRs, and as binary so `git diff` doesn't try to render them.
/// 2. `_index/**/*.ndjson linguist-generated=true merge=union` —
///    index shards are line-based; mark as generated AND apply
///    merge=union so two PRs adding distinct symbols to the same
///    module auto-merge without conflict.
/// 3. Trailing newline so editors don't whine.
///
/// Per the design conversation, this content is the cross-process
/// contract for how git handles committed graph artifacts.
pub const GITATTRIBUTES_CONTENT: &str = "\
**/*.bin linguist-generated=true binary
_index/**/*.ndjson linguist-generated=true merge=union
";

/// Bootstrap a repo's Aethyme layout. Creates the directory tree,
/// writes `.gitattributes` into `.aethyme/graph/`, and pins the
/// engine version in `.aethyme/engine-version`.
///
/// Idempotent: running it twice with the same `engine_version`
/// produces the same files. Running it with a different
/// `engine_version` overwrites the pin (this is intentional —
/// upgrading the engine is a deliberate operation that pins the
/// new version).
pub fn bootstrap_repo(
    repo_root: &Path,
    engine_version: &str,
) -> Result<BootstrapPaths, BootstrapError> {
    if engine_version.is_empty() {
        return Err(BootstrapError::EmptyEngineVersion);
    }

    let graph_dir = graph_dir(repo_root);
    let index_dir = index_dir(repo_root);
    fs::create_dir_all(&index_dir).map_err(BootstrapError::Io)?;

    let gitattributes = graph_dir.join(".gitattributes");
    fs::write(&gitattributes, GITATTRIBUTES_CONTENT)
        .map_err(BootstrapError::Io)?;

    let version_file = engine_version_path(repo_root);
    fs::write(&version_file, format!("{engine_version}\n"))
        .map_err(BootstrapError::Io)?;

    Ok(BootstrapPaths {
        graph_dir,
        index_dir,
        gitattributes,
        engine_version_file: version_file,
    })
}

/// Read the pinned engine version. Trims the trailing newline that
/// `bootstrap_repo` writes.
pub fn read_engine_version(
    repo_root: &Path,
) -> Result<String, EngineVersionReadError> {
    let path = engine_version_path(repo_root);
    let contents = fs::read_to_string(&path)
        .map_err(EngineVersionReadError::Io)?;
    let trimmed = contents.trim_end_matches('\n').trim_end_matches('\r');
    if trimmed.is_empty() {
        return Err(EngineVersionReadError::Empty);
    }
    Ok(trimmed.to_owned())
}

fn graph_dir(repo_root: &Path) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    p.push(AETHYME_DIR);
    p.push(GRAPH_SUBDIR);
    p
}

fn index_dir(repo_root: &Path) -> PathBuf {
    let mut p = graph_dir(repo_root);
    p.push(INDEX_SUBDIR);
    p
}

/// Paths created by [`bootstrap_repo`], returned for the caller's
/// convenience (e.g., to log the layout that was just established).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPaths {
    pub graph_dir: PathBuf,
    pub index_dir: PathBuf,
    pub gitattributes: PathBuf,
    pub engine_version_file: PathBuf,
}

#[derive(Debug)]
pub enum BootstrapError {
    EmptyEngineVersion,
    Io(io::Error),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyEngineVersion => {
                f.write_str("bootstrap: engine_version must not be empty")
            }
            Self::Io(e) => write!(f, "bootstrap I/O: {e}"),
        }
    }
}

impl std::error::Error for BootstrapError {}

#[derive(Debug)]
pub enum EngineVersionReadError {
    Empty,
    Io(io::Error),
}

impl std::fmt::Display for EngineVersionReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => {
                f.write_str("engine-version file is empty after trimming")
            }
            Self::Io(e) => write!(f, "engine-version read I/O: {e}"),
        }
    }
}

impl std::error::Error for EngineVersionReadError {}
