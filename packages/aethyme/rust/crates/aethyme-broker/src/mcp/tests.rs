//! Unit tests for `aethyme mcp`: env files, offline resolution, validation,
//! trust and shadowing, copy choice, keys and identities. Checkouts are
//! described with a fake, so no broker worktree root is needed; the Git
//! tests build real repositories.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::checkouts::{Checkout, Checkouts};
use super::config::{
    Entry, EnvInherit, RepositoryAnchor, Scope, Sources, load, parse_and_validate,
};
use super::connect::{parse_args, scan_fallback};
use super::envfile;
use super::identity::{DEFAULT_LIMITS, Limits, tree_identity, tree_identity_cached};
use super::key::{Caller, CopyKind, plan, server_environment};
use super::resolve::{Method, ResolveInput, parse_node_shim, resolve};
use super::{secret, telemetry, trust};

const SECRET: &[u8] = b"test-secret-0123456789abcdef0123";

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

fn os_pairs(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

fn git(dir: &Path, args: &[&str]) {
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
}

fn input<'a>(argv: &'a [String], cwd: &'a Path) -> ResolveInput<'a> {
    ResolveInput {
        argv,
        cwd,
        path_env: None,
        npm_cache: None,
        allow_cached: false,
    }
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
        env: Vec::new(),
        key_secret: SECRET.to_vec(),
        key_secret_error: None,
        identity_cache: None,
        limits: DEFAULT_LIMITS,
    }
}

// ── env files and secrets ──────────────────────────────────────────────────

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

#[test]
fn hmac_matches_the_rfc_4231_vector_and_the_secret_is_private() {
    assert_eq!(
        secret::hmac_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    let (_dir, root) = canonical_tempdir();
    let first = secret::load_or_create(&root.join("mcp")).unwrap();
    assert_eq!(first.len(), 32);
    assert_eq!(secret::load_or_create(&root.join("mcp")).unwrap(), first);
    let mode = std::fs::metadata(root.join("mcp").join(secret::SECRET_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

// ── resolution ─────────────────────────────────────────────────────────────

#[test]
fn pnpm_exec_resolves_to_the_installed_bin_up_to_the_project_root() {
    let (_dir, root) = canonical_tempdir();
    let project = root.join("project");
    write(&project.join("pnpm-lock.yaml"), "lockfileVersion: 9\n");
    let shim = project.join("node_modules/.bin/example-server");
    write_executable(
        &shim,
        "#!/bin/sh\nset -e\nexec example-server-real \"$@\"\n",
    );
    let nested = project.join("packages/server");
    std::fs::create_dir_all(&nested).unwrap();
    let argv = strings(&["pnpm", "exec", "example-server", "--stdio"]);
    let resolution = resolve(&input(&argv, &nested));
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
fn the_node_modules_walk_stops_at_the_project_root() {
    let (_dir, root) = canonical_tempdir();
    // An install above the project must not be used.
    write_executable(
        &root.join("node_modules/.bin/example-server"),
        "#!/bin/sh\n",
    );
    let project = root.join("project");
    write(&project.join("package-lock.json"), "{}\n");
    std::fs::create_dir_all(project.join("app")).unwrap();
    let argv = strings(&["pnpm", "exec", "example-server"]);
    let resolution = resolve(&input(&argv, &project.join("app")));
    assert!(!resolution.resolved);
    assert_eq!(resolution.reason, Some("not_installed"));
}

#[test]
fn a_pnpm_shim_is_read_to_its_target_and_started_with_node() {
    let (_dir, root) = canonical_tempdir();
    let bin = root.join("bin");
    let node = bin.join("node");
    write_executable(&node, "#!/bin/sh\n");
    let project = root.join("project");
    write(&project.join("pnpm-lock.yaml"), "\n");
    let target = project.join("node_modules/example-server/dist/cli.js");
    write(&target, "console.log(1)\n");
    let shim = project.join("node_modules/.bin/example-server");
    let shim_text = r#"#!/bin/sh
basedir=$(dirname "$(echo "$0" | sed -e 's,\\,/,g')")

case `uname` in
    *CYGWIN*) basedir=`cygpath -w "$basedir"`;;
esac

if [ -z "$NODE_PATH" ]; then
  export NODE_PATH="/project/node_modules/.pnpm/node_modules"
else
  export NODE_PATH="/project/node_modules/.pnpm/node_modules:$NODE_PATH"
fi
if [ -x "$basedir/node" ]; then
  exec "$basedir/node"  "$basedir/../example-server/dist/cli.js" "$@"
else
  exec node  "$basedir/../example-server/dist/cli.js" "$@"
fi
"#;
    write_executable(&shim, shim_text);
    let parsed = parse_node_shim(shim_text, shim.parent().unwrap()).unwrap();
    assert_eq!(
        parsed.node_path.as_deref(),
        Some("/project/node_modules/.pnpm/node_modules")
    );
    let argv = strings(&["pnpm", "exec", "example-server", "--stdio"]);
    let resolution = resolve(&ResolveInput {
        path_env: Some(bin.as_os_str()),
        ..input(&argv, &project)
    });
    assert!(resolution.resolved, "{resolution:?}");
    assert_eq!(resolution.argv[0], node.to_string_lossy());
    assert_eq!(
        Path::new(&resolution.argv[1]).canonicalize().unwrap(),
        target.canonicalize().unwrap()
    );
    assert_eq!(resolution.argv[2], "--stdio");
    assert_eq!(
        resolution.node_path.as_deref(),
        Some("/project/node_modules/.pnpm/node_modules")
    );

    // Without node on the caller's PATH the shim is kept, not broken.
    let kept = resolve(&input(&argv, &project));
    assert!(kept.resolved);
    assert_eq!(kept.argv[0], shim.to_string_lossy());

    // Anything not exactly the known shape is kept as it is.
    assert!(
        parse_node_shim(
            "#!/bin/sh\nrm -rf /tmp/x\nexec node \"$basedir/x\" \"$@\"\n",
            &bin
        )
        .is_none()
    );
    assert!(
        parse_node_shim(
            "#!/bin/sh\nexec node --inspect \"$basedir/x\" \"$@\"\n",
            &bin
        )
        .is_none()
    );
}

#[test]
fn npm_exec_and_npx_resolve_an_installed_package_through_env_shebangs() {
    let (_dir, root) = canonical_tempdir();
    let bin = root.join("bin");
    let node = bin.join("node");
    write_executable(&node, "#!/bin/sh\n");
    let project = root.join("project");
    write(&project.join("package-lock.json"), "{}\n");
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
    for (argv, tail) in [
        (
            strings(&["npm", "exec", "@example/server", "--", "--port", "0"]),
            strings(&["--port", "0"]),
        ),
        (
            strings(&[
                "npm",
                "exec",
                "--yes",
                "--",
                "@example/server@1.2.3",
                "--port",
                "0",
            ]),
            strings(&["--port", "0"]),
        ),
        // After a leading `--`, everything is the server's, even a second `--`.
        (
            strings(&["npm", "exec", "--", "@example/server", "--", "--port"]),
            strings(&["--", "--port"]),
        ),
        // Positionals may precede the `--` that protects the options.
        (
            strings(&[
                "npm",
                "exec",
                "@example/server",
                "serve",
                "--",
                "--port",
                "0",
            ]),
            strings(&["serve", "--port", "0"]),
        ),
        (
            strings(&["npx", "-y", "@example/server@^1.2", "--port", "0"]),
            strings(&["--port", "0"]),
        ),
        (
            strings(&["env", "npx", "-y", "@example/server@1.2.3", "--port", "0"]),
            strings(&["--port", "0"]),
        ),
    ] {
        let resolution = resolve(&ResolveInput {
            path_env: Some(bin.as_os_str()),
            ..input(&argv, &project)
        });
        assert!(resolution.resolved, "{argv:?}: {resolution:?}");
        assert_eq!(resolution.method, Method::InstalledPackage);
        let mut expected = vec![node_shown.clone(), script.clone()];
        expected.extend(tail);
        assert_eq!(resolution.argv, expected, "{argv:?}");
        assert_eq!(resolution.declared, argv);
        assert_eq!(
            resolution.package.as_ref().unwrap().version.as_deref(),
            Some("1.2.3")
        );
    }
}

#[test]
fn a_bin_that_escapes_its_package_is_refused() {
    let (_dir, root) = canonical_tempdir();
    let project = root.join("project");
    write(&project.join("package-lock.json"), "{}\n");
    write_executable(&root.join("outside.sh"), "#!/bin/sh\n");
    write(
        &project.join("node_modules/example-server/package.json"),
        r#"{"name":"example-server","version":"1.0.0","bin":"../../../outside.sh"}"#,
    );
    let argv = strings(&["npx", "-y", "example-server@1.0.0"]);
    let resolution = resolve(&input(&argv, &project));
    assert_eq!(resolution.reason, Some("bin_outside_package"));
    assert_eq!(resolution.argv, argv);
}

#[test]
fn an_exact_version_that_is_not_installed_is_not_substituted() {
    let (_dir, root) = canonical_tempdir();
    let project = root.join("project");
    write(&project.join("package-lock.json"), "{}\n");
    let package = project.join("node_modules/example-server");
    write(
        &package.join("package.json"),
        r#"{"name":"example-server","version":"1.2.4","bin":"cli.js"}"#,
    );
    write_executable(&package.join("cli.js"), "#!/bin/sh\n");
    let argv = strings(&["npx", "-y", "example-server@1.2.3"]);
    let resolution = resolve(&input(&argv, &project));
    assert!(!resolution.resolved);
    assert_eq!(resolution.reason, Some("not_installed"));
    assert!(resolution.message.unwrap().contains("1.2.4"));
    assert_eq!(resolution.argv, argv);
}

fn npx_cache(root: &Path) -> PathBuf {
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
    cache
}

#[test]
fn an_unpinned_request_uses_the_npx_cache_only_when_allowed() {
    let (_dir, root) = canonical_tempdir();
    let cache = npx_cache(&root);
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let argv = strings(&["npx", "-y", "example-browser@latest", "--isolated"]);
    let refused = resolve(&ResolveInput {
        npm_cache: Some(&cache),
        ..input(&argv, &workdir)
    });
    assert!(!refused.resolved);
    assert_eq!(refused.reason, Some("unpinned"));
    assert!(refused.message.unwrap().contains("not resolved: unpinned"));
    assert_eq!(refused.argv, argv);

    let allowed = resolve(&ResolveInput {
        npm_cache: Some(&cache),
        allow_cached: true,
        ..input(&argv, &workdir)
    });
    assert!(allowed.resolved, "{allowed:?}");
    assert_eq!(allowed.method, Method::NpxCache);
    assert_eq!(
        allowed.package.as_ref().unwrap().version.as_deref(),
        Some("2.1.0")
    );
    assert!(allowed.argv[0].contains("bbbb"), "{:?}", allowed.argv);
    assert_eq!(allowed.argv[1], "--isolated");
    assert!(allowed.notes.iter().any(|note| note.contains("@latest")));

    // A pinned version may come from the cache without allow_cached.
    let pinned_argv = strings(&["npx", "-y", "example-browser@2.0.5"]);
    let pinned = resolve(&ResolveInput {
        npm_cache: Some(&cache),
        ..input(&pinned_argv, &workdir)
    });
    assert!(pinned.resolved);
    assert!(pinned.argv[0].contains("cccc"));
}

#[test]
fn resolution_never_downloads_and_keeps_the_declared_argv() {
    let (_dir, root) = canonical_tempdir();
    let cache = root.join("npm-cache");
    std::fs::create_dir_all(&cache).unwrap();
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let bin = root.join("bin");
    // Package managers on PATH that would record any execution.
    let marker = root.join("ran");
    for launcher in ["npx", "npm", "pnpm", "yarn", "bunx", "bun", "pnpx"] {
        write_executable(
            &bin.join(launcher),
            &format!("#!/bin/sh\ntouch {}\n", marker.display()),
        );
    }
    for (argv, reason) in [
        (
            strings(&["npx", "-y", "example-devtools@latest"]),
            "unpinned",
        ),
        (
            strings(&["npx", "-y", "example-devtools@1.0.0"]),
            "not_installed",
        ),
        (strings(&["npm", "exec", "example-devtools"]), "unpinned"),
        (
            strings(&["pnpm", "exec", "example-devtools"]),
            "not_installed",
        ),
        (
            strings(&["yarn", "dlx", "example-devtools"]),
            "unsupported_launcher_form",
        ),
        (
            strings(&["bunx", "example-devtools"]),
            "unsupported_launcher_form",
        ),
        (
            strings(&["bun", "x", "example-devtools"]),
            "unsupported_launcher_form",
        ),
        (
            strings(&["pnpx", "example-devtools"]),
            "unsupported_launcher_form",
        ),
        (
            strings(&["env", "npx", "-y", "example-devtools@latest"]),
            "unpinned",
        ),
        (
            strings(&["env", "TOKEN=x", "npx", "example-devtools"]),
            "unsupported_launcher_form",
        ),
    ] {
        let resolution = resolve(&ResolveInput {
            path_env: Some(bin.as_os_str()),
            npm_cache: Some(&cache),
            ..input(&argv, &workdir)
        });
        assert!(!resolution.resolved, "{argv:?}");
        assert_eq!(resolution.reason, Some(reason), "{argv:?}");
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
        let resolution = resolve(&input(&argv, &root));
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
        path_env: Some(bin.as_os_str()),
        ..input(&argv, &root)
    });
    assert!(resolution.resolved);
    assert_eq!(resolution.method, Method::Direct);
    assert_eq!(
        resolution.argv,
        vec![server.to_string_lossy().into_owned(), "-v".into()]
    );
    let missing = resolve(&input(&argv, &root));
    assert_eq!(missing.reason, Some("program_not_found"));
}

// ── configuration, trust and shadowing ─────────────────────────────────────

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
    let stable_cwd = format!("cwd = {:?}", stable.display().to_string());
    for (field, lines) in [
        ("cwd", format!("cwd = {:?}", worktree.display().to_string())),
        (
            "source",
            format!(
                "source = {:?}",
                worktree.join("server").display().to_string()
            ),
        ),
        // A relative argument that climbs into a worktree.
        (
            "argv",
            format!("{stable_cwd}\nargv = [\"/bin/sh\", \"../worktrees/session-1/server/run.sh\"]"),
        ),
        // `--option=/path`.
        (
            "argv",
            format!(
                "{stable_cwd}\nargv = [\"/bin/sh\", \"--config={}\"]",
                worktree.join("server/config.json").display()
            ),
        ),
    ] {
        let argv_line = if lines.contains("argv =") {
            String::new()
        } else {
            "argv = [\"/bin/sh\"]\n".to_string()
        };
        let text =
            format!("schema = 1\n[[servers]]\nname = \"example-server\"\n{argv_line}{lines}\n");
        let (entries, errors) = parse_and_validate(
            &text,
            Path::new("/host/mcp/services.toml"),
            Scope::Host,
            None,
            &checkouts,
        );
        assert!(entries.is_empty(), "{field}: {lines}");
        assert_eq!(errors.len(), 1, "{field}");
        assert_eq!(errors[0].code, "session_worktree_path", "{lines}");
        assert_eq!(errors[0].field, Some(field));
        assert!(
            errors[0].message.contains("cleaned up"),
            "{}",
            errors[0].message
        );
    }
    let text = format!(
        "schema = 1\n[[servers]]\nname = \"example-server\"\nargv = [\"/bin/sh\"]\n{stable_cwd}\n"
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

fn repository_anchor(root: &Path) -> RepositoryAnchor {
    RepositoryAnchor {
        main_root: root.to_path_buf(),
        identity: "example-0123".into(),
    }
}

#[test]
fn repository_entries_anchor_to_and_stay_inside_their_repository() {
    let (_dir, root) = canonical_tempdir();
    let repository = root.join("repository");
    std::fs::create_dir_all(repository.join("packages/server")).unwrap();
    write(&root.join("outside/secret.env"), "TOKEN=x\n");
    std::os::unix::fs::symlink(root.join("outside"), repository.join("linked")).unwrap();
    let anchor = repository_anchor(&repository);
    let file = repository.join(".aethyme/mcp.toml");
    let (entries, errors) = parse_and_validate(
        "schema = 1\n[[servers]]\nname = \"s\"\nargv = [\"node\", \"dist/server.js\"]\ncwd = \"packages/server\"\nsource = \".\"\nenv_file = \".env\"\n",
        &file,
        Scope::Repository,
        Some(&anchor),
        &NoCheckouts,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let entry = &entries[0];
    assert_eq!(
        entry.cwd.as_deref(),
        Some(repository.join("packages/server").as_path())
    );
    assert_eq!(
        entry.env_file.as_deref(),
        Some(repository.join("packages/server/.env").as_path())
    );
    assert!(!entry.trusted, "repository entries start untrusted");

    for (field, line) in [
        ("argv", "argv = [\"/bin/sh\"]"),
        (
            "env_file",
            "argv = [\"node\"]\nenv_file = \"../outside/secret.env\"",
        ),
        (
            "env_file",
            "argv = [\"node\"]\nenv_file = \"linked/secret.env\"",
        ),
        ("cwd", "argv = [\"node\"]\ncwd = \"/tmp\""),
        (
            "argv",
            "argv = [\"node\", \"--require=/opt/elsewhere/hook.js\"]",
        ),
    ] {
        let text = format!("schema = 1\n[[servers]]\nname = \"s\"\n{line}\n");
        let (entries, errors) =
            parse_and_validate(&text, &file, Scope::Repository, Some(&anchor), &NoCheckouts);
        assert!(entries.is_empty(), "{line}");
        assert_eq!(errors[0].code, "outside_repository", "{line}");
        assert_eq!(errors[0].field, Some(field), "{line}");
    }
}

#[test]
fn a_host_entry_always_wins_and_repository_entries_need_trust() {
    let (_dir, root) = canonical_tempdir();
    let host_file = root.join("host/mcp/services.toml");
    write(
        &host_file,
        "schema = 1\n[[servers]]\nname = \"shared-name\"\nargv = [\"/bin/sh\", \"host\"]\n",
    );
    let repository = root.join("repository");
    let repository_file = repository.join(".aethyme/mcp.toml");
    let repository_text = "schema = 1
[[servers]]
name = \"shared-name\"
argv = [\"node\", \"evil.js\"]
[[servers]]
name = \"repository-only\"
argv = [\"node\", \"server.js\"]
";
    write(&repository_file, repository_text);
    let digest = trust::file_digest(repository_text.as_bytes());
    let sources = |trusted: Vec<String>| Sources {
        host_file: Some(host_file.clone()),
        repository: Some((repository.clone(), repository_anchor(&repository))),
        trusted_digests: trusted,
    };

    let loaded = load(&sources(Vec::new()), &NoCheckouts, None);
    let shared = loaded.find("shared-name").unwrap();
    assert_eq!(shared.scope, Scope::Host);
    assert_eq!(shared.argv, strings(&["/bin/sh", "host"]));
    assert_eq!(loaded.shadowed, vec!["shared-name".to_string()]);
    let repository_only = loaded.find("repository-only").unwrap();
    assert!(!repository_only.trusted);
    assert!(!loaded.repository_trust.as_ref().unwrap().trusted);

    // Validating only one name still reports the shadowing for that name.
    let only = load(&sources(Vec::new()), &NoCheckouts, Some("shared-name"));
    assert_eq!(only.shadowed, vec!["shared-name".to_string()]);
    assert!(only.find("repository-only").is_none());

    let trusted = load(&sources(vec![digest.clone()]), &NoCheckouts, None);
    assert!(trusted.find("repository-only").unwrap().trusted);
    assert_eq!(trusted.find("shared-name").unwrap().scope, Scope::Host);

    // Any change to the file needs trust again.
    write(&repository_file, &format!("{repository_text}\n# edited\n"));
    let edited = load(&sources(vec![digest]), &NoCheckouts, None);
    assert!(!edited.find("repository-only").unwrap().trusted);
}

#[test]
fn trust_records_digests_per_repository() {
    let (_dir, root) = canonical_tempdir();
    let directory = root.join("mcp");
    assert!(trust::trusted_digests(&directory, "repository-a").is_empty());
    trust::record(
        &directory,
        "repository-a",
        "aaaa",
        Path::new("/r/.aethyme/mcp.toml"),
        "digest",
    )
    .unwrap();
    trust::record(
        &directory,
        "repository-a",
        "bbbb",
        Path::new("/r/.aethyme/mcp.toml"),
        "digest",
    )
    .unwrap();
    trust::record(
        &directory,
        "repository-a",
        "aaaa",
        Path::new("/r/.aethyme/mcp.toml"),
        "digest",
    )
    .unwrap();
    assert_eq!(
        trust::trusted_digests(&directory, "repository-a"),
        vec!["bbbb".to_string(), "aaaa".to_string()]
    );
    assert!(trust::trusted_digests(&directory, "repository-b").is_empty());
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

#[test]
fn keys_follow_passed_values_without_revealing_them() {
    let (_dir, root) = canonical_tempdir();
    let stable = root.join("project");
    write(&stable.join("src/main.txt"), "main\n");
    write(&stable.join("server.env"), "TOKEN=file-secret-value\n");
    let entry = host_entry(
        &keyed_entry(
            &stable,
            "env_file = \"server.env\"\nenv_allow = [\"API_TOKEN\"]",
        ),
        &NoCheckouts,
    );
    let with_token = |token: &str| {
        let mut caller = caller(&root, None);
        caller.env = os_pairs(&[("API_TOKEN", token), ("HOME", "/home/example")]);
        plan(&entry, &caller, &NoCheckouts).unwrap()
    };
    let first = with_token("one-secret-value");
    let second = with_token("two-secret-value");
    assert_ne!(
        first.key, second.key,
        "different credentials, different server"
    );
    assert_eq!(first.key, with_token("one-secret-value").key);
    // A shared server depends on every inherited value too (HOME selects
    // credential and configuration files).
    let mut other_home = caller(&root, None);
    other_home.env = os_pairs(&[("API_TOKEN", "one-secret-value"), ("HOME", "/elsewhere")]);
    assert_ne!(
        plan(&entry, &other_home, &NoCheckouts).unwrap().key,
        first.key
    );
    // A direct one only on env_allow and the env file.
    let mut direct = entry.clone();
    direct.mode = super::config::Mode::Direct;
    let mut home = caller(&root, None);
    home.env = os_pairs(&[("API_TOKEN", "one-secret-value"), ("HOME", "/home/example")]);
    assert_eq!(
        plan(&direct, &home, &NoCheckouts).unwrap().key,
        plan(&direct, &other_home, &NoCheckouts).unwrap().key
    );

    let shown = serde_json::to_string(&first).unwrap() + &format!("{first:?}");
    for secret_value in ["one-secret-value", "file-secret-value"] {
        assert!(!shown.contains(secret_value), "{secret_value} leaked");
    }
    let plain = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(b"TOKEN=file-secret-value\n")
    );
    assert_ne!(first.env_file_identity.as_deref(), Some(plain.as_str()));
    assert!(!shown.contains(&plain));
}

#[test]
fn build_output_packages_and_lockfiles_inside_the_copy_are_in_the_key() {
    let (_dir, root) = canonical_tempdir();
    let stable = root.join("project");
    write(&stable.join("src/main.txt"), "main\n");
    write_executable(&stable.join("dist/server.sh"), "#!/bin/sh\necho one\n");
    let checkouts = FakeCheckouts {
        stable: stable.clone(),
        worktrees: Vec::new(),
        common: stable.join(".git"),
    };
    let entry = host_entry(
        &format!(
            "schema = 1\n[[servers]]\nname = \"s\"\nmode = \"shared\"\nargv = [\"./dist/server.sh\"]\ncwd = {:?}\nsource = {:?}\n",
            stable.display().to_string(),
            stable.join("src").display().to_string()
        ),
        &checkouts,
    );
    let key = || {
        plan(&entry, &caller(&stable, None), &checkouts)
            .unwrap()
            .key
    };
    let base = key();
    write_executable(&stable.join("dist/server.sh"), "#!/bin/sh\necho two\n");
    let rebuilt = key();
    assert_ne!(base, rebuilt, "rebuilt output with the same source");
    write(&stable.join("pnpm-lock.yaml"), "lockfileVersion: 9\n");
    assert_ne!(rebuilt, key(), "a lockfile change is another server");
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

    write(&second.join("src/main.txt"), "branch\n");
    let same_branch = plan(&entry, &caller(&second, None), &checkouts).unwrap();
    assert_eq!(same_branch.copy.kind, CopyKind::Worktree);
    assert_eq!(same_branch.key, branch_plan.key);

    write(&second.join("src/main.txt"), "other branch\n");
    let other_branch = plan(&entry, &caller(&second, None), &checkouts).unwrap();
    assert_ne!(other_branch.key, branch_plan.key);
    assert_ne!(other_branch.key, stable_plan.key);
}

#[test]
fn a_dependency_only_branch_uses_its_own_copy() {
    let (_dir, stable, first, _second, checkouts) = branch_fixture();
    write(&stable.join("pnpm-lock.yaml"), "a: 1\n");
    write(&first.join("pnpm-lock.yaml"), "a: 2\n");
    let entry = host_entry(&keyed_entry(&stable, ""), &checkouts);
    let planned = plan(&entry, &caller(&first, None), &checkouts).unwrap();
    assert_eq!(planned.copy.kind, CopyKind::Worktree);
    assert_eq!(planned.copy.reason, "lockfile_differs");
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
fn an_identity_failure_never_blocks_direct_mode() {
    let (_dir, root) = canonical_tempdir();
    let stable = root.join("project");
    for index in 0..3 {
        write(&stable.join(format!("src/file{index}.txt")), "x\n");
    }
    let mut small = caller(&root, None);
    small.limits = Limits {
        max_files: 1,
        max_bytes: 1024,
    };
    let direct = host_entry(
        &keyed_entry(&stable, "").replace("mode = \"shared\"\n", ""),
        &NoCheckouts,
    );
    let planned = plan(&direct, &small, &NoCheckouts).unwrap();
    assert!(!planned.key_complete);
    assert!(planned.source_identity.is_none());
    assert!(
        planned
            .notes
            .iter()
            .any(|note| note.contains("direct mode"))
    );
    let shared = host_entry(&keyed_entry(&stable, ""), &NoCheckouts);
    assert_eq!(
        plan(&shared, &small, &NoCheckouts).unwrap_err().code,
        "source_identity_failed"
    );
}

// ── identities ─────────────────────────────────────────────────────────────

#[test]
fn git_source_identity_matches_identical_worktrees_and_sees_changes() {
    let (_dir, root) = canonical_tempdir();
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
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
    let base = tree_identity(&main_source, &DEFAULT_LIMITS).unwrap();
    assert_eq!(
        base,
        tree_identity(&worktree_source, &DEFAULT_LIMITS).unwrap()
    );

    write(&worktree.join("other.txt"), "changed outside\n");
    assert_eq!(
        base,
        tree_identity(&worktree_source, &DEFAULT_LIMITS).unwrap(),
        "outside source"
    );

    write(&worktree.join("src/index.txt"), "two\n");
    let modified = tree_identity(&worktree_source, &DEFAULT_LIMITS).unwrap();
    assert_ne!(base, modified);

    write(&worktree.join("src/index.txt"), "one\n");
    write(&worktree.join("src/new.txt"), "untracked\n");
    let untracked = tree_identity(&worktree_source, &DEFAULT_LIMITS).unwrap();
    assert_ne!(base, untracked);
    assert_ne!(modified, untracked);
}

#[test]
fn a_source_inside_a_large_outer_repository_is_capped_and_literal() {
    let (_dir, root) = canonical_tempdir();
    let outer = root.join("outer");
    std::fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-q", "-b", "main"]);
    write(&outer.join("tools/server/index.txt"), "server\n");
    write(&outer.join("a*/file.txt"), "starred\n");
    write(&outer.join("ab/file.txt"), "sibling\n");
    git(&outer, &["add", "-A"]);
    git(&outer, &["commit", "-qm", "init"]);
    for index in 0..30 {
        write(&outer.join(format!("big/untracked{index}.txt")), "noise\n");
    }
    let limits = Limits {
        max_files: 5,
        max_bytes: 1024 * 1024,
    };
    let source = outer.join("tools/server");
    let base = tree_identity(&source, &limits).expect("untracked files elsewhere are not listed");
    for index in 0..10 {
        write(&source.join(format!("new{index}.txt")), "x\n");
    }
    assert!(
        tree_identity(&source, &limits)
            .unwrap_err()
            .contains("narrower")
    );
    for index in 0..10 {
        std::fs::remove_file(source.join(format!("new{index}.txt"))).unwrap();
    }
    assert_eq!(tree_identity(&source, &limits).unwrap(), base);

    // `a*` is a literal directory name, not a glob that also matches `ab`.
    let starred = tree_identity(&outer.join("a*"), &limits).unwrap();
    write(&outer.join("ab/file.txt"), "changed sibling\n");
    assert_eq!(tree_identity(&outer.join("a*"), &limits).unwrap(), starred);
}

#[test]
fn the_file_walk_is_capped_and_git_identities_are_cached() {
    let (_dir, root) = canonical_tempdir();
    let plain = root.join("plain");
    for index in 0..4 {
        write(&plain.join(format!("f{index}")), "x\n");
    }
    let limits = Limits {
        max_files: 3,
        max_bytes: 1024,
    };
    assert!(tree_identity(&plain, &limits).is_err());
    assert!(tree_identity(&plain, &DEFAULT_LIMITS).is_ok());

    let repository = root.join("repository");
    std::fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "-q", "-b", "main"]);
    write(&repository.join("src/a.txt"), "a\n");
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "init"]);
    let cache = root.join("cache");
    let source = repository.join("src");
    let (first, hit) = tree_identity_cached(&source, &DEFAULT_LIMITS, Some(&cache)).unwrap();
    assert!(!hit);
    let (second, hit) = tree_identity_cached(&source, &DEFAULT_LIMITS, Some(&cache)).unwrap();
    assert!(hit);
    assert_eq!(first, second);
    // A commit moves HEAD: the cache entry no longer matches.
    write(&repository.join("src/b.txt"), "b\n");
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "more"]);
    let (third, hit) = tree_identity_cached(&source, &DEFAULT_LIMITS, Some(&cache)).unwrap();
    assert!(!hit);
    assert_ne!(third, first);
}

// ── connect ────────────────────────────────────────────────────────────────

#[test]
fn connect_arguments_parse_and_the_fallback_survives_usage_errors() {
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
    let fallback = parsed.fallback.unwrap();
    assert_eq!(fallback.cwd, Some(PathBuf::from("/srv/example")));
    assert_eq!(
        fallback.command,
        strings(&["npx", "-y", "example-server@latest", "--help"])
    );
    assert!(parse_args(&strings(&["a", "--", "x"])).is_err());
    assert!(parse_args(&strings(&["a", "--fallback"])).is_err());
    assert!(parse_args(&strings(&["--fallback", "--", "x"])).is_err());

    // A usage error still finds the fallback.
    let line = strings(&["a", "--bogus", "--fallback", "--", "/bin/true"]);
    assert!(parse_args(&line).is_err());
    assert_eq!(
        scan_fallback(&line).unwrap().command,
        strings(&["/bin/true"])
    );
    assert!(scan_fallback(&strings(&["a", "--", "/bin/true"])).is_none());
}

#[test]
fn the_server_environment_has_a_default_set_unless_none() {
    let caller_env = os_pairs(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/example"),
        ("LC_CTYPE", "C.UTF-8"),
        ("XDG_CACHE_HOME", "/home/example/.cache"),
        ("KEEP", "caller"),
        ("SECRET", "never"),
        ("NODE_PATH", "/caller/node_path"),
        ("API_URL", "https://example.invalid"),
    ]);
    let file = vec![
        ("TOKEN".to_string(), "from-file".to_string()),
        ("KEEP".to_string(), "overridden".to_string()),
    ];
    let allow = strings(&["KEEP", "API_URL", "ABSENT", "NODE_PATH"]);
    let mut environment = server_environment(
        EnvInherit::Default,
        &allow,
        &caller_env,
        &file,
        Some("/shim/node_path"),
    );
    environment.sort();
    assert_eq!(
        environment,
        os_pairs(&[
            ("API_URL", "https://example.invalid"),
            ("HOME", "/home/example"),
            ("KEEP", "overridden"),
            ("LC_CTYPE", "C.UTF-8"),
            ("NODE_PATH", "/shim/node_path:/caller/node_path"),
            ("PATH", "/usr/bin"),
            ("TOKEN", "from-file"),
            ("XDG_CACHE_HOME", "/home/example/.cache"),
        ])
    );
    let mut bare = server_environment(
        EnvInherit::None,
        &strings(&["KEEP"]),
        &caller_env,
        &[],
        None,
    );
    bare.sort();
    assert_eq!(bare, os_pairs(&[("KEEP", "caller")]));
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
        shadowed: false,
        fallback: false,
        warnings: Vec::new(),
    };
    telemetry::append(&root, &event("exec", None, 2)).unwrap();
    telemetry::append(&root, &event("fallback", Some("not_installed"), 4)).unwrap();
    telemetry::append(&root, &event("error", Some("not_installed"), 9)).unwrap();
    // A failed exec: an `exec` line followed by its correction.
    telemetry::append(&root, &event("exec", None, 1)).unwrap();
    telemetry::append(
        &root,
        &telemetry::Event {
            fallback: true,
            shadowed: true,
            ..event("exec_failed", Some("exec_failed"), 1)
        },
    )
    .unwrap();
    let summary = telemetry::summarize(&telemetry::read_events(&root));
    let counters = &summary["example-server"];
    assert_eq!(counters.events, 4);
    assert_eq!(counters.outcomes["exec"], 1);
    assert_eq!(counters.outcomes["exec_failed"], 1);
    assert_eq!(counters.outcomes["fallback"], 2);
    assert_eq!(counters.failure_reasons["not_installed"], 2);
    assert_eq!(counters.shadowed_repository_entries, 1);
    assert_eq!(counters.resolve_ms_max, Some(9));

    let script = root.join("server.sh");
    write_executable(&script, "#!/bin/sh\n");
    let argv = vec![script.to_string_lossy().into_owned()];
    let resolved = resolve(&input(&argv, &root));
    let identity = resolved.identity.unwrap();
    let first = telemetry::compare_and_remember(&root, "host-example", &argv, &identity);
    assert!(first.first_seen);
    let same = telemetry::compare_and_remember(&root, "host-example", &argv, &identity);
    assert!(!same.first_seen && same.changes.is_empty());
    write_executable(&script, "#!/bin/sh\necho a longer script\n");
    let changed = resolve(&input(&argv, &root)).identity.unwrap();
    let after = telemetry::compare_and_remember(&root, "host-example", &argv, &changed);
    assert_eq!(after.changes, vec!["content"]);
}

// ── re-review fixes ────────────────────────────────────────────────────────

#[test]
fn an_unstaged_worktree_edit_is_never_hidden_by_the_identity_cache() {
    let (_dir, root) = canonical_tempdir();
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    write(&main.join("src/index.txt"), "one\n");
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
    let checkouts = FakeCheckouts {
        stable: main.clone(),
        worktrees: vec![worktree.clone()],
        common: main.join(".git"),
    };
    let cache = root.join("cache");
    let direct = host_entry(
        &keyed_entry(&main, "").replace("mode = \"shared\"\n", ""),
        &checkouts,
    );
    let mut inside = caller(&worktree, None);
    inside.identity_cache = Some(cache.clone());
    assert_eq!(
        plan(&direct, &inside, &checkouts).unwrap().copy.reason,
        "source_identical"
    );
    // Same size, no index or directory change: only content differs.
    write(&worktree.join("src/index.txt"), "two\n");
    let edited = plan(&direct, &inside, &checkouts).unwrap();
    assert_eq!(edited.copy.kind, CopyKind::Worktree);
    assert_eq!(edited.copy.reason, "source_differs");

    // Shared mode never touches the cache.
    let shared_cache = root.join("shared-cache");
    let shared = host_entry(&keyed_entry(&main, ""), &checkouts);
    let mut shared_caller = caller(&main, None);
    shared_caller.identity_cache = Some(shared_cache.clone());
    plan(&shared, &shared_caller, &checkouts).unwrap();
    assert!(!shared_cache.exists());
}

#[test]
fn a_short_key_secret_is_an_error_not_a_silent_replacement() {
    let (_dir, root) = canonical_tempdir();
    let directory = root.join("mcp");
    write(&directory.join(secret::SECRET_FILE), "short");
    let error = secret::load_or_create(&directory).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(directory.join(secret::SECRET_FILE)).unwrap(),
        b"short"
    );
    // A fresh directory gets exactly one whole secret and no leftovers.
    let fresh = root.join("fresh");
    let first = secret::load_or_create(&fresh).unwrap();
    assert_eq!(secret::load_or_create(&fresh).unwrap(), first);
    let names: Vec<String> = std::fs::read_dir(&fresh)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![secret::SECRET_FILE.to_string()]);
}

#[test]
fn shim_targets_stay_inside_node_modules_and_both_branches_must_agree() {
    let (_dir, root) = canonical_tempdir();
    let bin_dir = root.join("project/node_modules/.bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write(&root.join("project/node_modules/pkg/cli.js"), "1\n");
    write(&root.join("project/outside.js"), "1\n");
    let shim = |local: &str, node: &str| {
        format!(
            "#!/bin/sh\nif [ -x \"$basedir/node\" ]; then\n  exec \"$basedir/node\"  \"$basedir/{local}\" \"$@\"\nelse\n  exec node  \"$basedir/{node}\" \"$@\"\nfi\n"
        )
    };
    assert!(parse_node_shim(&shim("../pkg/cli.js", "../pkg/cli.js"), &bin_dir).is_some());
    assert!(parse_node_shim(&shim("../../outside.js", "../../outside.js"), &bin_dir).is_none());
    assert!(
        parse_node_shim(
            &shim("../pkg/../../outside.js", "../pkg/../../outside.js"),
            &bin_dir
        )
        .is_none()
    );
    assert!(parse_node_shim(&shim("../pkg/cli.js", "../pkg/other.js"), &bin_dir).is_none());
    // A symlink that leaves node_modules is refused too.
    std::os::unix::fs::symlink(
        root.join("project/outside.js"),
        root.join("project/node_modules/pkg/link.js"),
    )
    .unwrap();
    assert!(parse_node_shim(&shim("../pkg/link.js", "../pkg/link.js"), &bin_dir).is_none());
}

#[test]
fn a_node_beside_the_shim_is_preferred_as_the_shim_does() {
    let (_dir, root) = canonical_tempdir();
    let project = root.join("project");
    write(&project.join("pnpm-lock.yaml"), "\n");
    write(&project.join("node_modules/pkg/cli.js"), "1\n");
    let bin_dir = project.join("node_modules/.bin");
    let local_node = bin_dir.join("node");
    write_executable(&local_node, "#!/bin/sh\n");
    let path_bin = root.join("bin");
    write_executable(&path_bin.join("node"), "#!/bin/sh\n");
    write_executable(
        &bin_dir.join("example-server"),
        "#!/bin/sh\nif [ -x \"$basedir/node\" ]; then\n  exec \"$basedir/node\"  \"$basedir/../pkg/cli.js\" \"$@\"\nelse\n  exec node  \"$basedir/../pkg/cli.js\" \"$@\"\nfi\n",
    );
    let argv = strings(&["pnpm", "exec", "example-server"]);
    let resolution = resolve(&ResolveInput {
        path_env: Some(path_bin.as_os_str()),
        ..input(&argv, &project)
    });
    assert!(resolution.resolved, "{resolution:?}");
    assert_eq!(resolution.argv[0], local_node.to_string_lossy());
}

#[test]
fn shared_keys_cover_the_build_output_directory_of_the_entry() {
    let (_dir, root) = canonical_tempdir();
    let stable = root.join("project");
    write(&stable.join("src/main.txt"), "main\n");
    write_executable(&stable.join("dist/server.sh"), "#!/bin/sh\n");
    write(&stable.join("dist/chunk.js"), "one\n");
    let checkouts = FakeCheckouts {
        stable: stable.clone(),
        worktrees: Vec::new(),
        common: stable.join(".git"),
    };
    let text = |mode: &str| {
        format!(
            "schema = 1\n[[servers]]\nname = \"s\"\nmode = \"{mode}\"\nargv = [\"./dist/server.sh\"]\ncwd = {:?}\nsource = {:?}\n",
            stable.display().to_string(),
            stable.join("src").display().to_string()
        )
    };
    let shared = host_entry(&text("shared"), &checkouts);
    let direct = host_entry(&text("direct"), &checkouts);
    let key = |entry: &Entry| plan(entry, &caller(&stable, None), &checkouts).unwrap().key;
    let (shared_before, direct_before) = (key(&shared), key(&direct));
    write(&stable.join("dist/chunk.js"), "two\n");
    assert_ne!(
        key(&shared),
        shared_before,
        "a rebuilt chunk is another shared server"
    );
    assert_eq!(key(&direct), direct_before);
    // Over the walk limits, a shared plan fails (and connect starts privately).
    let mut small = caller(&stable, None);
    small.limits = Limits {
        max_files: 1,
        max_bytes: 1024,
    };
    assert_eq!(
        plan(&shared, &small, &checkouts).unwrap_err().code,
        "entry_directory_identity_failed"
    );
}
