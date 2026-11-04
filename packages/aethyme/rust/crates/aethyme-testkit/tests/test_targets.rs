//! A crate with `autotests = false` compiles only the `[[test]]` targets its
//! manifest lists, so a new `tests/<suite>.rs` that nobody lists would never be
//! built or run, and nothing would report it. Most crates gather their suites
//! as modules of one `it` binary (`tests/it.rs`, see the comment there), which
//! needs `autotests = false`. This suite makes the omission loud: every
//! top-level `tests/*.rs` file must be reached exactly once, either as a
//! `[[test]]` target or as a `#[path]` module of one.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The top-level `tests/*.rs` files of a crate, by file name.
fn suite_files(crate_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(crate_dir.join("tests")) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".rs"))
        .collect();
    files.sort();
    files
}

/// Sibling suite files a target root loads with `#[path = "<file>.rs"]`. A path
/// with a directory names a helper (`common/isolation.rs`), not a suite.
fn path_modules(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("#[path = \"")?
                .strip_suffix("\"]")
                .filter(|path| !path.contains('/'))
                .map(str::to_owned)
        })
        .collect()
}

/// Where each top-level suite file is compiled, for a crate with
/// `autotests = false`: `[[test]]` targets and the `#[path]` modules of their
/// roots. `None` when the crate discovers its tests automatically.
fn reached_suites(crate_dir: &Path) -> Option<BTreeMap<String, Vec<String>>> {
    let manifest: toml::Table = std::fs::read_to_string(crate_dir.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let autotests = manifest
        .get("package")
        .and_then(|package| package.get("autotests"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    if autotests {
        return None;
    }
    let mut reached: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let targets = manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for target in targets {
        let name = target["name"].as_str().unwrap().to_owned();
        let path = target
            .get("path")
            .and_then(toml::Value::as_str)
            .map_or_else(|| format!("tests/{name}.rs"), str::to_owned);
        let Some(file) = path
            .strip_prefix("tests/")
            .filter(|file| !file.contains('/'))
        else {
            continue;
        };
        reached
            .entry(file.to_owned())
            .or_default()
            .push(format!("[[test]] {name}"));
        for module in path_modules(&crate_dir.join(&path)) {
            reached
                .entry(module.clone())
                .or_default()
                .push(format!("module of {name}"));
        }
    }
    Some(reached)
}

#[test]
fn every_top_level_test_file_is_compiled_exactly_once() {
    let mut problems = Vec::new();
    let mut checked = 0;
    let mut crates: Vec<PathBuf> = std::fs::read_dir(crates_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect();
    crates.sort();
    for crate_dir in crates {
        let Some(reached) = reached_suites(&crate_dir) else {
            continue;
        };
        checked += 1;
        let crate_name = crate_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let files = suite_files(&crate_dir);
        for file in &files {
            match reached.get(file).map(Vec::as_slice) {
                None | Some([]) => problems.push(format!(
                    "{crate_name}/tests/{file} is never compiled: add `#[path = \"{file}\"] mod ...;` to tests/it.rs, or a [[test]] target"
                )),
                Some([_]) => {}
                Some(places) => problems.push(format!(
                    "{crate_name}/tests/{file} is compiled more than once, so its tests run twice: {places:?}"
                )),
            }
        }
        for file in reached.keys() {
            if !files.contains(file) {
                problems.push(format!(
                    "{crate_name}: {file} is listed but tests/{file} does not exist"
                ));
            }
        }
    }
    assert!(
        checked > 0,
        "no crate under {:?} sets autotests = false",
        crates_dir()
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
