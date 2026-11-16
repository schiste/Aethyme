//! `aethyme mcp` through the built router.
//!
//! Every process gets its own host state directory and home, so no case reads
//! or writes a real agent configuration or the real `<host state>/mcp`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use aethyme_testkit::aethyme_bin;

struct Fixture {
    _dir: tempfile::TempDir,
    state: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let fixture = Self {
            state: root.join("state"),
            home: root.join("home"),
            work: root.join("work"),
            _dir: dir,
        };
        for path in [&fixture.state, &fixture.home, &fixture.work] {
            std::fs::create_dir_all(path).unwrap();
        }
        fixture
    }

    fn services(&self, body: &str) {
        let path = self.state.join("mcp/services.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("schema = 1\n{body}")).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(aethyme_bin());
        command
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("AETHYME_HOST_STATE_DIR", &self.state)
            .env_remove("AETHYME_REPO")
            .env_remove("npm_config_cache")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
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
fn connect_replaces_itself_with_the_server_and_passes_stdio_through() {
    let fixture = Fixture::new();
    let script = fixture.work.join("server.sh");
    write_executable(
        &script,
        "#!/bin/sh\necho \"$$\" > \"$1\"\n\
         echo \"server KEEP=${KEEP-unset} DROP=${DROP-unset} FILE=${FILE_VALUE-unset}\"\n",
    );
    std::fs::write(fixture.work.join("server.env"), "FILE_VALUE='from file'\n").unwrap();
    let pid_file = fixture.work.join("pid");
    fixture.services(&format!(
        "[[servers]]\nname = \"example-server\"\nargv = [{:?}, {:?}]\ncwd = {:?}\n\
         env_file = \"server.env\"\nenv_allow = [\"KEEP\"]\n",
        script.display().to_string(),
        pid_file.display().to_string(),
        fixture.work.display().to_string(),
    ));
    let child = fixture
        .command(&["mcp", "connect", "example-server"])
        .env("KEEP", "kept")
        .env("DROP", "dropped")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(&pid_file).unwrap().trim(),
        pid.to_string(),
        "the server must run in the process the agent spawned"
    );
    assert_eq!(
        stdout(&output),
        "server KEEP=kept DROP=unset FILE=from file\n"
    );
    assert!(
        stderr(&output).contains("aethyme mcp: using stable copy, key "),
        "{}",
        stderr(&output)
    );
    assert!(
        fixture.events().contains("\"outcome\":\"exec\""),
        "{}",
        fixture.events()
    );
}

#[test]
fn connect_runs_the_fallback_in_place_when_the_entry_is_missing() {
    let fixture = Fixture::new();
    fixture.services("");
    let child = fixture
        .command(&[
            "mcp",
            "connect",
            "missing-server",
            "--fallback",
            "--",
            "/bin/sh",
            "-c",
            "echo $$; echo fallback-ran",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), format!("{pid}\nfallback-ran\n"));
    assert!(
        stderr(&output).contains("no MCP entry named"),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("running the fallback command"));
    let events = fixture.events();
    assert!(events.contains("\"outcome\":\"fallback\""), "{events}");
    assert!(events.contains("\"reason\":\"unknown_entry\""), "{events}");

    let refused = fixture.run(&["mcp", "connect", "missing-server"]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(stdout(&refused).is_empty());
    assert!(stderr(&refused).contains("no MCP entry named"));
}

#[test]
fn resolve_finds_the_installed_bin_and_remembers_its_identity() {
    let fixture = Fixture::new();
    let shim = fixture.work.join("node_modules/.bin/example-server");
    write_executable(&shim, "#!/bin/sh\nexec true\n");
    fixture.services(&format!(
        "[[servers]]\nname = \"example-server\"\nmode = \"shared\"\n\
         argv = [\"pnpm\", \"exec\", \"example-server\", \"--stdio\"]\ncwd = {:?}\n",
        fixture.work.display().to_string()
    ));
    let output = fixture.run(&["mcp", "resolve", "example-server", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = json(&output);
    assert_eq!(value["schema"], "aethyme.mcp-resolve/v1");
    assert_eq!(value["resolution"]["resolved"], true);
    assert_eq!(value["resolution"]["method"], "node_modules_bin");
    assert_eq!(value["resolution"]["launcher"], "pnpm exec");
    assert_eq!(
        value["resolution"]["argv"],
        serde_json::json!([shim.display().to_string(), "--stdio"])
    );
    assert_eq!(value["copy"]["kind"], "stable");
    assert_eq!(value["key_short"].as_str().unwrap().len(), 12);
    assert_eq!(value["changed_since_last"]["first_seen"], true);
    assert!(value["phases_ms"]["resolve"].is_u64());

    let again = json(&fixture.run(&["mcp", "resolve", "example-server", "--json"]));
    assert_eq!(again["changed_since_last"]["first_seen"], false);
    assert_eq!(again["key"], value["key"]);

    let text = fixture.run(&["mcp", "resolve", "example-server"]);
    assert!(stdout(&text).contains("resolved: "), "{}", stdout(&text));
}

#[test]
fn resolve_without_an_installed_copy_keeps_the_declared_argv_and_downloads_nothing() {
    let fixture = Fixture::new();
    fixture.services(&format!(
        "[[servers]]\nname = \"example-devtools\"\nargv = [\"npx\", \"-y\", \"example-devtools@latest\"]\ncwd = {:?}\n",
        fixture.work.display().to_string()
    ));
    let output = fixture.run(&["mcp", "resolve", "example-devtools", "--json"]);
    assert_eq!(output.status.code(), Some(6), "{}", stderr(&output));
    let value = json(&output);
    assert_eq!(value["resolution"]["resolved"], false);
    assert_eq!(value["resolution"]["reason"], "not_installed");
    assert_eq!(
        value["resolution"]["argv"],
        serde_json::json!(["npx", "-y", "example-devtools@latest"])
    );
    assert!(
        !fixture.home.join(".npm").exists(),
        "the npm cache was touched"
    );
    assert!(!fixture.work.join("node_modules").exists());

    let connect = fixture.run(&["mcp", "connect", "example-devtools"]);
    assert_eq!(connect.status.code(), Some(6));
    assert!(
        stderr(&connect).contains("never installs or downloads"),
        "{}",
        stderr(&connect)
    );
    assert!(fixture.events().contains("\"reason\":\"not_installed\""));
}

#[test]
fn config_print_only_prints_snippets_for_each_agent() {
    let fixture = Fixture::new();
    fixture.services(
        "[[servers]]\nname = \"example-server\"\nargv = [\"npx\", \"-y\", \"example-server@latest\"]\n",
    );
    let before: Vec<_> = std::fs::read_dir(fixture.state.join("mcp"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    let codex = fixture.run(&["mcp", "config", "print", "--agent", "codex"]);
    assert!(codex.status.success(), "{}", stderr(&codex));
    let text = stdout(&codex);
    assert!(text.contains("[mcp_servers.example-server]"), "{text}");
    assert!(text.contains("\"connect\""), "{text}");
    assert!(stderr(&codex).contains("printed only"));
    let claude = fixture.run(&[
        "mcp",
        "config",
        "print",
        "--agent",
        "claude",
        "--name",
        "example-server",
    ]);
    assert!(claude.status.success(), "{}", stderr(&claude));
    let value = json(&claude);
    assert_eq!(value["mcpServers"]["example-server"]["command"], "aethyme");
    assert_eq!(
        value["mcpServers"]["example-server"]["args"],
        serde_json::json!([
            "mcp",
            "connect",
            "example-server",
            "--fallback",
            "--",
            "npx",
            "-y",
            "example-server@latest"
        ])
    );
    let after: Vec<_> = std::fs::read_dir(fixture.state.join("mcp"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(before, after, "config print wrote a file");
    assert!(std::fs::read_dir(&fixture.home).unwrap().next().is_none());

    let unknown = fixture.run(&[
        "mcp", "config", "print", "--agent", "codex", "--name", "nope",
    ]);
    assert_eq!(unknown.status.code(), Some(3));
}

#[test]
fn status_reports_entries_refusals_counters_and_no_hub() {
    let fixture = Fixture::new();
    let script = fixture.work.join("server.sh");
    write_executable(&script, "#!/bin/sh\n");
    fixture.services(&format!(
        "[[servers]]\nname = \"example-server\"\nargv = [{:?}]\n\n\
         [[servers]]\nname = \"relative\"\nargv = [\"x\"]\ncwd = \"not/absolute\"\n",
        script.display().to_string()
    ));
    assert!(
        fixture
            .run(&["mcp", "resolve", "example-server"])
            .status
            .success()
    );
    let output = fixture.run(&["mcp", "status", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = json(&output);
    assert_eq!(value["schema"], "aethyme.mcp-status/v1");
    assert_eq!(value["hub"]["state"], "not_available");
    let entries = value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "example-server");
    assert_eq!(entries[0]["plan"]["copy"]["kind"], "stable");
    assert_eq!(entries[0]["counters"]["outcomes"]["resolved"], 1);
    assert_eq!(value["errors"][0]["code"], "relative_path");
    assert_eq!(value["errors"][0]["entry"], "relative");
}
