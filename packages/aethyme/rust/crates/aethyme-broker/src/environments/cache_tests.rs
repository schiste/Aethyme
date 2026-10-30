//! Content-addressed builds and templates (#1219).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::lifecycle::{Lifecycle, UpOptions};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::EnvironmentStore;
use super::{Loaded, parse_loaded};
use crate::resources::HostResourceCoordinator;

fn lifecycle(host: &Path) -> Lifecycle {
    Lifecycle {
        store: EnvironmentStore::open(&host.join("environments.db")).unwrap(),
        coordinator: HostResourceCoordinator::open(&host.join("host.db")).unwrap(),
        root: host.join("environments"),
        holder_pid: None,
        hook_timeout: Duration::from_secs(20),
        actor: "test".into(),
    }
}

fn snapshot(text: &str, worktree: &Path) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    assert!(found.is_empty(), "{found:#?}");
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        &file.profiles[0].name,
        "t",
        &CompileContext {
            repository: "repo-cache".into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: fingerprint.clone(),
            },
            worktree_fingerprint: fingerprint,
            source_commit: None,
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .unwrap()
}

fn worktree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    dir
}

fn up(lifecycle: &mut Lifecycle, text: &str, worktree: &Path) -> String {
    let snapshot = snapshot(text, worktree);
    lifecycle
        .up(
            &snapshot,
            worktree,
            None,
            None,
            &UpOptions {
                wait: Duration::from_secs(5),
                reconfigure: false,
            },
        )
        .unwrap()
        .env_id
}

fn lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

fn site(counter: &Path) -> String {
    format!(
        r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
build = 'mkdir -p dist && cp web/app.txt dist/index.html && echo built >> "{counter}"'
cache_inputs = ["web/**"]
"#,
        counter = counter.display()
    )
}

/// #1219 acceptance: two worktrees on the same tree build once; a change
/// outside cache_inputs keeps the cache valid, a change inside does not.
#[test]
fn identical_trees_build_once_and_only_inputs_invalidate() {
    let host = tempfile::tempdir().unwrap();
    let counter = host.path().join("builds.log");
    let text = site(&counter);
    let mut lifecycle = lifecycle(host.path());
    let a = worktree(&[("web/app.txt", "v1"), ("notes.md", "a")]);
    let b = worktree(&[("web/app.txt", "v1"), ("notes.md", "a")]);

    let first = up(&mut lifecycle, &text, a.path());
    assert_eq!(lines(&counter), 1);
    let second = up(&mut lifecycle, &text, b.path());
    assert_eq!(lines(&counter), 1, "the second worktree reuses the build");
    assert_eq!(
        std::fs::read_to_string(b.path().join("dist/index.html")).unwrap(),
        "v1"
    );
    let log = std::fs::read_to_string(
        host.path()
            .join("environments")
            .join(&second)
            .join("site.log"),
    )
    .unwrap();
    assert!(log.contains("build cache hit"), "{log}");

    lifecycle.down(&second, None).unwrap();
    std::fs::write(b.path().join("notes.md"), "changed outside the inputs").unwrap();
    let again = up(&mut lifecycle, &text, b.path());
    assert_eq!(
        lines(&counter),
        1,
        "a file outside cache_inputs does not invalidate"
    );

    lifecycle.down(&again, None).unwrap();
    std::fs::write(b.path().join("web/app.txt"), "v2").unwrap();
    let rebuilt = up(&mut lifecycle, &text, b.path());
    assert_eq!(lines(&counter), 2, "a file inside cache_inputs does");
    assert_eq!(
        std::fs::read_to_string(b.path().join("dist/index.html")).unwrap(),
        "v2"
    );

    for id in [first, rebuilt] {
        lifecycle.down(&id, None).unwrap();
    }
    let entries = lifecycle.cache_entries();
    assert_eq!(
        entries.iter().filter(|e| e.kind == "build").count(),
        2,
        "v1 and v2"
    );
}

fn database(templates: &Path, destroys: &Path) -> String {
    format!(
        r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
template = 'sleep 0.3; echo "$AETHYME_ENV_TEMPLATE_ID" >> "{templates}"'
template_destroy = 'echo "$AETHYME_ENV_TEMPLATE_ID" >> "{destroys}"'
template_inputs = ["db/**"]
provision = 'echo "AETHYME_RECEIPT {{\"template\":\"$AETHYME_ENV_TEMPLATE_ID\"}}"'
destroy = "true"
"#,
        templates = templates.display(),
        destroys = destroys.display()
    )
}

/// #1219 acceptance: several environments provisioning on a new schema
/// digest run `template` exactly once, and each provision gets its id.
#[test]
fn concurrent_provisions_share_one_template() {
    let host = tempfile::tempdir().unwrap();
    let templates = host.path().join("templates.log");
    let destroys = host.path().join("destroys.log");
    let text = database(&templates, &destroys);
    let trees: Vec<tempfile::TempDir> = (0..3)
        .map(|_| worktree(&[("db/schema.sql", "create table t(id int);")]))
        .collect();
    // Open the stores once so concurrent openers race only on rows.
    drop(lifecycle(host.path()));
    let ids: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = trees
            .iter()
            .map(|tree| {
                let (host, text) = (host.path().to_path_buf(), text.clone());
                scope.spawn(move || up(&mut lifecycle(&host), &text, tree.path()))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(lines(&templates), 1, "one template for one digest");
    let mut lifecycle = lifecycle(host.path());
    let template = std::fs::read_to_string(&templates)
        .unwrap()
        .trim()
        .to_string();
    for id in &ids {
        let receipt = lifecycle
            .store
            .components(id)
            .unwrap()
            .into_iter()
            .find(|c| c.component == "db")
            .and_then(|c| c.receipt)
            .unwrap();
        assert_eq!(
            receipt["template"],
            template.as_str(),
            "provision received the id"
        );
    }

    // In use: prune keeps it, whatever the mode.
    let report = lifecycle.prune_cache(true);
    assert!(report.evicted.is_empty(), "{report:?}");
    assert_eq!(lines(&destroys), 0);
    for id in &ids {
        lifecycle.down(id, None).unwrap();
    }
    // Unused: evicted only through template_destroy.
    let report = lifecycle.prune_cache(true);
    assert_eq!(report.evicted.len(), 1, "{report:?}");
    assert_eq!(std::fs::read_to_string(&destroys).unwrap().trim(), template);
    assert!(lifecycle.cache_entries().is_empty());
}

#[test]
fn a_template_whose_destroy_fails_is_kept_and_reported() {
    let host = tempfile::tempdir().unwrap();
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
template = "true"
template_destroy = "exit 9"
template_inputs = ["db/**"]
provision = "true"
destroy = "true"
"#;
    let tree = worktree(&[("db/schema.sql", "x")]);
    let mut lifecycle = lifecycle(host.path());
    let id = up(&mut lifecycle, text, tree.path());
    lifecycle.down(&id, None).unwrap();
    let report = lifecycle.prune_cache(true);
    assert!(report.evicted.is_empty());
    assert_eq!(report.kept.len(), 1);
    assert!(
        report.kept[0].1.contains("template_destroy exited"),
        "{report:?}"
    );
    assert_eq!(
        lifecycle.cache_entries().len(),
        1,
        "never dropped without its destroy"
    );
}
