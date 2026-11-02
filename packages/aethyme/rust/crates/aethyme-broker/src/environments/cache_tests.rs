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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn interactive_prune_refuses_unsupported_platform_without_deleting_entries() {
    let host = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(host.path());
    let sentinel = lifecycle
        .cache_root()
        .join(super::cache::BUILDS)
        .join("repo-cache")
        .join("site")
        .join("0123456789abcdef")
        .join("output/index.html");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, b"keep").unwrap();

    let report = lifecycle.prune_cache(true);

    assert!(
        report
            .refused
            .as_deref()
            .is_some_and(|reason| reason.contains("supported only on macOS and Linux")),
        "unsupported prune must give an actionable refusal: {report:?}"
    );
    assert!(report.evicted.is_empty(), "{report:?}");
    assert_eq!(std::fs::read(sentinel).unwrap(), b"keep");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn prune_all_refuses_incomplete_template_inventory_before_removing_builds() {
    let host = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(host.path());
    let cache = lifecycle.cache_root();
    let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let build = cache
        .join(super::cache::BUILDS)
        .join("repo-cache")
        .join("site")
        .join(&digest[..16]);
    std::fs::create_dir_all(build.join("output")).unwrap();
    std::fs::write(
        build.join(super::cache::KEY_FILE),
        format!("aethyme-env-build:repo-cache:site:{digest}"),
    )
    .unwrap();
    std::fs::write(build.join(super::cache::COMPLETE), b"").unwrap();
    std::fs::write(build.join("output/index.html"), b"cached").unwrap();

    let template_dir = cache
        .join(super::cache::TEMPLATES)
        .join("repo-cache")
        .join("database");
    std::fs::create_dir_all(&template_dir).unwrap();
    let outside = host.path().join("outside-marker.json");
    std::fs::write(&outside, b"leave this file alone").unwrap();
    std::os::unix::fs::symlink(&outside, template_dir.join("1234567890abcdef.json")).unwrap();

    let report = lifecycle.prune_cache(true);

    assert!(report.refused.is_some(), "{report:?}");
    assert!(report.evicted.is_empty(), "{report:?}");
    assert!(build.join("output/index.html").exists());
    assert_eq!(std::fs::read(outside).unwrap(), b"leave this file alone");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn ordinary_prune_does_not_inspect_template_markers() {
    let host = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(host.path());
    let template_dir = lifecycle
        .cache_root()
        .join(super::cache::TEMPLATES)
        .join("repo-cache")
        .join("database");
    std::fs::create_dir_all(&template_dir).unwrap();
    let outside = host.path().join("outside-marker.json");
    std::fs::write(&outside, b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, template_dir.join("bad.json")).unwrap();

    let report = lifecycle.prune_cache(false);

    assert!(report.refused.is_none(), "{report:?}");
    assert!(report.evicted.is_empty(), "{report:?}");
    assert_eq!(std::fs::read(outside).unwrap(), b"keep");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn prune_retains_a_build_without_its_exact_historical_lock_key() {
    let host = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(host.path());
    let build = lifecycle
        .cache_root()
        .join(super::cache::BUILDS)
        .join("repo-cache")
        .join("site")
        .join("0123456789abcdef");
    std::fs::create_dir_all(build.join("output")).unwrap();
    std::fs::write(build.join(super::cache::COMPLETE), b"").unwrap();
    std::fs::write(build.join("output/index.html"), b"cached").unwrap();

    let report = lifecycle.prune_cache(true);

    assert!(report.evicted.is_empty(), "{report:?}");
    assert_eq!(report.kept.len(), 1, "{report:?}");
    assert!(report.kept[0].1.contains("no recorded shared cache lock"));
    assert!(build.join("output/index.html").exists());
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
template_destroy = 'printf "%s\n%s\n%s\n" "$DB_SERVER" "$AETHYME_ENV_TEMPLATE_ID" "$PWD" >> "{destroys}"'
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
    let output = std::fs::read_to_string(&destroys).unwrap();
    let mut lines = output.lines();
    assert_eq!(lines.next(), Some("pg-test"), "destroy got component env");
    assert!(
        lines.next().is_some_and(|id| id.len() == 16),
        "destroy got the template id: {output}"
    );
    let cwd = std::fs::canonicalize(lifecycle.cache_root())
        .unwrap()
        .join(super::cache::TEMPLATES)
        .join("repo-cache")
        .join("db");
    let hook_cwd = Path::new(
        lines
            .next()
            .expect("destroy recorded its working directory"),
    );
    assert!(
        hook_cwd.starts_with(cwd),
        "destroy ran from its pinned template directory: {hook_cwd:?}"
    );
}

/// When prune cannot tell what a live environment uses, it evicts nothing.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
        super::cache::gc_inventory(Some(&lifecycle.store), &root, "repo-cache", 0, None)
    };

    let (inventory, candidates) = plan(&lifecycle);
    let inventory = inventory.expect("this repository has cache entries");
    assert_eq!(inventory.build_entries, Some(2), "{inventory:?}");
    assert_eq!(inventory.in_use_entries, Some(1), "{inventory:?}");
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    let mut legacy_candidate = candidates[0].clone();
    legacy_candidate.repository.clear();
    legacy_candidate.cache_root.clear();
    let refusal = lifecycle
        .evict_planned_build(&legacy_candidate)
        .expect_err("a pre-identity plan must not be rebound from current disk state");
    assert!(
        refusal.contains("environment_cache_identity_missing"),
        "{refusal}"
    );
    assert!(
        Path::new(&legacy_candidate.path)
            .join(super::cache::KEY_FILE)
            .exists()
    );
    let used = lifecycle
        .cache_entries()
        .into_iter()
        .find(|e| e.in_use)
        .unwrap();
    assert_ne!(Path::new(&candidates[0].path), used.path);
    assert!(
        super::cache::gc_inventory(Some(&lifecycle.store), &root, "other", 0, None)
            .0
            .is_none()
    );

    // The used build, even if a plan named it, is kept.
    let mut forged = candidates[0].clone();
    let canonical_cache = std::fs::canonicalize(root.join(super::cache::CACHE_DIRECTORY)).unwrap();
    let canonical_used = std::fs::canonicalize(&used.path).unwrap();
    let relative_used = canonical_used
        .strip_prefix(canonical_cache.join(super::cache::BUILDS))
        .unwrap();
    let mut components = relative_used.components();
    forged.repository = components
        .next()
        .unwrap()
        .as_os_str()
        .to_str()
        .unwrap()
        .into();
    forged.component = components
        .next()
        .unwrap()
        .as_os_str()
        .to_str()
        .unwrap()
        .into();
    forged.digest = components
        .next()
        .unwrap()
        .as_os_str()
        .to_str()
        .unwrap()
        .into();
    assert!(components.next().is_none());
    forged.path = canonical_used.to_string_lossy().into_owned();
    forged.cache_key = Some(
        used.key
            .clone()
            .expect("used build has a recorded lock key"),
    );
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
    assert!(kept.starts_with("used_since_plan:"), "{kept}");
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

/// A partial host walk is never exposed as a complete report or eviction set.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn gc_withholds_partial_inventory_when_later_repository_is_unsafe() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("environments");
    let cache = root.join(super::cache::CACHE_DIRECTORY);
    let repository = "repo-cache";
    let build = cache
        .join(super::cache::BUILDS)
        .join(repository)
        .join("component")
        .join("digest");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join(super::cache::KEY_FILE), "cache-key").unwrap();
    std::fs::write(build.join(super::cache::COMPLETE), b"").unwrap();
    std::fs::write(build.join("payload"), b"payload").unwrap();

    let outside = temporary.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, cache.join(super::cache::BUILDS).join("zz-link")).unwrap();

    let (inventory, candidates) = super::cache::gc_inventory(None, &root, "repo-cache", 0, None);
    let inventory = inventory.expect("the malformed host cache must be reported");
    assert!(!inventory.complete, "{inventory:?}");
    assert_eq!(inventory.refusal_code.as_deref(), Some("unsafe_path"));
    assert!(inventory.host_total_bytes.is_none());
    assert!(inventory.build_entries.is_none());
    assert!(inventory.build_bytes.is_none());
    assert!(inventory.in_use_entries.is_none());
    assert!(inventory.in_use_bytes.is_none());
    assert!(inventory.reclaimable_bytes.is_none());
    assert!(inventory.template_entries.is_none());
    assert!(
        candidates.is_empty(),
        "partial walk cannot authorize deletion"
    );
    assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"keep");
}

/// A live cacheable environment without readable use metadata authorizes no eviction.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn gc_withholds_candidates_when_a_live_use_record_is_missing() {
    let host = tempfile::tempdir().unwrap();
    let counter = host.path().join("builds.log");
    let text = site(&counter);
    let mut lifecycle = lifecycle(host.path());
    let tree = worktree(&[("web/app.txt", "v1")]);
    let live = up(&mut lifecycle, &text, tree.path());
    let use_record = lifecycle.root.join(&live).join(super::cache::USES_FILE);
    std::fs::remove_file(&use_record).unwrap();

    let (inventory, candidates) = super::cache::gc_inventory(
        Some(&lifecycle.store),
        &lifecycle.root,
        "repo-cache",
        0,
        None,
    );
    let inventory = inventory.expect("the cache entry is present");
    assert!(!inventory.complete, "{inventory:?}");
    assert_eq!(inventory.refusal_code.as_deref(), Some("store_unreadable"));
    assert!(inventory.host_total_bytes.is_none());
    assert!(candidates.is_empty());
    assert!(use_record.parent().unwrap().exists());
    lifecycle.down(&live, None).unwrap();
}

/// Expired bounded inventory also withholds every count and candidate.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn gc_withholds_inventory_when_deadline_is_already_expired() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("environments");
    std::fs::create_dir_all(root.join(super::cache::CACHE_DIRECTORY)).unwrap();

    let (inventory, candidates) = super::cache::gc_inventory(
        None,
        &root,
        "repo-cache",
        0,
        Some(std::time::Instant::now()),
    );
    let inventory = inventory.expect("an incomplete scan is reportable");
    assert!(!inventory.complete, "{inventory:?}");
    assert_eq!(inventory.refusal_code.as_deref(), Some("deadline_exceeded"));
    assert!(inventory.host_total_bytes.is_none());
    assert!(inventory.build_entries.is_none());
    assert!(inventory.reclaimable_bytes.is_none());
    assert!(candidates.is_empty());
}
