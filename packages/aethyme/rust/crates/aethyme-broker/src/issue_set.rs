//! Validation and dependency ordering for issue-set manifests.
//!
//! Validation is deliberately independent of GitHub writes: every local
//! reference and body must be checked before an apply can create anything.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub set_id: String,
    pub parent: Entry,
    #[serde(default)]
    pub issues: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub key: String,
    pub title: String,
    pub body_file: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ValidatedManifest {
    pub manifest: Manifest,
    pub bodies: BTreeMap<String, String>,
    /// Dependencies precede dependents; body references may be cyclic and
    /// must be rendered once all issue numbers have been discovered.
    pub order: Vec<String>,
}

fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} must be a regular file of at most 1 MiB",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{} is larger than 1 MiB", path.display()));
    }
    Ok(text)
}

impl ValidatedManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = read_bounded(path)?;
        let manifest: Manifest =
            toml::from_str(&text).map_err(|e| format!("invalid issue-set manifest: {e}"))?;
        if !valid_key(&manifest.set_id) {
            return Err("set_id must contain 1-64 ASCII letters, digits, '.', '_' or '-'".into());
        }
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()
            .map_err(|e| format!("manifest directory: {e}"))?;
        let entries: Vec<&Entry> = std::iter::once(&manifest.parent)
            .chain(&manifest.issues)
            .collect();
        let mut keys = BTreeSet::new();
        for entry in &entries {
            if !valid_key(&entry.key) || !keys.insert(entry.key.clone()) {
                return Err(format!("invalid or duplicate issue key {:?}", entry.key));
            }
            if entry.title.trim().is_empty()
                || entry.title.chars().count() > 200
                || entry.title.contains(['\r', '\n'])
            {
                return Err(format!("invalid title for {:?}", entry.key));
            }
            if entry.labels.iter().any(|label| label.trim().is_empty()) {
                return Err(format!("empty label for {:?}", entry.key));
            }
        }
        let mut bodies = BTreeMap::new();
        let mut remaining = BTreeMap::new();
        for entry in &entries {
            let path = root
                .join(&entry.body_file)
                .canonicalize()
                .map_err(|e| format!("body for {:?}: {e}", entry.key))?;
            if !path.starts_with(&root) {
                return Err(format!(
                    "body for {:?} is outside the manifest directory",
                    entry.key
                ));
            }
            let body = read_bounded(&path)?;
            let mut rest = body.as_str();
            while let Some((_, after)) = rest.split_once("{{") {
                let (key, suffix) = after
                    .split_once("}}")
                    .ok_or_else(|| format!("unclosed reference in {:?}", entry.key))?;
                if !keys.contains(key) {
                    return Err(format!("unknown reference {key:?} in {:?}", entry.key));
                }
                rest = suffix;
            }
            let mut dependencies = BTreeSet::new();
            for dependency in &entry.depends_on {
                if dependency == &entry.key
                    || !keys.contains(dependency)
                    || !dependencies.insert(dependency.clone())
                {
                    return Err(format!(
                        "invalid dependency {dependency:?} in {:?}",
                        entry.key
                    ));
                }
            }
            bodies.insert(entry.key.clone(), body);
            remaining.insert(entry.key.clone(), dependencies);
        }
        let mut order = Vec::new();
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .find(|(_, deps)| deps.is_empty())
                .map(|(key, _)| key.clone())
                .ok_or_else(|| "issue-set dependencies contain a cycle".to_string())?;
            remaining.remove(&next);
            for deps in remaining.values_mut() {
                deps.remove(&next);
            }
            order.push(next);
        }
        Ok(Self {
            manifest,
            bodies,
            order,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(manifest: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("epic.md"), "Child {{E1}}\n").unwrap();
        std::fs::write(dir.path().join("child.md"), "Parent {{epic}}\n").unwrap();
        let path = dir.path().join("set.toml");
        std::fs::write(&path, manifest).unwrap();
        (dir, path)
    }

    const VALID: &str = r#"
set_id = "environments"
[parent]
key = "epic"
title = "Epic"
body_file = "epic.md"
[[issues]]
key = "E1"
title = "Child"
body_file = "child.md"
depends_on = ["epic"]
"#;

    #[test]
    fn validates_cyclic_body_references_and_orders_dependencies() {
        let (_dir, path) = fixture(VALID);
        let manifest = ValidatedManifest::load(&path).unwrap();
        assert_eq!(manifest.order, ["epic", "E1"]);
        assert_eq!(manifest.manifest.set_id, "environments");
        assert!(manifest.bodies["epic"].contains("{{E1}}"));
    }

    #[test]
    fn refuses_unknown_references_and_dependency_cycles() {
        let (dir, path) = fixture(VALID);
        std::fs::write(dir.path().join("child.md"), "{{missing}}").unwrap();
        assert!(
            ValidatedManifest::load(&path)
                .unwrap_err()
                .contains("unknown reference")
        );
        std::fs::write(dir.path().join("child.md"), "{{epic}}").unwrap();
        std::fs::write(
            &path,
            VALID.replace(
                "body_file = \"epic.md\"",
                "body_file = \"epic.md\"\ndepends_on = [\"E1\"]",
            ),
        )
        .unwrap();
        assert!(
            ValidatedManifest::load(&path)
                .unwrap_err()
                .contains("cycle")
        );
    }

    #[test]
    fn refuses_duplicate_keys_and_escaping_body_paths() {
        let (_dir, path) = fixture(&VALID.replace("key = \"E1\"", "key = \"epic\""));
        assert!(
            ValidatedManifest::load(&path)
                .unwrap_err()
                .contains("duplicate")
        );
        let (_dir, path) = fixture(&VALID.replace("child.md", "../outside.md"));
        assert!(ValidatedManifest::load(&path).is_err());
    }
}
