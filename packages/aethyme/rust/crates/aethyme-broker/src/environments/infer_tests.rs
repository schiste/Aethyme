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

/// Paths Git would C-quote (non-ASCII here) still match their rules.
#[test]
fn unusual_paths_are_matched_verbatim() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, "README", "x\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
    git(dir, &["branch", "aethyme/integration"]);
    write(dir, "db/schema/café.sql", "create table t();\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "schema"]);

    let (_, changed) = changed_paths(dir).unwrap();
    assert!(
        changed.iter().any(|p| p == "db/schema/café.sql"),
        "{changed:?}"
    );
    assert_eq!(infer(&file(), &changed).unwrap().minimum, "isolated");
}

/// Without any base, uncommitted changes to tracked files still count.
#[test]
fn without_a_base_uncommitted_changes_still_count() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "trunk"]);
    write(dir, "server/api.py", "x = 1\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
    write(dir, "server/api.py", "x = 2\n");

    let (base, changed) = changed_paths(dir).unwrap();
    assert!(base.is_none());
    assert_eq!(changed, vec!["server/api.py".to_string()]);
    assert_eq!(infer(&file(), &changed).unwrap().minimum, "backend");
}

/// A merge commit counts what it brought in, compared with its first parent.
#[test]
fn a_range_replay_counts_what_a_merge_brought_in() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, "README", "x\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
    git(dir, &["checkout", "-qb", "topic"]);
    write(dir, "app/migrations/0001.sql", "x\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "migration"]);
    git(dir, &["checkout", "-q", "main"]);
    git(
        dir,
        &["merge", "-q", "--no-ff", "-m", "merge topic", "topic"],
    );

    let report = infer_range(dir, &file(), "HEAD^1..HEAD").unwrap();
    assert_eq!(report.commits, 2, "{report:?}");
    assert_eq!(report.profiles.get("isolated"), Some(&2), "{report:?}");
}

/// A raised minimum retires a cheaper environment only after the new one is
/// up, and never a pinned one.
#[test]
fn replacing_below_minimum_keeps_pinned_environments() {
    use super::compile::{CompileContext, EnvironmentOwner, compile};
    use super::lifecycle::{Lifecycle, UpOptions};
    use super::spec::HostEnvironmentsFile;
    use super::store::EnvironmentStore;
    use std::time::Duration;

    let host = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let mut lifecycle = Lifecycle {
        store: EnvironmentStore::open(&host.path().join("environments.db")).unwrap(),
        coordinator: crate::resources::HostResourceCoordinator::open(&host.path().join("host.db"))
            .unwrap(),
        root: host.path().join("environments"),
        holder_pid: None,
        hook_timeout: Duration::from_secs(20),
        actor: "test".into(),
    };
    let file = file();
    let fingerprint = crate::console::worktree_fingerprint(worktree.path());
    let up = |lifecycle: &mut Lifecycle, profile: &str| {
        let snapshot = compile(
            &file,
            &HostEnvironmentsFile::default(),
            profile,
            "t",
            &CompileContext {
                repository: "repo-infer".into(),
                owner: EnvironmentOwner::Worktree {
                    fingerprint: fingerprint.clone(),
                },
                worktree_fingerprint: fingerprint.clone(),
                source_commit: None,
                source_dirty: false,
                source_digest: String::new(),
            },
        )
        .unwrap();
        lifecycle
            .up(
                &snapshot,
                worktree.path(),
                None,
                None,
                &UpOptions {
                    wait: Duration::from_secs(5),
                    reconfigure: false,
                },
            )
            .unwrap()
            .env_id
    };
    let pinned = up(&mut lifecycle, "static");
    lifecycle
        .pin(&pinned, Duration::from_secs(3600), "demo")
        .unwrap();
    let plain = up(&mut lifecycle, "backend");
    let inference = infer(&file, &paths(&["db/schema/a.sql"])).unwrap();

    let (replaced, kept) =
        super::operate::replace_below_minimum(&mut lifecycle, worktree.path(), &file, &inference);
    assert_eq!(replaced, vec![plain.clone()]);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, pinned);
    assert!(
        lifecycle
            .store
            .get(&plain)
            .unwrap()
            .is_none_or(|r| !r.phase.retains_worktree())
    );
    lifecycle.down(&pinned, None).unwrap();
}

/// #1273: a session whose committed changes raise the minimum above its
/// running environment is reported for `broker status`, and stops being
/// reported once an environment meeting the minimum runs.
#[test]
fn a_live_environment_below_the_minimum_is_reported() {
    use super::compile::{CompileContext, EnvironmentOwner, compile};
    use super::lifecycle::{Lifecycle, UpOptions};
    use super::spec::HostEnvironmentsFile;
    use super::store::EnvironmentStore;
    use std::time::Duration;

    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, ".aethyme/environments.toml", RULES);
    write(dir, "README", "x\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
    git(dir, &["branch", "aethyme/integration"]);

    let host = tempfile::tempdir().unwrap();
    let mut lifecycle = Lifecycle {
        store: EnvironmentStore::open(&host.path().join("environments.db")).unwrap(),
        coordinator: crate::resources::HostResourceCoordinator::open(&host.path().join("host.db"))
            .unwrap(),
        root: host.path().join("environments"),
        holder_pid: None,
        hook_timeout: Duration::from_secs(20),
        actor: "test".into(),
    };
    let file = file();
    let fingerprint = crate::console::worktree_fingerprint(dir);
    let up = |lifecycle: &mut Lifecycle, profile: &str| {
        let snapshot = compile(
            &file,
            &HostEnvironmentsFile::default(),
            profile,
            "t",
            &CompileContext {
                repository: "repo-below".into(),
                owner: EnvironmentOwner::Worktree {
                    fingerprint: fingerprint.clone(),
                },
                worktree_fingerprint: fingerprint.clone(),
                source_commit: None,
                source_dirty: false,
                source_digest: String::new(),
            },
        )
        .unwrap();
        lifecycle
            .up(
                &snapshot,
                dir,
                None,
                None,
                &UpOptions {
                    wait: Duration::from_secs(5),
                    reconfigure: false,
                },
            )
            .unwrap()
            .env_id
    };
    let cheap = up(&mut lifecycle, "static");
    let sessions = vec![(7, dir.to_path_buf())];
    assert!(
        super::infer::below_minimum(&lifecycle.store, &sessions).is_empty(),
        "no changes yet"
    );

    write(dir, "db/migrations/0002.sql", "alter table t add c int;\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "migration"]);
    let found = super::infer::below_minimum(&lifecycle.store, &sessions);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].session_id, 7);
    assert_eq!(found[0].env_id, cheap);
    assert_eq!(found[0].minimum, "isolated");
    assert!(
        found[0].rules[0].contains("db/migrations/0002.sql"),
        "{found:?}"
    );

    let enough = up(&mut lifecycle, "isolated");
    assert!(
        super::infer::below_minimum(&lifecycle.store, &sessions).is_empty(),
        "an environment meeting the minimum now runs"
    );
    for id in [cheap, enough] {
        lifecycle.down(&id, None).unwrap();
    }
}

/// #1290: a rule's `exclude` carves exceptions out of a broad rule; the
/// highest-rank rule still wins for everything it does match.
#[test]
fn an_excluded_path_never_fires_its_rule() {
    let text = RULES.replace(
        "paths = [\"server/**\", \"src/**/*.rs\"]\n",
        "paths = [\"server/**\", \"src/**/*.rs\"]\nexclude = [\"server/docs/**\"]\n",
    );
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", &text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));

    let docs = infer(&file, &paths(&["server/docs/api.md"])).unwrap();
    assert_eq!(docs.minimum, "static", "{docs:?}");
    assert!(docs.fired.is_empty(), "{docs:?}");
    assert_eq!(docs.unmatched, paths(&["server/docs/api.md"]));

    let code = infer(&file, &paths(&["server/docs/api.md", "server/main.rs"])).unwrap();
    assert_eq!(code.minimum, "backend", "{code:?}");
    assert_eq!(code.fired[0].paths, paths(&["server/main.rs"]));
}
