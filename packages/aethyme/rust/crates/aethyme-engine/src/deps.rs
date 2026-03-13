use std::fs;
use std::path::Path;

use crate::config::ConfigNode;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExternalDep {
    pub name: String,
    pub dep_type: String,
    pub source_config: String,
}

pub fn extract_external_deps(root: &Path, configs: &[ConfigNode]) -> Vec<ExternalDep> {
    let mut deps = Vec::new();
    for config in configs {
        let contents = fs::read_to_string(root.join(&config.path)).unwrap_or_default();
        match config.config_type.as_str() {
            "cargo" => deps.extend(extract_cargo_deps(&contents, &config.path)),
            "npm" => deps.extend(extract_npm_deps(&contents, &config.path)),
            "python" => deps.extend(extract_python_deps(&contents, &config.path)),
            _ => {}
        }
    }
    deps.sort();
    deps.dedup();
    deps
}

fn extract_cargo_deps(contents: &str, source: &str) -> Vec<ExternalDep> {
    let mut deps = Vec::new();
    let mut in_deps_section = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps_section = trimmed == "[dependencies]"
                || trimmed == "[dev-dependencies]"
                || trimmed == "[build-dependencies]";
            continue;
        }
        if in_deps_section {
            if let Some((key, _)) = trimmed.split_once('=') {
                let name = key.trim();
                if !name.is_empty() && !name.starts_with('#') {
                    deps.push(ExternalDep {
                        name: name.to_string(),
                        dep_type: "cargo".to_string(),
                        source_config: source.to_string(),
                    });
                }
            }
        }
    }
    deps
}

fn extract_npm_deps(contents: &str, source: &str) -> Vec<ExternalDep> {
    let mut deps = Vec::new();
    let dep_sections = ["\"dependencies\"", "\"devDependencies\""];
    for section in dep_sections {
        if let Some(start) = contents.find(section) {
            let after = &contents[start + section.len()..];
            if let Some(brace_start) = after.find('{') {
                let obj = &after[brace_start..];
                if let Some(brace_end) = find_matching_brace(obj) {
                    let inner = &obj[1..brace_end];
                    for key in extract_json_keys(inner) {
                        deps.push(ExternalDep {
                            name: key,
                            dep_type: "npm".to_string(),
                            source_config: source.to_string(),
                        });
                    }
                }
            }
        }
    }
    deps
}

fn extract_python_deps(contents: &str, source: &str) -> Vec<ExternalDep> {
    let mut deps = Vec::new();
    if source.ends_with("requirements.txt") {
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }
            let name = trimmed
                .split(&['>', '<', '=', '!', '~', '[', ';'][..])
                .next()
                .unwrap_or("")
                .trim();
            if !name.is_empty() {
                deps.push(ExternalDep {
                    name: name.to_string(),
                    dep_type: "pip".to_string(),
                    source_config: source.to_string(),
                });
            }
        }
    } else if source.ends_with("pyproject.toml") {
        let mut in_deps = false;
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed == "dependencies = [" || trimmed.starts_with("[project.dependencies]") {
                in_deps = true;
                continue;
            }
            if in_deps {
                if trimmed == "]" || (trimmed.starts_with('[') && trimmed.ends_with(']')) {
                    in_deps = false;
                    continue;
                }
                let cleaned = trimmed.trim_matches(|c| c == '"' || c == '\'' || c == ',');
                let name = cleaned
                    .split(&['>', '<', '=', '!', '~', '[', ';'][..])
                    .next()
                    .unwrap_or("")
                    .trim();
                if !name.is_empty() {
                    deps.push(ExternalDep {
                        name: name.to_string(),
                        dep_type: "pip".to_string(),
                        source_config: source.to_string(),
                    });
                }
            }
        }
    }
    deps
}

fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_json_keys(inner: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut remaining = inner;
    while let Some(quote_start) = remaining.find('"') {
        let after_start = &remaining[quote_start + 1..];
        if let Some(quote_end) = after_start.find('"') {
            let key = &after_start[..quote_end];
            keys.push(key.to_string());
            let after_key = &after_start[quote_end + 1..];
            if let Some(colon) = after_key.find(':') {
                remaining = &after_key[colon + 1..];
                if let Some(next_quote) = remaining.find('"') {
                    let after_val_start = &remaining[next_quote + 1..];
                    if let Some(val_end) = after_val_start.find('"') {
                        remaining = &after_val_start[val_end + 1..];
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            break;
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cargo_deps_finds_dependencies() {
        let contents = "[package]\nname = \"demo\"\n\n[dependencies]\nrayon = \"1.10\"\nsha2 = \"0.10\"\n\n[dev-dependencies]\ntempfile = \"3\"\n";
        let deps = extract_cargo_deps(contents, "Cargo.toml");
        assert_eq!(deps.len(), 3);
        assert!(deps.iter().any(|d| d.name == "rayon"));
        assert!(deps.iter().any(|d| d.name == "sha2"));
        assert!(deps.iter().any(|d| d.name == "tempfile"));
    }

    #[test]
    fn extract_npm_deps_finds_dependencies() {
        let contents = r#"{"dependencies":{"react":"^18.0.0","next":"^14.0.0"},"devDependencies":{"jest":"^29.0.0"}}"#;
        let deps = extract_npm_deps(contents, "package.json");
        assert_eq!(deps.len(), 3);
        assert!(deps.iter().any(|d| d.name == "react"));
        assert!(deps.iter().any(|d| d.name == "next"));
        assert!(deps.iter().any(|d| d.name == "jest"));
    }

    #[test]
    fn extract_requirements_txt() {
        let contents = "flask>=2.0\nrequests\n# comment\npandas>=1.0,<2.0\n";
        let deps = extract_python_deps(contents, "requirements.txt");
        assert_eq!(deps.len(), 3);
        assert!(deps.iter().any(|d| d.name == "flask"));
        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "pandas"));
    }
}
