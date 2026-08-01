//! CPython string/path semantics the autofix port depends on.
//!
//! The fix side compares character counts, splits lines on the full
//! Unicode boundary set, and renders paths through `PurePosixPath`.
//! Rust's stdlib differs on every one of those, so the primitives live
//! here with the divergence documented at the definition.

use std::path::{Component, Path, PathBuf};

/// CPython `str.splitlines()` boundaries. Beyond `\n` / `\r` / `\r\n`
/// this includes the vertical tab, form feed, the three information
/// separators, NEL, LINE SEPARATOR, and PARAGRAPH SEPARATOR — Rust's
/// `str::lines` splits on `\n` alone, which would miscount every file
/// containing one of these.
fn is_line_boundary(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{001c}'
            | '\u{001d}'
            | '\u{001e}'
            | '\u{0085}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// CPython `str.splitlines(keepends=...)`. A trailing boundary does NOT
/// produce a final empty element (`"a\n".splitlines() == ["a"]`), and an
/// empty string yields no lines at all.
pub fn splitlines(text: &str, keepends: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if !is_line_boundary(c) {
            i += c.len_utf8();
            continue;
        }
        let content_end = i;
        let mut end = i + c.len_utf8();
        if c == '\r' && end < text.len() && bytes[end] == b'\n' {
            end += 1;
        }
        out.push(if keepends {
            &text[start..end]
        } else {
            &text[start..content_end]
        });
        i = end;
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `"".join(handle.readline() for _ in range(n))` over already
/// newline-translated text. `readline` splits on `\n` ONLY — the exotic
/// `splitlines` boundaries are not line terminators for the file
/// protocol, so this deliberately differs from `splitlines`.
pub fn first_lines(text: &str, max_lines: usize) -> String {
    let mut end = 0usize;
    let mut lines = 0usize;
    for (idx, byte) in text.as_bytes().iter().enumerate() {
        if *byte == b'\n' {
            lines += 1;
            end = idx + 1;
            if lines == max_lines {
                return text[..end].to_string();
            }
        }
    }
    let _ = end;
    text.to_string()
}

/// CPython `len(str)`: a count of code points, not bytes. Used for the
/// safety engine's size comparisons and the patch summary's
/// `size_change`, both of which are character deltas in the Python.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// CPython `str.replace(old, new, 1)`: replace the FIRST occurrence
/// only, returning the string unchanged when `old` is absent. The
/// fixers rely on this running against the progressively-rewritten
/// buffer, so repeated identical elements each consume the next
/// remaining occurrence.
pub fn replace_first(haystack: &str, old: &str, new: &str) -> String {
    match haystack.find(old) {
        Some(idx) => {
            let mut out = String::with_capacity(haystack.len() + new.len());
            out.push_str(&haystack[..idx]);
            out.push_str(new);
            out.push_str(&haystack[idx + old.len()..]);
            out
        }
        None => haystack.to_string(),
    }
}

/// CPython `str.replace(old, new)`: replace EVERY occurrence.
pub fn replace_all(haystack: &str, old: &str, new: &str) -> String {
    if old.is_empty() {
        return haystack.to_string();
    }
    haystack.replace(old, new)
}

/// `PurePosixPath.as_posix()`: components rejoined with `/`, a leading
/// `/` for absolute paths, `.` for the empty path. Python's `Path`
/// normalizes away `.` segments and duplicate separators at
/// construction; `Path::to_string_lossy` does not.
pub fn as_posix(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut absolute = false;
    for component in path.components() {
        match component {
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(name) => parts.push(name.to_string_lossy().to_string()),
            Component::Prefix(p) => parts.push(p.as_os_str().to_string_lossy().to_string()),
        }
    }
    if parts.is_empty() {
        return if absolute { "/".to_string() } else { ".".to_string() };
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `posixpath.relpath(path, start)`: a purely LEXICAL relative path —
/// no symlink resolution, no filesystem access. Relative inputs are
/// anchored to the current directory first, exactly like `abspath`.
pub fn relpath(path: &Path, start: &Path) -> PathBuf {
    let path_list = abs_components(path);
    let start_list = abs_components(start);
    let common = path_list
        .iter()
        .zip(start_list.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut rel: Vec<String> = vec!["..".to_string(); start_list.len() - common];
    rel.extend_from_slice(&path_list[common..]);
    if rel.is_empty() {
        return PathBuf::from(".");
    }
    PathBuf::from(rel.join("/"))
}

/// `abspath` + split into non-empty components. `..` is collapsed
/// lexically (`normpath`), never resolved through the filesystem.
fn abs_components(path: &Path) -> Vec<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out: Vec<String> = Vec::new();
    for part in absolute.to_string_lossy().split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_string()),
        }
    }
    out
}

/// `Path.parts` minus the root element: the named components only. The
/// Python membership tests (`"node_modules" in path.parts`) can never
/// match the root, so dropping it is behavior-preserving.
pub fn named_parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().to_string()),
            Component::ParentDir => Some("..".to_string()),
            _ => None,
        })
        .collect()
}

/// `Path.name`.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `Path.stem`: the file name with its `Path.suffix` removed.
pub fn py_stem(path: &Path) -> String {
    let name = file_name(path);
    let suffix = crate::walk::py_suffix(path);
    if suffix.is_empty() {
        name
    } else {
        name[..name.len() - suffix.len()].to_string()
    }
}

/// `bytes.decode("utf-8", errors="ignore")`: invalid sequences are
/// DROPPED, not replaced. `String::from_utf8_lossy` would insert
/// U+FFFD, changing both the content and every downstream length.
pub fn decode_utf8_ignore(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                out.push_str(valid);
                return out;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                out.push_str(unsafe { std::str::from_utf8_unchecked(&rest[..valid_up_to]) });
                let skip = err.error_len().unwrap_or(rest.len() - valid_up_to);
                rest = &rest[valid_up_to + skip..];
            }
        }
    }
}

/// Text-mode universal-newline translation (`\r\n` and lone `\r` become
/// `\n`), applied by CPython's `open(..., encoding=...)` on read.
pub fn translate_newlines(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_cpython_boundaries() {
        assert_eq!(splitlines("a\nb\nc", false), vec!["a", "b", "c"]);
        assert_eq!(splitlines("a\nb\n", false), vec!["a", "b"]);
        assert_eq!(splitlines("", false), Vec::<&str>::new());
        assert_eq!(splitlines("\n", false), vec![""]);
        assert_eq!(splitlines("a\r\nb", false), vec!["a", "b"]);
        assert_eq!(splitlines("a\rb", false), vec!["a", "b"]);
        // The boundaries Rust's str::lines would miss entirely.
        assert_eq!(splitlines("a\u{000b}b\u{2028}c\u{0085}d", false).len(), 4);
        assert_eq!(splitlines("a\r\nb\n", true), vec!["a\r\n", "b\n"]);
        assert_eq!(splitlines("a\nb", true), vec!["a\n", "b"]);
    }

    #[test]
    fn char_len_counts_code_points() {
        assert_eq!(char_len("héllo"), 5);
        assert_eq!(char_len("日本語"), 3);
        assert_ne!(char_len("日本語"), "日本語".len());
    }

    #[test]
    fn replace_first_walks_remaining_occurrences() {
        let s = "<b/><b/>";
        let once = replace_first(s, "<b/>", "<b x/>");
        assert_eq!(once, "<b x/><b/>");
        assert_eq!(replace_first(&once, "<b/>", "<b y/>"), "<b x/><b y/>");
        assert_eq!(replace_first("abc", "zz", "y"), "abc");
    }

    #[test]
    fn as_posix_normalizes_like_pathlib() {
        assert_eq!(as_posix(Path::new("/a/b")), "/a/b");
        assert_eq!(as_posix(Path::new("a//b")), "a/b");
        assert_eq!(as_posix(Path::new("./a")), "a");
        assert_eq!(as_posix(Path::new("")), ".");
        assert_eq!(as_posix(Path::new("/")), "/");
    }

    #[test]
    fn stem_matches_pathlib() {
        assert_eq!(py_stem(Path::new("a/LoginForm.tsx")), "LoginForm");
        assert_eq!(py_stem(Path::new("a/.gitignore")), ".gitignore");
        assert_eq!(py_stem(Path::new("a/x.tar.gz")), "x.tar");
    }

    #[test]
    fn decode_ignore_drops_invalid_bytes() {
        assert_eq!(decode_utf8_ignore(b"ab\xffcd"), "abcd");
        assert_eq!(decode_utf8_ignore(&[0xff, 0xfe]), "");
        assert_eq!(decode_utf8_ignore("é".as_bytes()), "é");
    }
}
