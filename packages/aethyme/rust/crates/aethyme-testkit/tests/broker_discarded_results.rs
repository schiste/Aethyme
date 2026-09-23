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

    let mut sites = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        for line in production_discards(&text) {
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
