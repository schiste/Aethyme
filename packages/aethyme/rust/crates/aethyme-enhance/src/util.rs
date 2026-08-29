//! Small Python-semantics shims shared across the enhance port.

use std::path::{Path, PathBuf};

/// `str.splitlines()` for the newline conventions that occur in real
/// repository files: `\n`, `\r\n`, `\r`. (Python also splits on exotic
/// separators like `\x0b`/`\x1c`/` `; those never appear in the
/// manifests this pipeline reads — accepted simplification.)
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                lines.push(&text[start..i]);
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `Path(p).expanduser().resolve()`: canonicalize (symlinks and all) with
/// a graceful fallback to the input when the path does not exist —
/// Python's `resolve(strict=False)` never raises.
pub fn resolve_path(path: &Path) -> PathBuf {
    let expanded = expanduser(path);
    expanded.canonicalize().unwrap_or(expanded)
}

fn expanduser(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if text == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(py_splitlines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(py_splitlines("a\r\nb\rc"), vec!["a", "b", "c"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("x"), vec!["x"]);
    }
}
