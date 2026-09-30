//! Minimal file snapshots for the CLI surface suites.
//!
//! A snapshot is plain text under `tests/snapshots/<group>/<name>.snap`, so a
//! change to the command surface shows up as a reviewable diff. Set
//! `AETHYME_UPDATE_SNAPSHOTS=1` to rewrite them after an intended change.

#![allow(dead_code)]

use std::path::PathBuf;

fn snapshot_dir(group: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(group)
}

fn updating() -> bool {
    std::env::var_os("AETHYME_UPDATE_SNAPSHOTS").is_some_and(|value| value == "1")
}

/// Compare every `(name, body)` against its stored snapshot. Collects all
/// mismatches before failing, so one run shows the whole surface change.
/// When updating, also removes snapshots no longer produced.
pub fn assert_snapshots(group: &str, entries: &[(String, String)]) {
    let dir = snapshot_dir(group);
    if updating() {
        std::fs::create_dir_all(&dir).expect("create snapshot dir");
        for orphan in orphaned_snapshots(&dir, entries) {
            std::fs::remove_file(dir.join(format!("{orphan}.snap")))
                .expect("remove stale snapshot");
        }
        for (name, body) in entries {
            std::fs::write(dir.join(format!("{name}.snap")), body).expect("write snapshot");
        }
        return;
    }
    let mut problems = Vec::new();
    for (name, body) in entries {
        let path = dir.join(format!("{name}.snap"));
        match std::fs::read_to_string(&path) {
            Ok(stored) if stored == *body => {}
            Ok(stored) => problems.push(format!(
                "{name}: differs from {}\n--- stored\n{}\n--- actual\n{}",
                path.display(),
                first_lines(&stored),
                first_lines(body)
            )),
            Err(_) => problems.push(format!("{name}: no snapshot at {}", path.display())),
        }
    }
    // A stored snapshot no entry produced verifies nothing, and the next
    // update run would delete it silently. `broker push` shipped that way.
    for orphan in orphaned_snapshots(&dir, entries) {
        problems.push(format!(
            "{orphan}: snapshot is not produced by this suite; register its command or delete it"
        ));
    }
    assert!(
        problems.is_empty(),
        "{} snapshot problem(s); rerun with AETHYME_UPDATE_SNAPSHOTS=1 if intended:\n{}",
        problems.len(),
        problems.join("\n\n")
    );
}

/// Stems of `.snap` files in `dir` that no entry produces, sorted.
fn orphaned_snapshots(dir: &std::path::Path, entries: &[(String, String)]) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut orphans: Vec<String> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "snap"))
        .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
        .filter(|stem| !entries.iter().any(|(name, _)| name == stem))
        .collect();
    orphans.sort();
    orphans
}

fn first_lines(text: &str) -> String {
    text.lines().take(15).collect::<Vec<_>>().join("\n")
}

/// The type skeleton of a JSON value: objects keep their (sorted) keys, arrays
/// keep the shape of their first element, and leaves become their type name.
/// Values change run to run; the shape is the contract.
pub fn json_shape(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Null => Value::String("null".into()),
        Value::Bool(_) => Value::String("bool".into()),
        Value::Number(number) if number.is_f64() => Value::String("float".into()),
        Value::Number(_) => Value::String("integer".into()),
        Value::String(_) => Value::String("string".into()),
        Value::Array(items) => match items.first() {
            Some(first) => Value::Array(vec![json_shape(first)]),
            None => Value::Array(Vec::new()),
        },
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map
                .iter()
                .map(|(key, value)| (key.clone(), json_shape(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
    }
}
