//! Shared repository walk with the scorecard ignore rules.
//!
//! Replicates CPython 3.13+ `pathlib.Path.rglob("*")` semantics, which
//! every Python detector relied on for both file discovery and —
//! critically for byte parity — **finding order**:
//!
//! - breadth-first over directories (FIFO queue), starting at the root;
//!   for each dequeued directory, all its entries are yielded in
//!   `readdir` order, and subdirectories are enqueued in that same
//!   order (probed empirically against Python 3.14 on APFS — see the
//!   Phase 4 crate-skeleton commit);
//! - symlinks are yielded but never descended (`recurse_symlinks=False`
//!   is the pathlib default);
//! - hidden files/dirs ARE yielded (pathlib globs match dotfiles);
//! - unreadable directories are skipped silently (glob suppresses
//!   `OSError`).
//!
//! Rust's `read_dir` and Python's `os.scandir` are both thin `readdir`
//! wrappers, so on a given filesystem the per-directory order matches.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

/// One walked entry. Type flags follow symlinks (Python's
/// `Path.is_file()` / `Path.is_dir()` stat semantics); a broken symlink
/// reports neither.
pub struct WalkEntry {
    pub path: PathBuf,
    pub is_file: bool,
    pub is_dir: bool,
}

/// Equivalent of `repo_path.rglob('*')`: every entry under `root`
/// (files, dirs, symlinks), in Python's traversal order.
pub fn rglob_all(root: &Path) -> Vec<WalkEntry> {
    let mut out = Vec::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(root.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue; // glob suppresses OSError
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Descend decision: entry type WITHOUT following symlinks
            // (pathlib recurse_symlinks=False).
            let descend = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            // Reported type: WITH following symlinks (Path.is_file/is_dir).
            let (is_file, is_dir) = match fs::metadata(&path) {
                Ok(meta) => (meta.is_file(), meta.is_dir()),
                Err(_) => (false, false),
            };
            if descend {
                queue.push_back(path.clone());
            }
            out.push(WalkEntry {
                path,
                is_file,
                is_dir,
            });
        }
    }
    out
}

/// Equivalent of `glob('**/{name}')` used only for existence checks:
/// does any entry (file OR directory — pathlib name-matches both) named
/// `name` exist under `root`? No ignore rules — the Python call sites
/// applied none.
pub fn any_entry_named(root: &Path, name: &str) -> bool {
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(root.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy() == name {
                return true;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                queue.push_back(entry.path());
            }
        }
    }
    false
}

/// Directory names excluded by every detector (`BaseDetector`'s
/// `exclude_dirs`).
const EXCLUDE_DIRS: [&str; 13] = [
    "node_modules",
    "__pycache__",
    ".git",
    "venv",
    ".venv",
    "dist",
    "build",
    ".pytest_cache",
    ".mypy_cache",
    "site-packages",
    ".next",
    "coverage",
    ".tox",
];

/// Port of `BaseDetector.should_skip_file`. Operates on the FULL
/// (absolute) path's components, exactly like the Python
/// `file_path.parts` checks — including the quirk that a repository
/// living under a dotted directory (e.g. a `.claude/worktrees/...`
/// checkout) causes every path to be skipped. Parity first; V2 may
/// scope this to the repo-relative path.
pub fn should_skip_file(path: &Path) -> bool {
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        if EXCLUDE_DIRS.contains(&name.as_ref()) {
            return true;
        }
        if name.starts_with('.') && name != ".env.example" && name != ".env.template" {
            return true;
        }
    }
    false
}

/// Directory names the engine's `_count_files` excludes (same list as
/// `EXCLUDE_DIRS`, but WITHOUT the hidden-component rule — the Python
/// engine only intersected `parts` with the skip set).
pub fn count_files_skip(path: &Path) -> bool {
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        if EXCLUDE_DIRS.contains(&name.to_string_lossy().as_ref()) {
            return true;
        }
    }
    false
}

/// Port of `BaseDetector.read_file_safe`: strict UTF-8, `None` on any
/// read/decode failure, and — because Python's `read_text` opens in
/// text mode — universal-newline translation (`\r\n` → `\n`, lone
/// `\r` → `\n`), which shifts line numbers and evidence exactly like
/// CPython does.
pub fn read_file_safe(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(translate_newlines(&text))
}

fn translate_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Python `Path.suffix`: the final `.`-suffix of the last component,
/// empty for names that start with the only dot (`.gitignore`) or end
/// with a bare trailing dot (`foo.`). Rust's `Path::extension` differs
/// on the trailing-dot case, hence this port.
pub fn py_suffix(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return String::new();
    };
    let name = name.to_string_lossy();
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[i..].to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn mk(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn walk_yields_bfs_order_and_skips_symlink_descent() {
        let tmp = std::env::temp_dir().join(format!("aq-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        mk(&tmp, "a/c/cc.py", "");
        mk(&tmp, "b/bb.py", "");
        mk(&tmp, "top.py", "");
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.join("b"), tmp.join("blink")).unwrap();

        let entries = rglob_all(&tmp);
        let rels: Vec<String> = entries
            .iter()
            .map(|e| {
                e.path
                    .strip_prefix(&tmp)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        // Symlinked dir yielded but not descended.
        assert!(rels.contains(&"blink".to_string()));
        assert!(!rels.iter().any(|r| r.starts_with("blink/")));
        // BFS: all depth-1 entries precede depth-2, which precede depth-3.
        let depth =
            |r: &String| r.matches('/').count();
        let depths: Vec<usize> = rels.iter().map(depth).collect();
        let mut sorted = depths.clone();
        sorted.sort_unstable();
        // BFS over dirs isn't a strict depth sort in general, but in this
        // fixture (each dir discovered at depth d only from depth d-1) it is.
        assert_eq!(depths, sorted, "expected BFS ordering, got {rels:?}");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skip_rules_match_python() {
        assert!(should_skip_file(Path::new("/r/node_modules/x.py")));
        assert!(should_skip_file(Path::new("/r/.hidden/x.py")));
        assert!(should_skip_file(Path::new("/r/a/.git/x")));
        assert!(!should_skip_file(Path::new("/r/src/x.py")));
        assert!(!should_skip_file(Path::new("/r/.env.example")));
        // Full-path quirk: dotted ancestor outside the repo still skips.
        assert!(should_skip_file(Path::new("/home/u/.claude/wt/repo/src/x.py")));
        // Engine count variant has no hidden rule.
        assert!(!count_files_skip(Path::new("/home/u/.claude/wt/repo/src/x.py")));
        assert!(count_files_skip(Path::new("/r/dist/x.py")));
    }

    #[test]
    fn suffix_matches_python_path_suffix() {
        assert_eq!(py_suffix(Path::new("a/x.py")), ".py");
        assert_eq!(py_suffix(Path::new("a/x.tar.gz")), ".gz");
        assert_eq!(py_suffix(Path::new("a/.gitignore")), "");
        assert_eq!(py_suffix(Path::new("a/foo.")), "");
        assert_eq!(py_suffix(Path::new("a/foo")), "");
    }

    #[test]
    fn read_file_safe_translates_newlines_and_rejects_non_utf8() {
        let tmp = std::env::temp_dir().join(format!("aq-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("crlf.txt"), b"a\r\nb\rc\n").unwrap();
        assert_eq!(read_file_safe(&tmp.join("crlf.txt")).unwrap(), "a\nb\nc\n");
        fs::write(tmp.join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_file_safe(&tmp.join("bin.dat")).is_none());
        assert!(read_file_safe(&tmp.join("missing")).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }
}
