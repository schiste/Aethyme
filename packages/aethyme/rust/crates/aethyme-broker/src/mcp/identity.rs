//! Content identities: source trees, lockfiles and single files.
//!
//! A source tree inside a Git checkout is identified by the index entries
//! under it (blob ids are content addresses) plus the content of every
//! changed or untracked, not ignored, file; outside Git (or for an ignored
//! directory) by every file's path and content, skipping `.git` and
//! `node_modules`. Both are capped by [`Limits`], so a `source` inside a large
//! outer repository fails fast instead of hashing it.
//!
//! Git identities are cached under `<host state>/mcp/identity-cache/` for
//! [`CACHE_TTL_MS`], keyed by the path, the checkout's HEAD, its index file's
//! size and mtime, and the source directory's mtime, all read without
//! spawning Git. Within the TTL an edit that changes none of those is not
//! seen; the TTL is short for that reason.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Caps for hashing a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_files: usize,
    pub max_bytes: u64,
}

pub const DEFAULT_LIMITS: Limits = Limits {
    max_files: 20_000,
    max_bytes: 256 * 1024 * 1024,
};

pub const CACHE_TTL_MS: u64 = 15_000;
pub const CACHE_DIRECTORY: &str = "identity-cache";

/// Lockfiles whose content is part of a key and of the copy choice.
pub const LOCKFILES: &[&str] = &[
    "pnpm-lock.yaml",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "Cargo.lock",
    "go.sum",
    "uv.lock",
    "poetry.lock",
];

/// Files larger than this are identified by size only.
const LARGE_FILE: u64 = 64 * 1024 * 1024;

fn too_large(path: &Path, limits: &Limits) -> String {
    format!(
        "{} holds more than {} files or {} MiB to hash; declare a narrower `source`",
        path.display(),
        limits.max_files,
        limits.max_bytes / (1024 * 1024)
    )
}

/// Content identity of a file or directory tree, uncached.
pub fn tree_identity(path: &Path, limits: &Limits) -> Result<String, String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.is_file() {
        if metadata.len() > limits.max_bytes {
            return Err(too_large(path, limits));
        }
        return Ok(format!("file-v1:{}", content_digest(path)));
    }
    if in_git(path)
        && let Ok(repository) = crate::GitRepo::discover(path)
    {
        return git_tree_identity(&repository, path, limits);
    }
    walk_tree_identity(path, limits)
}

fn in_git(path: &Path) -> bool {
    path.ancestors().any(|dir| dir.join(".git").exists())
}

#[derive(Serialize, Deserialize)]
struct CachedIdentity {
    marker: String,
    identity: String,
    at_ms: u64,
}

/// [`tree_identity`] through the cache; the flag says whether it was a hit.
pub fn tree_identity_cached(
    path: &Path,
    limits: &Limits,
    cache: Option<&Path>,
) -> Result<(String, bool), String> {
    let marker = cache.and_then(|_| git_marker(path));
    let file = match (cache, &marker) {
        (Some(cache), Some(_)) => {
            let digest = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
            Some(cache.join(format!("{}.json", &digest[..32])))
        }
        _ => None,
    };
    if let (Some(file), Some(marker)) = (&file, &marker)
        && let Some(cached) = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str::<CachedIdentity>(&text).ok())
        && cached.marker == *marker
        && super::telemetry::now_ms().saturating_sub(cached.at_ms) < CACHE_TTL_MS
    {
        return Ok((cached.identity, true));
    }
    let identity = tree_identity(path, limits)?;
    if let (Some(file), Some(marker)) = (file, marker) {
        crate::warn_unrecorded(
            "cache the MCP source identity",
            write_cache(
                &file,
                &CachedIdentity {
                    marker,
                    identity: identity.clone(),
                    at_ms: super::telemetry::now_ms(),
                },
            ),
        );
    }
    Ok((identity, false))
}

fn write_cache(file: &Path, cached: &CachedIdentity) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let directory = file
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(cached).map_err(std::io::Error::other)?)?;
    temporary.persist(file).map_err(|error| error.error)?;
    Ok(())
}

fn stat_marker(path: &Path) -> String {
    path.metadata()
        .map(|metadata| {
            format!(
                "{}:{}.{}",
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec()
            )
        })
        .unwrap_or_else(|_| "-".into())
}

/// HEAD, index stat and source directory stat, read from files: no Git.
fn git_marker(path: &Path) -> Option<String> {
    let root = path.ancestors().find(|dir| dir.join(".git").exists())?;
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let named = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        if named.is_absolute() {
            named
        } else {
            root.join(named)
        }
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let named = PathBuf::from(text.trim());
            if named.is_absolute() {
                named
            } else {
                git_dir.join(named)
            }
        }
        Err(_) => git_dir.clone(),
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let commit = match head.strip_prefix("ref: ") {
        Some(reference) => std::fs::read_to_string(common_dir.join(reference))
            .or_else(|_| std::fs::read_to_string(git_dir.join(reference)))
            .map(|oid| oid.trim().to_string())
            .unwrap_or_else(|_| format!("packed:{}", stat_marker(&common_dir.join("packed-refs")))),
        None => head.to_string(),
    };
    Some(format!(
        "{}|{commit}|{}|{}",
        root.display(),
        stat_marker(&git_dir.join("index")),
        stat_marker(path)
    ))
}

fn git_tree_identity(
    repository: &crate::GitRepo,
    path: &Path,
    limits: &Limits,
) -> Result<String, String> {
    let root = repository
        .root()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| format!("{} is outside {}", canonical.display(), root.display()))?;
    // A literal pathspec: no glob or magic in the source path widens it.
    let pathspec = if relative.as_os_str().is_empty() {
        ":(literal).".to_string()
    } else {
        format!(":(literal){}", relative.to_string_lossy())
    };
    let index = repository
        .index_entries_under(&pathspec)
        .map_err(|error| error.to_string())?;
    let index_entries = index
        .split(|byte| *byte == 0)
        .filter(|r| !r.is_empty())
        .count();
    if index_entries > limits.max_files {
        return Err(too_large(path, limits));
    }
    let status = repository
        .status_entries_under(&pathspec)
        .map_err(|error| error.to_string())?;
    if index.is_empty() && status.is_empty() {
        // Nothing tracked and nothing visible: an ignored directory (build
        // output, a temporary directory inside a checkout). Git says nothing
        // about its content, so hash the files.
        return walk_tree_identity(path, limits);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"git-v1\0");
    hasher.update(&index);
    hasher.update(b"\0status\0");
    let mut records = status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    let mut changed_files = 0usize;
    let mut changed_bytes = 0u64;
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (state, changed) = (&record[..2], &record[3..]);
        hasher.update(state);
        hasher.update(changed);
        hasher.update(b"\0");
        if matches!(state[0], b'R' | b'C')
            && let Some(origin) = records.next()
        {
            hasher.update(origin);
            hasher.update(b"\0");
        }
        let file = root.join(String::from_utf8_lossy(changed).as_ref());
        changed_files += 1;
        changed_bytes =
            changed_bytes.saturating_add(file.symlink_metadata().map(|m| m.len()).unwrap_or(0));
        if index_entries + changed_files > limits.max_files || changed_bytes > limits.max_bytes {
            return Err(too_large(path, limits));
        }
        hasher.update(content_digest(&file).as_bytes());
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// A file's content digest, a symlink's target, or a marker. Files over 64
/// MiB are identified by their size.
pub fn content_digest(path: &Path) -> String {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::read_link(path)
            .map(|target| format!("link:{}", target.to_string_lossy()))
            .unwrap_or_else(|_| "link:?".into()),
        Ok(metadata) if metadata.is_file() && metadata.len() > LARGE_FILE => {
            format!("large:{}", metadata.len())
        }
        Ok(metadata) if metadata.is_file() => {
            let mut hasher = Sha256::new();
            match std::fs::File::open(path) {
                Ok(mut file) => {
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        match file.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(read) => hasher.update(&buffer[..read]),
                            Err(_) => return "unreadable".into(),
                        }
                    }
                    format!("{:x}", hasher.finalize())
                }
                Err(_) => "unreadable".into(),
            }
        }
        Ok(_) => "dir".into(),
        Err(_) => "absent".into(),
    }
}

fn walk_tree_identity(root: &Path, limits: &Limits) -> Result<String, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(dir) = pending.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == ".git" || name == "node_modules" {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
            } else {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
                files.push(path);
            }
            if files.len() > limits.max_files || total > limits.max_bytes {
                return Err(too_large(root, limits));
            }
        }
    }
    files.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"walk-v1\0");
    for file in files {
        let relative = file.strip_prefix(root).unwrap_or(&file);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(content_digest(&file).as_bytes());
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Digest of every lockfile directly in `root`, or `None` when it has none.
pub fn lockfile_digest(root: &Path) -> Option<String> {
    let mut hasher = Sha256::new();
    let mut any = false;
    for name in LOCKFILES {
        let path = root.join(name);
        if path.is_file() {
            any = true;
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(content_digest(&path).as_bytes());
            hasher.update(b"\0");
        }
    }
    any.then(|| format!("{:x}", hasher.finalize()))
}
