//! Unit tests for `aethyme mcp`: env files, offline resolution, validation,
//! copy choice and keys. Checkouts are described with a fake, so no broker
//! worktree root is needed; one test builds a real Git worktree to check the
//! Git-backed source identity.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::checkouts::{Checkout, Checkouts};
use super::config::{Entry, RepositoryAnchor, Scope, parse_and_validate};
use super::connect::{parse_args, server_environment};
use super::envfile;
use super::key::{Caller, CopyKind, plan, tree_identity};
use super::resolve::{Method, ResolveInput, resolve};
use super::telemetry;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn write_executable(path: &Path, text: &str) {
    write(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

/// No checkout anywhere.
struct NoCheckouts;

impl Checkouts for NoCheckouts {
    fn checkout(&self, _: &Path) -> Option<Checkout> {
        None
    }
    fn session_worktree(&self, _: &Path) -> Option<Checkout> {
        None
    }
}

/// One repository: a stable checkout and some session worktrees.
struct FakeCheckouts {
    stable: PathBuf,
    worktrees: Vec<PathBuf>,
    common: PathBuf,
}

impl FakeCheckouts {
    fn containing(&self, path: &Path, roots: &[&PathBuf]) -> Option<Checkout> {
        roots
            .iter()
            .find(|root| path.starts_with(root))
            .map(|root| Checkout {
                root: (*root).clone(),
                common_dir: self.common.clone(),
            })
    }
}

impl Checkouts for FakeCheckouts {
    fn checkout(&self, path: &Path) -> Option<Checkout> {
        let mut roots: Vec<&PathBuf> = self.worktrees.iter().collect();
        roots.push(&self.stable);
        self.containing(path, &roots)
    }
    fn session_worktree(&self, path: &Path) -> Option<Checkout> {
        let roots: Vec<&PathBuf> = self.worktrees.iter().collect();
        self.containing(path, &roots)
    }
}

fn host_entry(toml_text: &str, checkouts: &dyn Checkouts) -> Entry {
    let (mut entries, errors) = parse_and_validate(
        toml_text,
        Path::new("/host/mcp/services.toml"),
        Scope::Host,
        None,
        checkouts,
    );
    assert!(errors.is_empty(), "{errors:?}");
    entries.remove(0)
}

fn caller(cwd: &Path, path_env: Option<&Path>) -> Caller {
    Caller {
        cwd: cwd.to_path_buf(),
        path_env: path_env.map(|path| OsString::from(path.as_os_str())),
        npm_cache: None,
        from_worktree: false,
    }
}

// ── env files ──────────────────────────────────────────────────────────────

#[test]
fn env_file_parses_quotes_comments_and_never_expands() {
    let text = "\
# a comment
export API_URL=https://example.invalid/v1   # trailing comment
PLAIN = value with spaces
SINGLE='$HOME stays # literal'
DOUBLE=\"line\\nnext \\\"quoted\\\" $HOME\"
EMPTY=
HASH=abc#def
TICKS=`whoami`
PLAIN=replaced
";
    let parsed = envfile::parse(text).unwrap();
    let get = |name: &str| {
        parsed
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("API_URL"), Some("https://example.invalid/v1"));
    assert_eq!(get("SINGLE"), Some("$HOME stays # literal"));
    assert_eq!(get("DOUBLE"), Some("line\nnext \"quoted\" $HOME"));
    assert_eq!(get("EMPTY"), Some(""));
    assert_eq!(get("HASH"), Some("abc#def"));
    assert_eq!(get("TICKS"), Some("`whoami`"));
    assert_eq!(get("PLAIN"), Some("replaced"));
    assert_eq!(parsed.iter().filter(|(key, _)| key == "PLAIN").count(), 1);
}

#[test]
fn env_file_errors_name_the_line_but_not_the_value() {
    let error = envfile::parse("GOOD=1\nSECRET='abc123\n").unwrap_err();
    assert_eq!(error.line, 2);
    assert!(!error.to_string().contains("abc123"), "{error}");
    assert_eq!(envfile::parse("1BAD=x").unwrap_err().line, 1);
    assert_eq!(envfile::parse("no assignment").unwrap_err().line, 1);
    assert!(envfile::parse("A=\"x\" trailing").is_err());
}

// ── resolution ─────────────────────────────────────────────────────────────

#[test]
fn pnpm_exec_resolves_to_the_installed_bin_shim() {
    let (_dir, root) = canonical_tempdir();
    let project = root.join("project");
    let shim = project.join("node_modules/.bin/example-server");
    write_executable(&shim, "#!/bin/sh\nexec node \"$0\" \"$@\"\n");
    let nested = project.join("packages/server");
    std::fs::create_dir_all(&nested).unwrap();
    let argv = strings(&["pnpm", "exec", "example-server", "--stdio"]);
    let resolution = resolve(&ResolveInput {
        argv: &argv,
        cwd: &nested,
        path_env: None,
        npm_cache: None,
    });
    assert!(resolution.resolved, "{resolution:?}");
    assert_eq!(resolution.method, Method::NodeModulesBin);
    assert_eq!(resolution.launcher, Some("pnpm exec"));
    assert_eq!(
        resolution.argv,
        vec![shim.to_string_lossy().into_owned(), "--stdio".into()]
    );
    let identity = resolution.identity.unwrap();
    assert_eq!(identity.shebang.as_deref(), Some("/bin/sh"));
    assert!(identity.interpreter.is_some());
}

#[test]
fn npm_exec_and_npx_resolve_an_installed_package_through_env_shebangs() {
    let (_dir, root) = canonical_tempdir();
    let bin = root.join("bin");
    let node = bin.join("node");
    write_executable(&node, "#!/bin/sh\n");
    let project = root.join("project");
    let package = project.join("node_modules/@example/server");
    write(
        &package.join("package.json"),
        r#"{"name":"@example/server","version":"1.2.3","bin":{"example-server":"dist/cli.js"}}"#,
    );
    write_executable(
        &package.join("dist/cli.js"),
        "#!/usr/bin/env node\nconsole.log(1)\n",
    );
    let script = package.join("dist/cli.js").to_string_lossy().into_owned();
    let node_shown = node.to_string_lossy().into_owned();
    for argv in [
        strings(&["npm", "exec", "@example/server", "--", "--port", "0"]),
        strings(&[
            "npm",
            "exec",
            "--yes",
            "--",
            "@example/server@1.2.3",
            "--port",
            "0",
        ]),
        strings(&["npx", "-y", "@example/server@^1.2", "--port", "0"]),
    ] {
        let resolution = resolve(&ResolveInput {
            argv: &argv,
            cwd: &project,
            path_env: Some(bin.as_os_str()),
            npm_cache: None,
        });
        assert!(resolution.resolved, "{argv:?}: {resolution:?}");
        assert_eq!(resolution.method, Method::InstalledPackage);
        // `npm exec X -- args`: npm consumes the `--`; the server gets args.
        let mut expected = vec![node_shown.clone(), script.clone()];
        expected.extend(strings(&["--port", "0"]));
        assert_eq!(resolution.argv, expected, "{argv:?}");
        assert_eq!(
            resolution.package.as_ref().unwrap().version.as_deref(),
            Some("1.2.3")
        );
    }
}

#[test]
fn an_exact_version_that_is_not_installed_is_not_substituted() {
    let (_dir, root) = canonical_tempdir();
    let project = root.join("project");
    let package = project.join("node_modules/example-server");
    write(
        &package.join("package.json"),
        r#"{"name":"example-server","version":"1.2.4","bin":"cli.js"}"#,
    );
    write_executable(&package.join("cli.js"), "#!/bin/sh\n");
    let argv = strings(&["npx", "-y", "example-server@1.2.3"]);
    let resolution = resolve(&ResolveInput {
        argv: &argv,
        cwd: &project,
        path_env: None,
        npm_cache: None,
    });
    assert!(!resolution.resolved);
    assert_eq!(resolution.reason, Some("not_installed"));
    assert!(resolution.message.unwrap().contains("1.2.4"));
    assert_eq!(resolution.argv, argv);
}

#[test]
fn latest_resolves_to_the_newest_copy_in_the_npx_cache_with_a_note() {
    let (_dir, root) = canonical_tempdir();
    let cache = root.join("npm-cache");
    for (hash, version) in [("aaaa", "1.0.0"), ("bbbb", "2.1.0"), ("cccc", "2.0.5")] {
        let package = cache
            .join("_npx")
            .join(hash)
            .join("node_modules/example-browser");
        write(
            &package.join("package.json"),
            &format!(
                r#"{{"name":"example-browser","version":"{version}","bin":{{"example-browser":"cli.sh"}}}}"#
            ),
        );
        write_executable(&package.join("cli.sh"), "#!/bin/sh\n");
    }
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let argv = strings(&["npx", "-y", "example-browser@latest", "--isolated"]);
    let resolution = resolve(&ResolveInput {
        argv: &argv,
        cwd: &workdir,
        path_env: None,
        npm_cache: Some(&cache),
    });
    assert!(resolution.resolved, "{resolution:?}");
    assert_eq!(resolution.method, Method::NpxCache);
    assert_eq!(
        resolution.package.as_ref().unwrap().version.as_deref(),
        Some("2.1.0")
    );
    assert!(resolution.argv[0].contains("bbbb"), "{:?}", resolution.argv);
    assert_eq!(resolution.argv[1], "--isolated");
    assert!(resolution.notes.iter().any(|note| note.contains("@latest")));
}

#[test]
fn resolution_never_downloads_and_keeps_the_declared_argv() {
    let (_dir, root) = canonical_tempdir();
    let cache = root.join("npm-cache");
    std::fs::create_dir_all(&cache).unwrap();
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let bin = root.join("bin");
    // A package manager on PATH that would record any execution.
    let marker = root.join("ran");
    for launcher in ["npx", "npm", "pnpm"] {
        write_executable(
            &bin.join(launcher),
            &format!("#!/bin/sh\ntouch {}\n", marker.display()),
        );
    }
    for argv in [
        strings(&["npx", "-y", "example-devtools@latest"]),
        strings(&["npm", "exec", "example-devtools"]),
        strings(&["pnpm", "exec", "example-devtools"]),
    ] {
        let resolution = resolve(&ResolveInput {
            argv: &argv,
            cwd: &workdir,
            path_env: Some(bin.as_os_str()),
            npm_cache: Some(&cache),
        });
        assert!(!resolution.resolved, "{argv:?}");
        assert_eq!(resolution.reason, Some("not_installed"));
        assert_eq!(resolution.argv, argv);
        assert_eq!(resolution.method, Method::Unresolved);
    }
    assert!(!marker.exists(), "a package manager was executed");
    assert_eq!(
        std::fs::read_dir(&cache).unwrap().count(),
        0,
        "the cache was written"
    );
    assert!(!workdir.join("node_modules").exists());
}

#[test]
fn unsupported_launcher_forms_are_reported_not_guessed() {
    let (_dir, root) = canonical_tempdir();
    for argv in [
        strings(&["npx", "-p", "example", "example-cli"]),
        strings(&["npx", "./local-script.js"]),
        strings(&["pnpm", "dlx", "example"]),
        strings(&["npm", "run", "serve"]),
        strings(&["npx", "-y", "example@next"]),
        strings(&["npm", "exec", "example", "--port", "0"]),
    ] {
        let resolution = resolve(&ResolveInput {
            argv: &argv,
            cwd: &root,
            path_env: None,
            npm_cache: None,
        });
        assert!(!resolution.resolved, "{argv:?}");
        assert!(
            matches!(
                resolution.reason,
                Some("unsupported_launcher_form" | "unsupported_version")
            ),
            "{argv:?}: {:?}",
            resolution.reason
        );
        assert_eq!(resolution.argv, argv);
    }
}

#[test]
fn a_bare_program_resolves_on_the_callers_path() {
    let (_dir, root) = canonical_tempdir();
    let bin = root.join("bin");
    let server = bin.join("example-go-server");
    write_executable(&server, "\x7fELF-not-really");
    let argv = strings(&["example-go-server", "-v"]);
    let resolution = resolve(&ResolveInput {
        argv: &argv,
        cwd: &root,
        path_env: Some(bin.as_os_str()),
        npm_cache: None,
    });
    assert!(resolution.resolved);
    assert_eq!(resolution.method, Method::Direct);
    assert_eq!(
        resolution.argv,
        vec![server.to_string_lossy().into_owned(), "-v".into()]
    );
    let missing = resolve(&ResolveInput {
        argv: &argv,
        cwd: &root,
        path_env: None,
        npm_cache: None,
    });
    assert_eq!(missing.reason, Some("program_not_found"));
}

// ── configuration ──────────────────────────────────────────────────────────

#[test]
fn entries_inside_a_session_worktree_are_refused_with_the_reason() {
    let (_dir, root) = canonical_tempdir();
    let stable = root.join("project");
    let worktree = root.join("worktrees/session-1");
    std::fs::create_dir_all(&stable).unwrap();
    std::fs::create_dir_all(worktree.join("server")).unwrap();
    let checkouts = FakeCheckouts {
        stable: stable.clone(),
        worktrees: vec![worktree.clone()],
        common: stable.join(".git"),
    };
    for (field, line) in [
        ("cwd", format!("cwd = {:?}", worktree.display().to_string())),
        (
            "source",
            format!(
                "source = {:?}",
                worktree.join("server").display().to_string()
            ),
        ),
    ] {
        let text = format!(
            "schema = 1\n[[servers]]\nname = \"example-server\"\nargv = [\"/bin/sh\"]\n{line}\n"
        );
        let (entries, errors) = parse_and_validate(
            &text,
            Path::new("/host/mcp/services.toml"),
            Scope::Host,
            None,
            &checkouts,
        );
        assert!(entries.is_empty(), "{field}");
        assert_eq!(errors.len(), 1, "{field}");
        assert_eq!(errors[0].code, "session_worktree_path");
        assert_eq!(errors[0].field, Some(field));
        assert!(
            errors[0].message.contains("cleaned up"),
            "{}",
            errors[0].message
        );
    }
    let text = format!(
        "schema = 1\n[[servers]]\nname = \"example-server\"\nargv = [\"/bin/sh\"]\ncwd = {:?}\n",
        stable.display().to_string()
    );
    let (entries, errors) = parse_and_validate(
        &text,
        Path::new("/host/mcp/services.toml"),
        Scope::Host,
        None,
        &checkouts,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(entries.len(), 1);
}

#[test]
fn validation_refuses_bad_files_and_keeps_good_entries() {
    let path = Path::new("/host/mcp/services.toml");
    let (_, errors) = parse_and_validate(
        "schema = 1\n[[servers]]\nname = \"a\"\nargv = [\"x\"]\nunknown = 1\n",
        path,
        Scope::Host,
        None,
        &NoCheckouts,
    );
    assert_eq!(errors[0].code, "invalid_toml");
    let (_, errors) = parse_and_validate("schema = 2\n", path, Scope::Host, None, &NoCheckouts);
    assert_eq!(errors[0].code, "unsupported_schema");
    let text = "schema = 1
[[servers]]
name = \"good\"
argv = [\"/bin/sh\"]
[[servers]]
name = \"good\"
argv = [\"/bin/sh\"]
[[servers]]
name = \"relative\"
argv = [\"/bin/sh\"]
cwd = \"srv/example\"
[[servers]]
name = \"bad name\"
argv = [\"/bin/sh\"]
[[servers]]
name = \"bad-env\"
argv = [\"/bin/sh\"]
env_allow = [\"NOT-A-NAME\"]
";
    let (entries, errors) = parse_and_validate(text, path, Scope::Host, None, &NoCheckouts);
    assert_eq!(entries.len(), 1);
    let codes: Vec<&str> = errors.iter().map(|error| error.code).collect();
    assert_eq!(
        codes,
        vec![
            "duplicate_name",
            "relative_path",
            "invalid_name",
            "invalid_env_name"
        ]
    );
}

#[test]
fn repository_entries_anchor_relative_paths_to_the_main_checkout() {
    let (_dir, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("packages/server")).unwrap();
    let anchor = RepositoryAnchor {
        main_root: root.clone(),
        identity: "example-0123".into(),
    };
    let (entries, errors) = parse_and_validate(
        "schema = 1\n[[servers]]\nname = \"s\"\nargv = [\"/bin/sh\"]\ncwd = \"packages/server\"\nsource = \".\"\nenv_file = \".env\"\n",
        &root.join(".aethyme/mcp.toml"),
        Scope::Repository,
        Some(&anchor),
        &NoCheckouts,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let entry = &entries[0];
    assert_eq!(
        entry.cwd.as_deref(),
        Some(root.join("packages/server").as_path())
    );
    assert_eq!(
        entry.source.as_deref(),
        Some(root.join("packages/server").as_path())
    );
    assert_eq!(
        entry.env_file.as_deref(),
        Some(root.join("packages/server/.env").as_path())
    );
    assert_eq!(entry.repository.as_ref().unwrap().identity, "example-0123");
}

// ── keys and copies ────────────────────────────────────────────────────────

fn keyed_entry(stable: &Path, extra: &str) -> String {
    format!(
        "schema = 1\n[[servers]]\nname = \"example-server\"\nmode = \"shared\"\nargv = [\"/bin/sh\", \"server.sh\"]\ncwd = {:?}\nsource = {:?}\n{extra}\n",
        stable.display().to_string(),
        stable.join("src").display().to_string(),
    )
}

#[test]
fn keys_are_stable_and_separate_source_flavour_env_file_and_repository() {
    let (_dir, root) = canonical_tempdir();
    let stable = root.join("project");
    write(&stable.join("src/main.txt"), "main\n");
    write(&stable.join("env.prod"), "MODE=prod\n");
    write(&stable.join("env.staging"), "MODE=staging\n");
    let key_of = |extra: &str| {
        let entry = host_entry(&keyed_entry(&stable, extra), &NoCheckouts);
        plan(&entry, &caller(&root, None), &NoCheckouts)
            .unwrap()
            .key
    };
    let base = key_of("");
    assert_eq!(base, key_of(""), "same inputs, same key");
    assert_ne!(base, key_of("flavour = \"staging\""));
    assert_ne!(
        key_of("env_file = \"env.prod\""),
        key_of("env_file = \"env.staging\"")
    );

    write(&stable.join("src/main.txt"), "changed\n");
    assert_ne!(base, key_of(""), "a changed source tree is another server");

    let mut entry = host_entry(&keyed_entry(&stable, ""), &NoCheckouts);
    let host_key = plan(&entry, &caller(&root, None), &NoCheckouts)
        .unwrap()
        .key;
    entry.repository = Some(RepositoryAnchor {
        main_root: stable.clone(),
        identity: "repository-a".into(),
    });
    let repository_a = plan(&entry, &caller(&root, None), &NoCheckouts)
        .unwrap()
        .key;
    entry.repository = Some(RepositoryAnchor {
        main_root: stable.clone(),
        identity: "repository-b".into(),
    });
    let repository_b = plan(&entry, &caller(&root, None), &NoCheckouts)
        .unwrap()
        .key;
    assert_ne!(host_key, repository_a);
    assert_ne!(repository_a, repository_b);
}

fn branch_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, FakeCheckouts) {
    let (dir, root) = canonical_tempdir();
    let stable = root.join("project");
    let first = root.join("worktrees/session-1");
    let second = root.join("worktrees/session-2");
    for checkout in [&stable, &first, &second] {
        write(&checkout.join("src/main.txt"), "main\n");
        write(&checkout.join("server.sh"), "exit 0\n");
    }
    let checkouts = FakeCheckouts {
        stable: stable.clone(),
        worktrees: vec![first.clone(), second.clone()],
        common: stable.join(".git"),
    };
    (dir, stable, first, second, checkouts)
}

#[test]
fn a_session_worktree_with_the_same_source_uses_the_stable_copy() {
    let (_dir, stable, first, _second, checkouts) = branch_fixture();
    let entry = host_entry(&keyed_entry(&stable, ""), &checkouts);
    let outside = plan(&entry, &caller(stable.parent().unwrap(), None), &checkouts).unwrap();
    let inside = plan(&entry, &caller(&first, None), &checkouts).unwrap();
    assert_eq!(outside.copy.kind, CopyKind::Stable);
    assert_eq!(inside.copy.kind, CopyKind::Stable);
    assert_eq!(inside.copy.reason, "source_identical");
    assert_eq!(inside.cwd, stable);
    assert_eq!(inside.key, outside.key, "identical trees share a server");
}

#[test]
fn a_session_worktree_with_a_different_source_uses_its_own_copy() {
    let (_dir, stable, first, second, checkouts) = branch_fixture();
    write(&first.join("src/main.txt"), "branch\n");
    let entry = host_entry(&keyed_entry(&stable, ""), &checkouts);
    let stable_plan = plan(&entry, &caller(&stable, None), &checkouts).unwrap();
    let branch_plan = plan(&entry, &caller(&first.join("src"), None), &checkouts).unwrap();
    assert_eq!(branch_plan.copy.kind, CopyKind::Worktree);
    assert_eq!(branch_plan.copy.reason, "source_differs");
    assert_eq!(branch_plan.cwd, first);
    assert_eq!(
        branch_plan.source.as_deref(),
        Some(first.join("src").as_path())
    );
    assert_ne!(
        branch_plan.key, stable_plan.key,
        "a branch gets its own server"
    );

    // Another session on the same branch shares the branch's server.
    write(&second.join("src/main.txt"), "branch\n");
    let same_branch = plan(&entry, &caller(&second, None), &checkouts).unwrap();
    assert_eq!(same_branch.copy.kind, CopyKind::Worktree);
    assert_eq!(same_branch.key, branch_plan.key);

    // And a third branch gets a third server.
    write(&second.join("src/main.txt"), "other branch\n");
    let other_branch = plan(&entry, &caller(&second, None), &checkouts).unwrap();
    assert_ne!(other_branch.key, branch_plan.key);
    assert_ne!(other_branch.key, stable_plan.key);
}

#[test]
fn from_worktree_forces_the_worktree_copy_and_refuses_outside_one() {
    let (_dir, stable, first, _second, checkouts) = branch_fixture();
    let entry = host_entry(&keyed_entry(&stable, ""), &checkouts);
    let mut forced = caller(&first, None);
    forced.from_worktree = true;
    let plan_inside = plan(&entry, &forced, &checkouts).unwrap();
    assert_eq!(plan_inside.copy.kind, CopyKind::Worktree);
    assert_eq!(plan_inside.copy.reason, "requested");
    assert_eq!(plan_inside.cwd, first);
    let mut outside = caller(&stable, None);
    outside.from_worktree = true;
    let failure = plan(&entry, &outside, &checkouts).unwrap_err();
    assert_eq!(failure.code, "no_worktree_copy");
}

#[test]
fn git_source_identity_matches_identical_worktrees_and_sees_changes() {
    let (_dir, root) = canonical_tempdir();
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    let git = |dir: &Path, args: &[&str]| {
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
    };
    git(&main, &["init", "-q", "-b", "main"]);
    write(&main.join("src/index.txt"), "one\n");
    write(&main.join("other.txt"), "outside the source\n");
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "init"]);
    let worktree = root.join("wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "topic",
        ],
    );

    let main_source = main.join("src");
    let worktree_source = worktree.join("src");
    let base = tree_identity(&main_source).unwrap();
    assert_eq!(base, tree_identity(&worktree_source).unwrap());

    write(&worktree.join("other.txt"), "changed outside\n");
    assert_eq!(
        base,
        tree_identity(&worktree_source).unwrap(),
        "outside source"
    );

    write(&worktree.join("src/index.txt"), "two\n");
    let modified = tree_identity(&worktree_source).unwrap();
    assert_ne!(base, modified);

    write(&worktree.join("src/index.txt"), "one\n");
    write(&worktree.join("src/new.txt"), "untracked\n");
    let untracked = tree_identity(&worktree_source).unwrap();
    assert_ne!(base, untracked);
    assert_ne!(modified, untracked);
}

// ── connect ────────────────────────────────────────────────────────────────

#[test]
fn connect_arguments_parse_the_fallback_command_verbatim() {
    let parsed = parse_args(&strings(&[
        "example-server",
        "--from-worktree",
        "--fallback-cwd",
        "/srv/example",
        "--fallback",
        "--",
        "npx",
        "-y",
        "example-server@latest",
        "--help",
    ]))
    .unwrap();
    assert_eq!(parsed.name, "example-server");
    assert!(parsed.from_worktree);
    assert_eq!(parsed.fallback_cwd, Some(PathBuf::from("/srv/example")));
    assert_eq!(
        parsed.fallback,
        Some(strings(&["npx", "-y", "example-server@latest", "--help"]))
    );
    assert!(parse_args(&strings(&["a", "--", "x"])).is_err());
    assert!(parse_args(&strings(&["a", "--fallback"])).is_err());
    assert!(parse_args(&strings(&["--fallback", "--", "x"])).is_err());
}

#[test]
fn the_server_gets_allowed_names_and_the_env_file_only() {
    let (_dir, root) = canonical_tempdir();
    write(&root.join("env"), "TOKEN=from-file\nKEEP=overridden\n");
    let entry = host_entry(
        &format!(
            "schema = 1\n[[servers]]\nname = \"s\"\nargv = [\"/bin/sh\"]\ncwd = {:?}\nenv_file = \"env\"\nenv_allow = [\"PATH\", \"KEEP\", \"ABSENT\"]\n",
            root.display().to_string()
        ),
        &NoCheckouts,
    );
    let planned = plan(&entry, &caller(&root, None), &NoCheckouts).unwrap();
    assert!(
        !format!("{planned:?}").contains("from-file"),
        "Debug leaks values"
    );
    let lookup = |name: &str| match name {
        "PATH" => Some(OsString::from("/usr/bin")),
        "KEEP" => Some(OsString::from("caller")),
        "SECRET" => Some(OsString::from("never")),
        _ => None,
    };
    let mut environment = server_environment(&planned, &lookup);
    environment.sort();
    assert_eq!(
        environment,
        vec![
            (OsString::from("KEEP"), OsString::from("overridden")),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("TOKEN"), OsString::from("from-file")),
        ]
    );
}

#[test]
fn config_snippets_wrap_the_declared_argv_as_a_fallback() {
    let entry = host_entry(
        "schema = 1\n[[servers]]\nname = \"example-server\"\nargv = [\"npx\", \"-y\", \"example-server@latest\"]\ncwd = \"/srv/example\"\n",
        &NoCheckouts,
    );
    let codex = super::cli::config_snippet("codex", &[&entry]).unwrap();
    let parsed: toml::Table = toml::from_str(&codex).unwrap();
    let server = parsed["mcp_servers"]["example-server"].as_table().unwrap();
    assert_eq!(server["command"].as_str(), Some("aethyme"));
    let args: Vec<&str> = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(
        args,
        vec![
            "mcp",
            "connect",
            "example-server",
            "--fallback-cwd",
            "/srv/example",
            "--fallback",
            "--",
            "npx",
            "-y",
            "example-server@latest"
        ]
    );
    let claude = super::cli::config_snippet("claude", &[&entry]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&claude).unwrap();
    assert_eq!(parsed["mcpServers"]["example-server"]["command"], "aethyme");
    assert_eq!(
        parsed["mcpServers"]["example-server"]["args"][2],
        "example-server"
    );
    assert!(super::cli::config_snippet("other", &[&entry]).is_err());
}

// ── telemetry ──────────────────────────────────────────────────────────────

#[test]
fn telemetry_counts_outcomes_and_reasons_and_detects_changed_executables() {
    let (_dir, root) = canonical_tempdir();
    let event = |outcome: &str, reason: Option<&str>, resolve_ms: u64| telemetry::Event {
        at_ms: 1,
        command: "connect".into(),
        name: "example-server".into(),
        outcome: outcome.into(),
        reason: reason.map(str::to_string),
        copy: Some("stable".into()),
        key: Some("0123456789ab".into()),
        phases_ms: [("resolve".to_string(), resolve_ms)].into_iter().collect(),
    };
    telemetry::append(&root, &event("exec", None, 2)).unwrap();
    telemetry::append(&root, &event("fallback", Some("not_installed"), 4)).unwrap();
    telemetry::append(&root, &event("error", Some("not_installed"), 9)).unwrap();
    let summary = telemetry::summarize(&telemetry::read_events(&root));
    let counters = &summary["example-server"];
    assert_eq!(counters.events, 3);
    assert_eq!(counters.outcomes["fallback"], 1);
    assert_eq!(counters.failure_reasons["not_installed"], 2);
    assert_eq!(counters.resolve_ms_p50, Some(4));
    assert_eq!(counters.resolve_ms_max, Some(9));

    let script = root.join("server.sh");
    write_executable(&script, "#!/bin/sh\n");
    let argv = vec![script.to_string_lossy().into_owned()];
    let resolved = resolve(&ResolveInput {
        argv: &argv,
        cwd: &root,
        path_env: None,
        npm_cache: None,
    });
    let identity = resolved.identity.unwrap();
    let first = telemetry::compare_and_remember(&root, "host-example", &argv, &identity);
    assert!(first.first_seen);
    let same = telemetry::compare_and_remember(&root, "host-example", &argv, &identity);
    assert!(!same.first_seen && same.changes.is_empty());
    write_executable(&script, "#!/bin/sh\necho a longer script\n");
    let changed = resolve(&ResolveInput {
        argv: &argv,
        cwd: &root,
        path_env: None,
        npm_cache: None,
    })
    .identity
    .unwrap();
    let after = telemetry::compare_and_remember(&root, "host-example", &argv, &changed);
    assert_eq!(after.changes, vec!["content"]);
}
