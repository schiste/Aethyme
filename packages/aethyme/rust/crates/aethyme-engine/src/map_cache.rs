use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::map::RepositoryMap;

const MAP_CACHE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-map-v2-treesitter");

/// Skip the map cache (both load and save) when its serialized size would
/// exceed this threshold. bincode deserialization is O(size) and dominated by
/// allocating + populating the nested struct tree; on MediaWiki the cache
/// reached 8.2 GB and took 118-130 seconds to load — slower than rebuilding
/// from scratch (~71s with parse_store.redb warm). For repos under the
/// threshold the cache stays a real win (sub-second loads vs multi-second
/// rebuilds on small repos).
///
/// Override with `AETHYME_MAP_CACHE_MAX_MB`; set to 0 to disable the cache
/// entirely.
const DEFAULT_MAP_CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB

fn map_cache_max_bytes() -> u64 {
    if let Ok(v) = std::env::var("AETHYME_MAP_CACHE_MAX_MB") {
        if let Ok(mb) = v.trim().parse::<u64>() {
            return mb.saturating_mul(1024 * 1024);
        }
    }
    DEFAULT_MAP_CACHE_MAX_BYTES
}

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

/// Lightweight filesystem scan — collects only paths and sizes (no file reads).
/// Used to compute a fingerprint for cache validation without the cost of
/// `discover_repo()` which reads files to count lines.
fn quick_scan(root: &Path) -> Vec<(String, u64)> {
    let mut entries = Vec::new();
    quick_walk(root, root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn quick_walk(root: &Path, current: &Path, entries: &mut Vec<(String, u64)>) {
    let Ok(dir_entries) = fs::read_dir(current) else {
        return;
    };
    for entry in dir_entries.flatten() {
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() && fs::metadata(&path).is_err() {
            continue;
        }

        if meta.is_dir() {
            if EXCLUDED_DIRS.contains(&file_name)
                || file_name.starts_with(".venv")
                || file_name.starts_with("__pycache__")
            {
                continue;
            }
            quick_walk(root, &path, entries);
            continue;
        }

        if let Ok(rel) = path.strip_prefix(root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let size = if meta.file_type().is_symlink() {
                fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            } else {
                meta.len()
            };
            entries.push((rel_str, size));
        }
    }
}

/// Compute a fingerprint from file paths and sizes only.
///
/// Deliberately excludes the repo's absolute path so the fingerprint is
/// portable — a clone at a different location reuses the same cache.
fn scan_fingerprint(scan: &[(String, u64)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MAP_CACHE_VERSION.as_bytes());
    hasher.update(b"|");
    for (path, size) in scan {
        hasher.update(path.as_bytes());
        hasher.update(b":");
        hasher.update(size.to_le_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

fn cache_dir(root: &Path) -> std::path::PathBuf {
    root.join(".aethyme").join("cache")
}

fn cache_path(root: &Path, fingerprint: &str) -> std::path::PathBuf {
    cache_dir(root).join(format!("map-{}.bin", &fingerprint[..16]))
}

/// Try to load a cached `RepositoryMap` without running `discover_repo()`.
///
/// Performs a lightweight filesystem scan (paths + sizes only, no file reads)
/// to compute a fingerprint. If a valid cache exists AND is below the
/// `AETHYME_MAP_CACHE_MAX_MB` threshold, deserializes and returns the full
/// map. Oversized caches are skipped (and removed) — the rebuild path is
/// faster than deserializing them at MediaWiki scale.
pub fn try_load_cached_map(root: &Path) -> Option<RepositoryMap> {
    let max_bytes = map_cache_max_bytes();
    if max_bytes == 0 {
        return None;
    }
    let canonical = root.canonicalize().ok()?;
    let scan = quick_scan(&canonical);
    let fp = scan_fingerprint(&scan);
    let path = cache_path(&canonical, &fp);

    // Stat first so we can refuse oversized caches without paying the read.
    let metadata = fs::metadata(&path).ok()?;
    let size = metadata.len();
    if size > max_bytes {
        eprintln!(
            "aethyme: map cache at {} is {:.1} GB (over {:.1} GB limit) — \
             skipping load and removing; rebuild will be faster",
            path.display(),
            size as f64 / (1024.0 * 1024.0 * 1024.0),
            max_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        );
        let _ = fs::remove_file(&path);
        return None;
    }

    let data = fs::read(&path).ok()?;
    let map: RepositoryMap = bincode::deserialize(&data).ok()?;
    // Sanity check: file count should match
    if map.snapshot.files.len() != scan.len() {
        return None;
    }
    Some(map)
}

/// Save a built `RepositoryMap` to the cache.
///
/// Refuses to write caches that exceed `AETHYME_MAP_CACHE_MAX_MB` — at the
/// scale where loading would be slower than rebuilding, writing is also
/// wasted I/O.
pub fn save_cached_map(root: &Path, map: &RepositoryMap) {
    let max_bytes = map_cache_max_bytes();
    if max_bytes == 0 {
        return;
    }
    let canonical = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };
    let scan = quick_scan(&canonical);
    let fp = scan_fingerprint(&scan);
    let path = cache_path(&canonical, &fp);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = bincode::serialize(map) {
        if data.len() as u64 > max_bytes {
            eprintln!(
                "aethyme: serialized map is {:.1} GB (over {:.1} GB cache \
                 limit) — not writing cache; subsequent calls will rebuild",
                data.len() as f64 / (1024.0 * 1024.0 * 1024.0),
                max_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            );
            // Still clean up any stale cache files so we don't leave a stale
            // smaller cache that would be loaded next time.
            cleanup_stale_caches(&canonical, &path);
            return;
        }
        let _ = fs::write(&path, data);
    }
    cleanup_stale_caches(&canonical, &path);
}

fn cleanup_stale_caches(canonical: &Path, keep_path: &Path) {
    if let Ok(entries) = fs::read_dir(cache_dir(canonical)) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.extension().and_then(|e| e.to_str()) == Some("bin")
                && entry_path != keep_path
            {
                let _ = fs::remove_file(&entry_path);
            }
        }
    }
}
