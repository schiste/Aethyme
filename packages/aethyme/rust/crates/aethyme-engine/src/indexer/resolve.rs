use std::collections::HashMap;

pub fn resolve(
    language: &str,
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    source_file: &str,
    raw_target: &str,
) -> Option<String> {
    match language {
        "python" => resolve_python(files_by_path, files_by_last_segment, raw_target),
        "typescript" | "javascript" => {
            resolve_js(files_by_path, files_by_last_segment, source_file, raw_target)
        }
        "rust" => resolve_rust(files_by_path, files_by_last_segment, source_file, raw_target),
        "php" => resolve_php(files_by_path, files_by_last_segment, raw_target),
        "go" => resolve_go(files_by_path, files_by_last_segment, source_file, raw_target),
        "java" | "kotlin" | "c_sharp" => {
            resolve_java_family(files_by_path, files_by_last_segment, raw_target)
        }
        _ => resolve_heuristic(files_by_last_segment, raw_target),
    }
}

// --- Python: dotted paths → file paths ---

fn resolve_python(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    raw_target: &str,
) -> Option<String> {
    let dotted_path = raw_target.replace('.', "/");
    find_matching_file_id(files_by_path, &format!("{dotted_path}.py"))
        .or_else(|| find_matching_file_id(files_by_path, &format!("{dotted_path}/__init__.py")))
        .or_else(|| find_last_segment_file(files_by_last_segment, raw_target))
}

// --- JavaScript/TypeScript: relative paths with extension probing ---

fn resolve_js(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    source_file: &str,
    raw_target: &str,
) -> Option<String> {
    if raw_target.starts_with('.') {
        let source_dir = source_file.rsplit_once('/').map(|v| v.0).unwrap_or("");
        let candidate = normalize_relative_path(source_dir, raw_target);
        return find_matching_file_id(files_by_path, &candidate)
            .or_else(|| find_matching_file_id(files_by_path, &format!("{candidate}.ts")))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{candidate}.tsx")))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{candidate}.js")))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{candidate}.jsx")))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{candidate}/index.ts")))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{candidate}/index.js")));
    }
    find_last_segment_file(files_by_last_segment, raw_target)
}

// --- Rust: crate::/super::/self:: module paths ---

fn resolve_rust(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    source_file: &str,
    raw_target: &str,
) -> Option<String> {
    let source_dir = source_file.rsplit_once('/').map(|v| v.0).unwrap_or("");
    let target = raw_target.trim_end_matches(';').trim();
    let last_segment = target.split("::").last().unwrap_or(target);

    if target.starts_with("super::") {
        let parent_dir = source_dir.rsplit_once('/').map(|v| v.0).unwrap_or("");
        let normalized = if parent_dir.is_empty() {
            last_segment.to_string()
        } else {
            format!("{parent_dir}/{last_segment}")
        };
        return find_matching_file_id(files_by_path, &format!("{normalized}.rs"))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{normalized}/mod.rs")));
    }

    if target.starts_with("self::") {
        let normalized = if source_dir.is_empty() {
            last_segment.to_string()
        } else {
            format!("{source_dir}/{last_segment}")
        };
        return find_matching_file_id(files_by_path, &format!("{normalized}.rs"))
            .or_else(|| find_matching_file_id(files_by_path, &format!("{normalized}/mod.rs")));
    }

    if target.starts_with("crate::") {
        let relative = target.trim_start_matches("crate::").replace("::", "/");
        return find_matching_file_id(files_by_path, &format!("src/{relative}.rs"))
            .or_else(|| find_matching_file_id(files_by_path, &format!("src/{relative}/mod.rs")))
            .or_else(|| find_last_segment_file(files_by_last_segment, last_segment));
    }

    find_matching_file_id(files_by_path, &format!("{source_dir}/{target}.rs"))
        .or_else(|| find_matching_file_id(files_by_path, &format!("{source_dir}/{target}/mod.rs")))
        .or_else(|| find_last_segment_file(files_by_last_segment, last_segment))
}

// --- PHP: PSR-4 namespace → directory mapping ---

fn resolve_php(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    raw_target: &str,
) -> Option<String> {
    // Namespace paths use backslash: Foo\Bar\Baz → Foo/Bar/Baz.php
    let path = raw_target.replace('\\', "/");
    find_matching_file_id(files_by_path, &format!("{path}.php"))
        .or_else(|| find_matching_file_id(files_by_path, &format!("src/{path}.php")))
        .or_else(|| find_matching_file_id(files_by_path, &format!("includes/{path}.php")))
        .or_else(|| {
            // Try just the last segment (class name)
            let last = path.rsplit('/').next().unwrap_or(&path);
            find_last_segment_file(files_by_last_segment, last)
        })
}

// --- Go: package directory matching ---

fn resolve_go(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    source_file: &str,
    raw_target: &str,
) -> Option<String> {
    // Go imports are package paths; try to find a directory matching the last segment
    let pkg_name = raw_target.rsplit('/').next().unwrap_or(raw_target);
    let source_dir = source_file.rsplit_once('/').map(|v| v.0).unwrap_or("");

    // Try relative to source
    find_matching_file_id(files_by_path, &format!("{source_dir}/{pkg_name}/{pkg_name}.go"))
        .or_else(|| find_last_segment_file(files_by_last_segment, pkg_name))
}

// --- Java/Kotlin/C#: dotted package → directory ---

fn resolve_java_family(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    raw_target: &str,
) -> Option<String> {
    // com.foo.Bar → com/foo/Bar.java (or .kt, .cs)
    let path = raw_target.replace('.', "/");
    find_matching_file_id(files_by_path, &format!("{path}.java"))
        .or_else(|| find_matching_file_id(files_by_path, &format!("{path}.kt")))
        .or_else(|| find_matching_file_id(files_by_path, &format!("{path}.cs")))
        .or_else(|| find_matching_file_id(files_by_path, &format!("src/{path}.java")))
        .or_else(|| find_matching_file_id(files_by_path, &format!("src/main/java/{path}.java")))
        .or_else(|| {
            let last = raw_target.rsplit('.').next().unwrap_or(raw_target);
            find_last_segment_file(files_by_last_segment, last)
        })
}

// --- Heuristic: last-segment filename matching ---

fn resolve_heuristic(
    files_by_last_segment: &HashMap<String, Vec<String>>,
    raw_target: &str,
) -> Option<String> {
    find_last_segment_file(files_by_last_segment, raw_target)
}

// --- Shared helpers ---

pub fn normalize_relative_path(source_dir: &str, raw_target: &str) -> String {
    let mut parts: Vec<&str> = if source_dir.is_empty() {
        Vec::new()
    } else {
        source_dir.split('/').collect()
    };
    for segment in raw_target.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                let _ = parts.pop();
            }
            value => parts.push(value),
        }
    }
    parts.join("/")
}

pub fn find_matching_file_id(
    files_by_path: &HashMap<String, String>,
    candidate: &str,
) -> Option<String> {
    files_by_path.get(candidate).cloned()
}

pub fn find_last_segment_file(
    files_by_last_segment: &HashMap<String, Vec<String>>,
    raw_target: &str,
) -> Option<String> {
    let last = last_target_segment(raw_target);
    files_by_last_segment
        .get(&last)
        .and_then(|matches| matches.first().cloned())
}

pub fn last_target_segment(raw_target: &str) -> String {
    raw_target
        .rsplit('/')
        .next()
        .unwrap_or(raw_target)
        .rsplit('.')
        .next()
        .unwrap_or(raw_target)
        .rsplit("::")
        .next()
        .unwrap_or(raw_target)
        .to_string()
}
