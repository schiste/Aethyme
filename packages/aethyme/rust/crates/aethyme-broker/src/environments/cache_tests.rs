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
                recreate: Vec::new(),
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
        report.kept[0].1.contains("template_destroy failed"),
        "{report:?}"
    );
    assert_eq!(
        lifecycle.cache_entries().len(),
        1,
        "never dropped without its destroy"
    );
}

fn validation(text: &str) -> Vec<&'static str> {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    super::validate(&file, &HostEnvironmentsFile::default(), "t", "h")
        .iter()
        .map(|d| d.code)
        .collect()
}

/// A cache hit replaces the output directory: `.`, `..` or an absolute path
/// would replace the worktree itself or something outside it.
#[test]
fn an_output_outside_a_worktree_subdirectory_is_refused() {
    for output in [".", "..", "/tmp/x", "dist/../..", ""] {
        let text = format!(
            r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "{output}"
build = "true"
cache_inputs = ["web/**"]
"#
        );
        assert!(
            validation(&text).contains(&"invalid_path"),
            "{output:?} accepted"
        );
    }
    assert!(super::cache::is_worktree_subdirectory("dist/site"));
}

/// A cached build sees only its declared inputs, never the environment's
/// bindings, which differ per worktree and would leak into every hit.
#[test]
fn a_cached_build_never_sees_per_environment_bindings() {
    let host = tempfile::tempdir().unwrap();
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
build = 'mkdir -p dist && printf "%s|%s" "$AETHYME_ENV_NAMESPACE" "$GREETING" > dist/index.html'
cache_inputs = ["web/**"]
env = { GREETING = "hi" }
"#;
    let mut lifecycle = lifecycle(host.path());
    let tree = worktree(&[("web/app.txt", "v1")]);
    let id = up(&mut lifecycle, text, tree.path());
    assert_eq!(
        std::fs::read_to_string(tree.path().join("dist/index.html")).unwrap(),
        "|hi"
    );
    lifecycle.down(&id, None).unwrap();
}

/// Deleting a tracked input without staging it is an input change, not an
/// error that blocks every `env up`.
#[test]
fn a_deleted_tracked_input_rebuilds() {
    let host = tempfile::tempdir().unwrap();
    let counter = host.path().join("builds.log");
    let text = site(&counter);
    let mut lifecycle = lifecycle(host.path());
    let tree = worktree(&[("web/app.txt", "v1"), ("web/old.txt", "gone soon")]);
    let status = Command::new("git")
        .args(["add", "-A"])
        .current_dir(tree.path())
        .status()
        .unwrap();
    assert!(status.success());
    let first = up(&mut lifecycle, &text, tree.path());
    lifecycle.down(&first, None).unwrap();
    std::fs::remove_file(tree.path().join("web/old.txt")).unwrap();
    let second = up(&mut lifecycle, &text, tree.path());
    assert_eq!(lines(&counter), 2, "the deletion is a new input");
    lifecycle.down(&second, None).unwrap();
}

/// Eviction works after the creating worktree is gone, with the component's
/// own variables.
#[test]
fn a_template_is_destroyed_after_its_worktree_is_gone() {
    let host = tempfile::tempdir().unwrap();
    let destroys = host.path().join("destroys.log");
    let text = format!(
        r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
template = "true"
template_destroy = 'echo "$DB_SERVER $AETHYME_ENV_TEMPLATE_ID" >> "{destroys}"'
template_inputs = ["db/**"]
provision = "true"
destroy = "true"
env = {{ DB_SERVER = "pg-test" }}
"#,
        destroys = destroys.display()
    );
    let mut lifecycle = lifecycle(host.path());
    let tree = worktree(&[("db/schema.sql", "x")]);
    let id = up(&mut lifecycle, &text, tree.path());
    lifecycle.down(&id, None).unwrap();
    drop(tree);
    let report = lifecycle.prune_cache(true);
    assert_eq!(report.evicted.len(), 1, "{report:?}");
    assert!(
        std::fs::read_to_string(&destroys)
            .unwrap()
            .starts_with("pg-test "),
        "destroy got the component env"
    );
}

/// When prune cannot tell what a live environment uses, it evicts nothing.
#[test]
fn prune_evicts_nothing_when_live_use_is_unknown() {
    let host = tempfile::tempdir().unwrap();
    let counter = host.path().join("builds.log");
    let text = site(&counter);
    let mut lifecycle = lifecycle(host.path());
    let a = worktree(&[("web/app.txt", "v1")]);
    let b = worktree(&[("web/app.txt", "v2")]);
    let old = up(&mut lifecycle, &text, a.path());
    lifecycle.down(&old, None).unwrap();
    let live = up(&mut lifecycle, &text, b.path());
    std::fs::remove_file(
        host.path()
            .join("environments")
            .join(&live)
            .join(super::cache::USES_FILE),
    )
    .unwrap();
    let report = lifecycle.prune_cache(true);
    assert!(report.refused.is_some(), "{report:?}");
    assert!(report.evicted.is_empty());
    lifecycle.down(&live, None).unwrap();
}

/// #1275: `broker gc plan` proposes the unused build of this repository and
/// never the one a live environment uses; `gc apply` evicts through the same
/// key and use check as prune, and keeps an entry used since the plan.
#[test]
fn gc_proposes_and_evicts_only_an_unused_build() {
    let host = tempfile::tempdir().unwrap();
    let counter = host.path().join("builds.log");
    let text = site(&counter);
    let mut lifecycle = lifecycle(host.path());
    let a = worktree(&[("web/app.txt", "v1")]);
    let b = worktree(&[("web/app.txt", "v2")]);
    let old = up(&mut lifecycle, &text, a.path());
    lifecycle.down(&old, None).unwrap();
    let live = up(&mut lifecycle, &text, b.path());
    let root = lifecycle.root.clone();
    let plan = |lifecycle: &Lifecycle| {
        super::cache::gc_inventory(Some(&lifecycle.store), &root, "repo-cache", 0)
    };

    let (inventory, candidates) = plan(&lifecycle);
    let inventory = inventory.expect("this repository has cache entries");
    assert_eq!(inventory.build_entries, 2, "{inventory:?}");
    assert_eq!(inventory.in_use_entries, 1, "{inventory:?}");
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    let used = lifecycle
        .cache_entries()
        .into_iter()
        .find(|e| e.in_use)
        .unwrap();
    assert_ne!(Path::new(&candidates[0].path), used.path);
    assert!(
        super::cache::gc_inventory(Some(&lifecycle.store), &root, "other", 0)
            .0
            .is_none()
    );

    // The used build, even if a plan named it, is kept.
    let mut forged = candidates[0].clone();
    forged.path = used.path.to_string_lossy().into_owned();
    forged.last_used_at_ms = used.last_used;
    let kept = lifecycle.evict_planned_build(&forged).unwrap_err();
    assert!(kept.contains("in use"), "{kept}");
    assert!(used.path.exists());
    // Nothing outside the build cache, whatever the journal says.
    forged.path = a.path().to_string_lossy().into_owned();
    forged.last_used_at_ms = None;
    assert!(lifecycle.evict_planned_build(&forged).is_err());
    assert!(a.path().exists());

    assert_eq!(lifecycle.evict_planned_build(&candidates[0]), Ok(true));
    assert!(!Path::new(&candidates[0].path).exists());
    assert_eq!(lifecycle.evict_planned_build(&candidates[0]), Ok(false));

    // Used between plan and apply: kept.
    lifecycle.down(&live, None).unwrap();
    let (_, candidates) = plan(&lifecycle);
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    let complete = Path::new(&candidates[0].path).join(super::cache::COMPLETE);
    std::fs::File::options()
        .write(true)
        .open(&complete)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    let kept = lifecycle.evict_planned_build(&candidates[0]).unwrap_err();
    assert!(kept.contains("used since"), "{kept}");
    assert!(complete.exists());
}

/// #1297: `redeploy_on = "inputs"` needs inputs, and they must be globs.
#[test]
fn redeploy_on_inputs_needs_valid_inputs() {
    let text = |extra: &str| {
        format!(
            r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
[[shared]]
name = "db"
profile = "p"
ref = "default_branch"
{extra}
"#
        )
    };
    assert!(validation(&text("redeploy_on = \"inputs\"")).contains(&"missing_inputs"));
    assert!(
        validation(&text("redeploy_on = \"inputs\"\ninputs = [\"db/[\"]"))
            .contains(&"invalid_glob")
    );
    assert!(validation(&text("redeploy_on = \"inputs\"\ninputs = [\"db/**\"]")).is_empty());
}
