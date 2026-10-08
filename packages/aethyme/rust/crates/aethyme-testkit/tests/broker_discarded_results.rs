use std::path::{Path, PathBuf};

use aethyme_testkit::rust_workspace_root;

/// `let _ =` sites in aethyme-broker production code (outside `#[cfg(test)]`
/// items) after the P2.13 audit. Each remaining site is either infallible
/// (`write!` into a `String`), best-effort cleanup (temporary files, killing a
/// finished child, closing), or documented at the site as intentional.
///
/// A failed broker state write (store row, event, journal, lease or queue
/// transition, marker file) must propagate with `?` or be reported through
/// `crate::warn_unrecorded`, never dropped. Lower this number when a site goes
/// away; never raise it to admit a new one without that review.
const BASELINE: usize = 143;

fn rust_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files_under(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Lines holding `let _ =`, skipping every item annotated `#[cfg(test)]`.
///
/// The item is skipped by brace depth, which is enough for this crate's test
/// modules; a string literal with unbalanced braces would only miscount the
/// lines of the module it sits in.
fn production_discards(text: &str) -> Vec<usize> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() == "#[cfg(test)]" {
            let mut next = index + 1;
            while next < lines.len() && lines[next].trim().starts_with("#[") {
                next += 1;
            }
            if next < lines.len() && lines[next].trim_end().ends_with(';') {
                index = next + 1;
                continue;
            }
            let mut depth = 0_i64;
            let mut opened = false;
            while next < lines.len() {
                let line = lines[next];
                depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
                opened |= line.contains('{');
                next += 1;
                if opened && depth <= 0 {
                    break;
                }
            }
            index = next;
            continue;
        }
        if lines[index].contains("let _ =") {
            found.push(index + 1);
        }
        index += 1;
    }
    found
}

/// The directory holding the out-of-line submodules declared in `file`:
/// `foo/` for `foo.rs`, and the file's own directory for `mod.rs`, `lib.rs`
/// and `main.rs`.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new(""));
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod" | "lib" | "main") | None => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
    }
}

/// Paths that out-of-line `#[cfg(test)] mod NAME;` declarations in `file`
/// resolve to: `<dir>/NAME.rs`, and `<dir>/NAME/`, which holds both
/// `NAME/mod.rs` and every submodule beneath the test module.
fn out_of_line_test_modules(file: &Path, text: &str) -> Vec<PathBuf> {
    let lines = text.lines().collect::<Vec<_>>();
    let dir = module_dir(file);
    let mut paths = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        let mut next = index + 1;
        while next < lines.len() && lines[next].trim().starts_with("#[") {
            next += 1;
        }
        let Some(declaration) = lines.get(next) else {
            continue;
        };
        let declaration = declaration.trim();
        let Some(name) = declaration
            .strip_suffix(';')
            .and_then(|rest| rest.rsplit_once("mod "))
            .map(|(visibility, name)| (visibility.trim(), name.trim()))
            .filter(|(visibility, _)| visibility.is_empty() || visibility.starts_with("pub"))
            .map(|(_, name)| name)
        else {
            continue;
        };
        paths.push(dir.join(format!("{name}.rs")));
        paths.push(dir.join(name));
    }
    paths
}

/// Whether `path` is an out-of-line test module file or lies beneath one.
fn is_test_module_file(path: &Path, test_modules: &[PathBuf]) -> bool {
    test_modules
        .iter()
        .any(|module| path == module || path.starts_with(module))
}

#[test]
fn broker_production_code_does_not_discard_more_results() {
    let workspace = rust_workspace_root();
    let source = workspace.join("crates/aethyme-broker/src");
    let mut files = Vec::new();
    rust_files_under(&source, &mut files);
    files.sort();
    assert!(
        files.len() > 50,
        "expected to walk the broker sources, found {} files",
        files.len()
    );

    let texts = files
        .iter()
        .map(|file| {
            std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()))
        })
        .collect::<Vec<_>>();
    let test_modules = files
        .iter()
        .zip(&texts)
        .flat_map(|(file, text)| out_of_line_test_modules(file, text))
        .collect::<Vec<_>>();

    let mut sites = Vec::new();
    for (file, text) in files.iter().zip(&texts) {
        if is_test_module_file(file, &test_modules) {
            continue;
        }
        for line in production_discards(text) {
            sites.push(format!(
                "{}:{line}",
                file.strip_prefix(&workspace).unwrap_or(file).display()
            ));
        }
    }

    assert!(
        sites.len() <= BASELINE,
        "aethyme-broker production code has {} `let _ =` sites, above the baseline of \
         {BASELINE}. A discarded Result can silently lose broker state: propagate the \
         error with `?`, or report it with `crate::warn_unrecorded`. Only infallible or \
         best-effort cleanup may be discarded, with a comment where the reason is not \
         obvious. Sites:\n{}",
        sites.len(),
        sites.join("\n")
    );
}

#[test]
fn test_items_are_not_counted() {
    let text = "fn a() {\n    let _ = one();\n}\n#[cfg(test)]\nmod tests {\n    fn b() {\n        let _ = two();\n    }\n}\n#[cfg(test)]\nmod other;\nfn c() {\n    let _ = three();\n}\n";
    assert_eq!(production_discards(text), vec![2, 13]);
}

#[test]
fn out_of_line_test_modules_are_not_counted_and_plain_modules_are() {
    let text = "#[cfg(test)]\nmod tests;\nmod plain;\n#[cfg(test)]\n#[path = \"x\"]\npub(crate) mod helpers;\n";
    let modules = out_of_line_test_modules(Path::new("src/operations.rs"), text);
    for excluded in [
        "src/operations/tests.rs",
        "src/operations/tests/mod.rs",
        "src/operations/tests/fixtures.rs",
        "src/operations/helpers.rs",
    ] {
        assert!(
            is_test_module_file(Path::new(excluded), &modules),
            "{excluded} should be excluded"
        );
    }
    for counted in [
        "src/operations/plain.rs",
        "src/operations.rs",
        "src/operations/testsuite.rs",
    ] {
        assert!(
            !is_test_module_file(Path::new(counted), &modules),
            "{counted} should be counted"
        );
    }

    let root = out_of_line_test_modules(Path::new("src/lib.rs"), "#[cfg(test)]\nmod t;\n");
    assert!(is_test_module_file(Path::new("src/t.rs"), &root));
    let nested = out_of_line_test_modules(Path::new("src/a/mod.rs"), "#[cfg(test)]\nmod t;\n");
    assert!(is_test_module_file(Path::new("src/a/t/mod.rs"), &nested));
}
