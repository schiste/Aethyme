//! Profile inference (#1216): rules over the session diff, raise-only, and
//! the range replay for tuning.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::infer::{changed_paths, infer, infer_range};
use super::spec::EnvironmentsFile;
use super::{Loaded, parse_loaded};

const RULES: &str = r#"
schema = 1
default_profile = "static"

[[profiles]]
name = "static"
[[profiles.components]]
name = "web"
runtime = "hooks"
provision = "true"
destroy = "true"

[[profiles]]
name = "backend"
[[profiles.components]]
name = "api"
runtime = "hooks"
provision = "true"
destroy = "true"

[[profiles]]
name = "isolated"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"

[[rules]]
name = "schema"
paths = ["**/migrations/**", "db/schema/**"]
min_profile = "isolated"
reason = "schema changes need a private database"

[[rules]]
name = "server"
paths = ["server/**", "src/**/*.rs"]
min_profile = "backend"
"#;

fn file() -> EnvironmentsFile {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", RULES);
    loaded.value.unwrap_or_else(|d| panic!("{}", d.render()))
}

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|p| p.to_string()).collect()
}

#[test]
fn three_diffs_classify_into_three_profiles_with_explanations() {
    let file = file();
    let frontend = infer(&file, &paths(&["web/app.css", "web/index.html"])).unwrap();
    assert_eq!(frontend.minimum, "static");
    assert!(frontend.fired.is_empty());
    assert_eq!(frontend.unmatched.len(), 2, "unmatched paths are reported");

    let server = infer(&file, &paths(&["server/handlers.py", "web/app.css"])).unwrap();
    assert_eq!(server.minimum, "backend");
    assert_eq!(server.fired[0].rule, "server");
    assert_eq!(server.fired[0].paths, ["server/handlers.py"]);

    let schema = infer(
        &file,
        &paths(&["server/models.py", "app/migrations/0042_add.py"]),
    )
    .unwrap();
    assert_eq!(
        schema.minimum, "isolated",
        "the highest-ranked fired rule wins"
    );
    let fired: Vec<&str> = schema.fired.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(fired, ["schema", "server"]);
    assert_eq!(
        schema.fired[0].reason.as_deref(),
        Some("schema changes need a private database")
    );
}

#[test]
fn globs_cross_directories_with_double_star_only() {
    let file = file();
    assert_eq!(
        infer(&file, &paths(&["src/a/b.rs"])).unwrap().minimum,
        "backend"
    );
    assert_eq!(
        infer(&file, &paths(&["src/main.rs"])).unwrap().minimum,
        "backend"
    );
    assert_eq!(
        infer(&file, &paths(&["lib/src/main.rs"])).unwrap().minimum,
        "static",
        "patterns are anchored at the repository root"
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write(dir: &Path, path: &str, text: &str) {
    let full = dir.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

/// The diff basis: committed since the base, uncommitted, untracked, and
/// both sides of a rename.
#[test]
fn changed_paths_cover_the_whole_session_diff() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    write(
        dir,
        "server/old_name.py",
        "x = 1\n# a body long enough to be detected as a rename\n",
    );
    write(dir, "web/app.css", "a{}\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
    git(dir, &["branch", "aethyme/integration"]);
    git(dir, &["checkout", "-qb", "work"]);
    std::fs::create_dir_all(dir.join("db/schema")).unwrap();
    git(dir, &["mv", "server/old_name.py", "db/schema/new_name.py"]);
    git(dir, &["commit", "-qm", "rename"]);
    write(dir, "web/app.css", "a{color:red}\n");
    write(dir, "app/migrations/0001.py", "new\n");

    let (base, changed) = changed_paths(dir).unwrap();
    assert!(base.is_some());
    for expected in [
        "server/old_name.py",
        "db/schema/new_name.py",
        "web/app.css",
        "app/migrations/0001.py",
    ] {
        assert!(
            changed.iter().any(|p| p == expected),
            "{expected} in {changed:?}"
        );
    }
    assert_eq!(infer(&file(), &changed).unwrap().minimum, "isolated");
}

/// #1216 acceptance: replaying a 100-commit history completes in seconds
/// and counts commits per profile.
#[test]
fn a_range_replay_counts_profiles_quickly() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    for n in 0..100 {
        let path = match n % 10 {
            0 => format!("app/migrations/{n:04}.py"),
            1..=3 => format!("server/m{n}.py"),
            _ => format!("web/p{n}.css"),
        };
        write(dir, &path, &format!("{n}\n"));
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", &format!("c{n}")]);
    }
    let started = Instant::now();
    let report = infer_range(dir, &file(), "HEAD~99..HEAD").unwrap();
    assert!(
        started.elapsed().as_secs() < 5,
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(report.commits, 99);
    let total: usize = report.profiles.values().sum();
    assert_eq!(total, 99);
    assert_eq!(report.profiles.get("isolated"), Some(&9));
    assert_eq!(
        report.top_rule.get("isolated").map(String::as_str),
        Some("schema")
    );
}
