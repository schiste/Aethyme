//! Managed MCP servers through the built router: `manage`, launcher shims,
//! `repair`, `bypass` and the session-start repair.
//!
//! Every process gets its own host state directory, home and `PATH`, so no
//! case reads or writes a real agent configuration, shell profile or the
//! real `<host state>/mcp`.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use aethyme_testkit::aethyme_bin;

const SECRET: &str = "sk-test-SECRET-value-4567";

/// The real launcher stand-in: prints its argv, the guard, and its stdin.
const FAKE_NPX: &str = "#!/bin/sh\nprintf 'ARGS:'\nfor a in \"$@\"; do printf '%s|' \"$a\"; done\n\
printf '\\nGUARD:%s\\n' \"${AETHYME_MCP_SHIM_GUARD-unset}\"\nif [ \"$1\" = stdin ]; then \
printf 'STDIN:'; cat; fi\nexit 7\n";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    home: PathBuf,
    work: PathBuf,
    real: PathBuf,
    shims: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let fixture = Self {
            state: root.join("state"),
            home: root.join("home"),
            work: root.join("work"),
            real: root.join("real-bin"),
            shims: root.join("shims"),
            root,
            _dir: dir,
        };
        for path in [&fixture.state, &fixture.home, &fixture.work, &fixture.real] {
            std::fs::create_dir_all(path).unwrap();
        }
        write_executable(&fixture.real.join("npx"), FAKE_NPX);
        fixture
    }

    fn services(&self, body: &str) {
        let path = self.state.join("mcp/services.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("schema = 1\n{body}")).unwrap();
    }

    fn managed_devtools(&self) {
        self.services(
            "# owner's comment\n[[servers]]\nname = \"example-devtools\"\n\
             argv = [\"npx\", \"-y\", \"example-devtools-mcp@latest\"]\nmanaged = true\n\
             match_packages = [\"example-devtools-mcp\"]\n",
        );
    }

    fn path(&self) -> String {
        format!(
            "{}:{}:/usr/bin:/bin",
            self.shims.display(),
            self.real.display()
        )
    }

    fn command_named(&self, program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("AETHYME_HOST_STATE_DIR", &self.state)
            .env("AETHYME_HOST_CACHE_DIR", self.root.join("cache"))
            .env("PATH", self.path())
            .env_remove("AETHYME_MCP_SHIM_GUARD")
            .env_remove("AETHYME_MCP_SHIM_DEPTH")
            .env_remove("AETHYME_REPO")
            .env_remove("npm_config_cache")
            .stdin(Stdio::null());
        command
    }

    fn aethyme(&self, args: &[&str]) -> Output {
        self.command_named(aethyme_bin().to_str().unwrap(), args)
            .output()
            .unwrap()
    }

    fn install_shims(&self) {
        let output = self.aethyme(&[
            "mcp",
            "intercept",
            "install",
            "--dir",
            self.shims.to_str().unwrap(),
        ]);
        assert!(output.status.success(), "{}", stderr(&output));
    }

    fn events(&self) -> String {
        std::fs::read_to_string(self.state.join("mcp/events.jsonl")).unwrap_or_default()
    }
}

fn write_executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}\n{}", stdout(output), stderr(output)))
}

#[test]
fn manage_add_list_and_remove_edit_only_the_host_file() {
    let fixture = Fixture::new();
    fixture.services("# keep this comment\n");
    let added = fixture.aethyme(&[
        "mcp",
        "manage",
        "add",
        "example-devtools",
        "--package",
        "example-devtools-mcp@latest",
        "--executable",
        "example-devtools-mcp",
        "--json",
    ]);
    assert!(added.status.success(), "{}", stderr(&added));
    let value = json(&added);
    assert_eq!(value["schema"], "aethyme.mcp-manage/v1");
    assert_eq!(value["created"], true);
    assert_eq!(
        value["entries"][0]["match_packages"][0],
        "example-devtools-mcp"
    );
    assert_eq!(value["interception"]["state"], "not_installed");
    let text = std::fs::read_to_string(fixture.state.join("mcp/services.toml")).unwrap();
    assert!(
        text.starts_with("schema = 1\n# keep this comment\n"),
        "{text}"
    );
    assert!(text.contains("managed = true"), "{text}");

    let listed = json(&fixture.aethyme(&["mcp", "manage", "list", "--json"]));
    assert_eq!(listed["entries"][0]["name"], "example-devtools");
    assert_eq!(listed["entries"][0]["repair"], "auto");

    let removed = fixture.aethyme(&["mcp", "manage", "remove", "example-devtools", "--json"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert_eq!(json(&removed)["entries"], serde_json::json!([]));
    let text = std::fs::read_to_string(fixture.state.join("mcp/services.toml")).unwrap();
    assert!(text.contains("name = \"example-devtools\""));
    assert!(!text.contains("managed"));

    let refused = fixture.aethyme(&["mcp", "manage", "add", "x", "--executable", "npx"]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
}

#[test]
fn intercept_install_status_and_uninstall() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    let installed = fixture.aethyme(&[
        "mcp",
        "intercept",
        "install",
        "--dir",
        fixture.shims.to_str().unwrap(),
    ]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    assert!(
        stdout(&installed).contains("export PATH=\""),
        "{}",
        stdout(&installed)
    );
    for name in ["npx", "npm", "pnpm", "pnpx", "bunx", "yarn"] {
        assert!(
            std::fs::symlink_metadata(fixture.shims.join(name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let status = json(&fixture.aethyme(&["mcp", "intercept", "status", "--json"]));
    assert_eq!(status["schema"], "aethyme.mcp-intercept/v1");
    assert_eq!(status["status"]["complete"], true, "{status}");
    assert_eq!(status["status"]["on_path"], true);
    assert_eq!(status["status"]["ahead_of_real_tools"], true);

    let mcp_status = json(&fixture.aethyme(&["mcp", "status", "--json"]));
    assert_eq!(
        mcp_status["managed"]["entries"][0]["name"],
        "example-devtools"
    );
    assert_eq!(mcp_status["managed"]["interception"]["complete"], true);
    assert_eq!(mcp_status["managed"]["repair_on_session_start"], false);

    let removed = fixture.aethyme(&["mcp", "intercept", "uninstall", "--json"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(!fixture.shims.exists());
    assert!(!fixture.state.join("mcp/intercept.match").exists());
}

#[test]
fn a_non_matching_launcher_call_passes_through_untouched() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    let mut child = fixture
        .command_named("npx", &["stdin", "a b", "", "-y"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        stdout(&output),
        "ARGS:stdin|a b||-y|\nGUARD:unset\nSTDIN:hello"
    );
    assert_eq!(stderr(&output), "");
    assert!(
        !fixture.events().contains("intercept"),
        "{}",
        fixture.events()
    );
}

#[test]
fn a_matching_launcher_call_lands_in_connect_with_the_real_launcher_as_fallback() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    let output = fixture
        .command_named("npx", &["-y", "example-devtools-mcp@latest", "--flag"])
        .output()
        .unwrap();
    // Not installed offline: connect runs the fallback, the real npx, with
    // the original arguments and the loop guard set.
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "ARGS:-y|example-devtools-mcp@latest|--flag|\nGUARD:1\n"
    );
    assert!(
        stderr(&output).contains("aethyme mcp: example-devtools: running the fallback command"),
        "{}",
        stderr(&output)
    );
    let events = fixture.events();
    assert!(events.contains("\"command\":\"intercept\""), "{events}");
    assert!(events.contains("\"outcome\":\"handoff\""), "{events}");
    assert!(events.contains("\"command\":\"connect\""), "{events}");
    assert!(events.contains("\"outcome\":\"fallback\""), "{events}");
}

#[test]
fn the_guard_makes_a_shim_pass_through() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    let output = fixture
        .command_named("npx", &["-y", "example-devtools-mcp"])
        .env("AETHYME_MCP_SHIM_GUARD", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(stderr(&output), "");
    assert!(!fixture.events().contains("handoff"));
}

#[test]
fn a_missing_real_tool_exits_127() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    let output = fixture
        .command_named("pnpm", &["install"])
        .env("PATH", &fixture.shims)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127), "{}", stderr(&output));
    assert!(stderr(&output).contains("pnpm: command not found"));
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_corrupt_match_file_never_blocks_a_launcher() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    std::fs::write(fixture.state.join("mcp/intercept.match"), "garbage").unwrap();
    let output = fixture
        .command_named("npx", &["-y", "example-devtools-mcp"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        stdout(&output),
        "ARGS:-y|example-devtools-mcp|\nGUARD:unset\n"
    );
    assert_eq!(stderr(&output), "");
    assert!(fixture.events().contains("match_file_error"));
}

#[test]
fn a_direct_executable_shim_starts_the_managed_entry_through_connect() {
    let fixture = Fixture::new();
    fixture.services(
        "[[servers]]\nname = \"example-devtools\"\nargv = [\"example-devtools-mcp\", \"--stdio\"]\n\
         managed = true\nmatch_executables = [\"example-devtools-mcp\"]\n",
    );
    write_executable(
        &fixture.real.join("example-devtools-mcp"),
        "#!/bin/sh\necho \"SERVER:$* GUARD:${AETHYME_MCP_SHIM_GUARD-unset}\"\n",
    );
    fixture.install_shims();
    assert!(fixture.shims.join("example-devtools-mcp").exists());
    let output = fixture
        .command_named("example-devtools-mcp", &["--flag"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    // The entry's argv, then the caller's arguments.
    assert_eq!(stdout(&output), "SERVER:--stdio --flag GUARD:unset\n");
    assert!(
        stderr(&output).contains("aethyme mcp: using stable copy"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_shim_directory_twice_or_through_a_symlink_still_finds_the_real_tool() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    let alias = fixture.root.join("alias");
    std::os::unix::fs::symlink(&fixture.shims, &alias).unwrap();
    let path = format!(
        "{}:{}:{}:{}:/usr/bin:/bin",
        fixture.shims.display(),
        alias.display(),
        fixture.shims.display(),
        fixture.real.display()
    );
    let output = fixture
        .command_named("npx", &["other"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    assert_eq!(stdout(&output), "ARGS:other|\nGUARD:unset\n");
}

fn agent_configs(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let codex = fixture.home.join(".codex/config.toml");
    std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
    std::fs::write(
        &codex,
        format!(
            "[mcp_servers.devtools]\ncommand = \"npx\"\nargs = [\"-y\", \"example-devtools-mcp\"]\n\n\
             [mcp_servers.devtools.env]\nTOKEN = \"{SECRET}\"\n"
        ),
    )
    .unwrap();
    let claude = fixture.home.join(".claude.json");
    std::fs::write(
        &claude,
        format!(
            "{{\n  \"mcpServers\": {{\n    \"devtools\": {{\n      \"command\": \"npx\",\n      \
             \"args\": [\"example-devtools-mcp\"],\n      \"env\": {{\"TOKEN\": \"{SECRET}\"}}\n    }}\n  }}\n}}\n"
        ),
    )
    .unwrap();
    (codex, claude)
}

#[test]
fn repair_checks_applies_with_backups_and_never_prints_values() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    let (codex, claude) = agent_configs(&fixture);
    let checked = fixture.aethyme(&["mcp", "repair", "--json"]);
    assert!(checked.status.success(), "{}", stderr(&checked));
    let value = json(&checked);
    assert_eq!(value["schema"], "aethyme.mcp-repair/v1");
    assert_eq!(value["report"]["matched"], 2);
    assert_eq!(value["report"]["repaired"], 0);
    assert!(!stdout(&checked).contains(SECRET));
    let text = fixture.aethyme(&["mcp", "repair"]);
    assert!(stdout(&text).contains("would_repair"), "{}", stdout(&text));
    assert!(!stdout(&text).contains(SECRET));

    let applied = fixture.aethyme(&["mcp", "repair", "--apply", "--json"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert_eq!(json(&applied)["report"]["repaired"], 2);
    assert!(!stdout(&applied).contains(SECRET));
    assert!(!fixture.events().contains(SECRET));
    let codex_text = std::fs::read_to_string(&codex).unwrap();
    let written: toml_edit::DocumentMut = codex_text.parse().unwrap();
    assert_same_binary(
        written["mcp_servers"]["devtools"]["command"]
            .as_str()
            .unwrap(),
    );
    assert!(codex_text.contains(&format!("TOKEN = \"{SECRET}\"")));
    let claude_value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
    assert_same_binary(
        claude_value["mcpServers"]["devtools"]["command"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(
        claude_value["mcpServers"]["devtools"]["env"]["TOKEN"],
        SECRET
    );
    let backups = std::fs::read_dir(fixture.state.join("mcp/repair-backups"))
        .unwrap()
        .count();
    assert_eq!(backups, 2);

    // Idempotent, and `--auto` is silent on a no-op.
    let again = fixture.aethyme(&["mcp", "repair", "--apply", "--auto"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(stdout(&again), "");
}

/// A repaired entry runs this very `aethyme`, by absolute path.
fn assert_same_binary(command: &str) {
    assert!(Path::new(command).is_absolute(), "{command}");
    assert_eq!(
        Path::new(command).canonicalize().unwrap(),
        aethyme_bin().canonicalize().unwrap()
    );
}

fn session_start(fixture: &Fixture) -> Output {
    let mut child = fixture
        .command_named(aethyme_bin().to_str().unwrap(), &["hook", "SessionStart"])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", fixture.root.join("no-release").display()),
        )
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn the_session_start_hook_repairs_only_when_the_owner_turned_it_on() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    let (codex, claude) = agent_configs(&fixture);
    let before = std::fs::read_to_string(&claude).unwrap();
    let codex_before = std::fs::read_to_string(&codex).unwrap();
    let off = session_start(&fixture);
    assert!(off.status.success());
    assert!(!stdout(&off).contains("repaired"), "{}", stdout(&off));
    assert_eq!(std::fs::read_to_string(&claude).unwrap(), before);
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), codex_before);

    std::fs::write(
        fixture.state.join("mcp/settings.toml"),
        "[mcp]\nrepair_on_session_start = true\n",
    )
    .unwrap();
    let on = session_start(&fixture);
    assert!(on.status.success());
    assert!(
        stdout(&on).contains("repaired 1 managed MCP server entry"),
        "{}",
        stdout(&on)
    );
    // Only the TOML file: the JSON user configuration is left to an
    // explicit `repair --apply`.
    assert!(
        std::fs::read_to_string(&codex)
            .unwrap()
            .contains("\"connect\"")
    );
    assert_eq!(std::fs::read_to_string(&claude).unwrap(), before);
    let quiet = session_start(&fixture);
    assert!(!stdout(&quiet).contains("repaired"), "{}", stdout(&quiet));
}

#[test]
fn bypass_reports_counts_or_inconclusive_never_values() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    let output = fixture.aethyme(&["mcp", "bypass", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = json(&output);
    assert_eq!(value["schema"], "aethyme.mcp-bypass/v1");
    let state = value["report"]["state"].as_str().unwrap();
    assert!(state == "complete" || state == "inconclusive", "{value}");
    if state == "complete" {
        assert_eq!(value["report"]["entries"][0]["name"], "example-devtools");
    } else {
        assert!(value["report"]["reason"].is_string());
    }
}

#[test]
fn a_pinned_entry_leaves_a_caller_with_other_arguments_alone() {
    let fixture = Fixture::new();
    fixture.services(
        "[[servers]]\nname = \"example-devtools\"\n\
         argv = [\"npx\", \"-y\", \"example-devtools-mcp\", \"--headless\"]\nmanaged = true\n\
         args = \"pinned\"\nmatch_packages = [\"example-devtools-mcp\"]\n",
    );
    fixture.install_shims();
    let output = fixture
        .command_named("npx", &["-y", "example-devtools-mcp", "--port", "9"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        stdout(&output),
        "ARGS:-y|example-devtools-mcp|--port|9|\nGUARD:unset\n"
    );
    assert!(
        stderr(&output).contains("arguments_differ"),
        "{}",
        stderr(&output)
    );
    assert!(!fixture.events().contains("\"outcome\":\"handoff\""));
}

#[test]
fn help_and_version_calls_are_never_handed_off() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    for flag in ["--help", "--version"] {
        let output = fixture
            .command_named("npx", &["example-devtools-mcp", flag])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(
            stdout(&output),
            format!("ARGS:example-devtools-mcp|{flag}|\nGUARD:unset\n")
        );
        assert_eq!(stderr(&output), "");
    }
    assert!(!fixture.events().contains("handoff"));
}

#[test]
fn a_chain_of_shims_stops_at_the_hop_limit() {
    let fixture = Fixture::new();
    fixture.managed_devtools();
    fixture.install_shims();
    let output = fixture
        .command_named("npx", &["other"])
        .env("AETHYME_MCP_SHIM_HOPS", "4")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127));
    assert!(
        stderr(&output).contains("aethyme mcp intercept status"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "");
    // One hop below the limit still reaches the real tool, and the real
    // tool sees the counter incremented.
    let output = fixture
        .command_named("npx", &["other"])
        .env("AETHYME_MCP_SHIM_HOPS", "3")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
}
