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
    assert_eq!(value["resolution"]["reason"], "unpinned");
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
        stderr(&connect).contains("not resolved: unpinned"),
        "{}",
        stderr(&connect)
    );
    assert!(fixture.events().contains("\"reason\":\"unpinned\""));
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
fn status_reports_entries_refusals_counters_and_a_stopped_hub() {
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
    assert_eq!(value["hub"]["state"], "not_running");
    let entries = value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "example-server");
    assert_eq!(entries[0]["plan"]["copy"]["kind"], "stable");
    assert_eq!(entries[0]["counters"]["outcomes"]["resolved"], 1);
    assert_eq!(value["errors"][0]["code"], "relative_path");
    assert_eq!(value["errors"][0]["entry"], "relative");
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

const PNPM_SHIM: &str = r#"#!/bin/sh
basedir=$(dirname "$(echo "$0" | sed -e 's,\\,/,g')")

case `uname` in
    *CYGWIN*) basedir=`cygpath -w "$basedir"`;;
esac

if [ -z "$NODE_PATH" ]; then
  export NODE_PATH="/example/node_modules/.pnpm/node_modules"
else
  export NODE_PATH="/example/node_modules/.pnpm/node_modules:$NODE_PATH"
fi
if [ -x "$basedir/node" ]; then
  exec "$basedir/node"  "$basedir/../example-server/dist/cli.js" "$@"
else
  exec node  "$basedir/../example-server/dist/cli.js" "$@"
fi
"#;

#[test]
fn a_pnpm_shim_starts_with_node_from_the_callers_path_without_env_allow() {
    let fixture = Fixture::new();
    let bin = fixture.work.join("fake-bin");
    let pid_file = fixture.work.join("pid");
    // A stand-in for node in a directory only the caller's PATH names.
    write_executable(
        &bin.join("node"),
        &format!(
            "#!/bin/sh\necho \"$$\" > {}\necho \"node-ran $1 $2 NODE_PATH=$NODE_PATH\"\n",
            pid_file.display()
        ),
    );
    let target = fixture.work.join("node_modules/example-server/dist/cli.js");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "console.log(1)\n").unwrap();
    write_executable(
        &fixture.work.join("node_modules/.bin/example-server"),
        PNPM_SHIM,
    );
    fixture.services(&format!(
        "[[servers]]\nname = \"example-server\"\nargv = [\"pnpm\", \"exec\", \"example-server\", \"--stdio\"]\ncwd = {:?}\n",
        fixture.work.display().to_string()
    ));
    let child = fixture
        .command(&["mcp", "connect", "example-server"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("NODE_PATH")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let line = stdout(&output);
    assert!(line.starts_with("node-ran "), "{line}");
    assert!(
        line.contains("example-server/dist/cli.js --stdio"),
        "{line}"
    );
    assert!(
        line.contains("NODE_PATH=/example/node_modules/.pnpm/node_modules"),
        "{line}"
    );
    assert_eq!(
        std::fs::read_to_string(&pid_file).unwrap().trim(),
        pid.to_string()
    );
}

#[test]
fn a_failed_preflight_runs_the_fallback_while_it_still_can() {
    let fixture = Fixture::new();
    // A shim this build does not read; with env_inherit = "none" its `node`
    // cannot be found, which must be caught before the exec.
    write_executable(
        &fixture.work.join("node_modules/.bin/example-server"),
        "#!/bin/sh\nset -e\nexec node \"$0.js\" \"$@\"\n",
    );
    fixture.services(&format!(
        "[[servers]]\nname = \"example-server\"\nargv = [\"pnpm\", \"exec\", \"example-server\"]\ncwd = {:?}\nenv_inherit = \"none\"\n",
        fixture.work.display().to_string()
    ));
    let output = fixture.run(&[
        "mcp",
        "connect",
        "example-server",
        "--fallback",
        "--",
        "/bin/sh",
        "-c",
        "echo fallback-ran",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "fallback-ran\n");
    assert!(
        stderr(&output).contains("not on the server's PATH"),
        "{}",
        stderr(&output)
    );
    assert!(fixture.events().contains("\"reason\":\"preflight_failed\""));
}

#[test]
fn the_fallback_runs_after_a_usage_error_or_a_panic() {
    let fixture = Fixture::new();
    fixture.services("");
    let usage = fixture.run(&[
        "mcp",
        "connect",
        "example-server",
        "--bogus",
        "--fallback",
        "--",
        "/bin/sh",
        "-c",
        "echo fallback-ran",
    ]);
    assert!(usage.status.success(), "{}", stderr(&usage));
    assert_eq!(stdout(&usage), "fallback-ran\n");
    assert!(stderr(&usage).contains("unknown option --bogus"));

    let panicked = fixture
        .command(&[
            "mcp",
            "connect",
            "example-server",
            "--fallback",
            "--",
            "/bin/sh",
            "-c",
            "echo fallback-ran",
        ])
        .env("AETHYME_TEST_MCP_CONNECT_PANIC", "1")
        .env("CARGO_MANIFEST_DIR", env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(panicked.status.success(), "{}", stderr(&panicked));
    assert_eq!(stdout(&panicked), "fallback-ran\n");
    assert!(
        stderr(&panicked).contains("internal error"),
        "{}",
        stderr(&panicked)
    );
    let events = fixture.events();
    assert!(events.contains("\"reason\":\"usage\""), "{events}");
    assert!(events.contains("\"reason\":\"panic\""), "{events}");
}

#[test]
fn repository_entries_need_trust_and_never_shadow_a_host_entry() {
    let fixture = Fixture::new();
    git(&fixture.work, &["init", "-q", "-b", "main"]);
    write_executable(
        &fixture.work.join("repository.sh"),
        "#!/bin/sh\necho repository-ran\n",
    );
    write_executable(&fixture.work.join("host.sh"), "#!/bin/sh\necho host-ran\n");
    let repository_file = fixture.work.join(".aethyme/mcp.toml");
    std::fs::create_dir_all(repository_file.parent().unwrap()).unwrap();
    let declared = "schema = 1
[[servers]]
name = \"repository-server\"
argv = [\"./repository.sh\"]
cwd = \".\"

[[servers]]
name = \"shared-name\"
argv = [\"./repository.sh\"]
cwd = \".\"
";
    std::fs::write(&repository_file, declared).unwrap();
    fixture.services(&format!(
        "[[servers]]\nname = \"shared-name\"\nargv = [{:?}]\n",
        fixture.work.join("host.sh").display().to_string()
    ));

    let refused = fixture.run(&["mcp", "connect", "repository-server"]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("aethyme mcp trust"),
        "{}",
        stderr(&refused)
    );

    // The host entry wins even before any trust, with a note.
    let shared = fixture.run(&["mcp", "connect", "shared-name"]);
    assert!(shared.status.success(), "{}", stderr(&shared));
    assert_eq!(stdout(&shared), "host-ran\n");
    assert!(stderr(&shared).contains("is ignored and the host entry is used"));

    // Without a terminal, trust shows the entries and records nothing.
    let shown = fixture.run(&["mcp", "trust", "--json"]);
    assert_eq!(shown.status.code(), Some(3), "{}", stderr(&shown));
    let value = json(&shown);
    assert_eq!(value["schema"], "aethyme.mcp-trust/v1");
    assert_eq!(value["trusted"], false);
    assert_eq!(value["reason"], "confirmation_required");
    assert_eq!(value["entries"][0]["name"], "repository-server");
    assert_eq!(
        value["entries"][0]["argv"],
        serde_json::json!(["./repository.sh"])
    );
    let digest = value["sha256"].as_str().unwrap().to_string();
    assert!(!fixture.state.join("mcp/trusted-repositories.json").exists());
    let still_refused = fixture.run(&["mcp", "connect", "repository-server"]);
    assert_eq!(still_refused.status.code(), Some(3));

    let wrong = fixture.run(&["mcp", "trust", "--yes", "--digest", "0000", "--json"]);
    assert_eq!(wrong.status.code(), Some(3));
    assert_eq!(json(&wrong)["reason"], "digest_mismatch");
    let missing = fixture.run(&["mcp", "trust", "--yes"]);
    assert_eq!(missing.status.code(), Some(2));

    let trusted = fixture.run(&["mcp", "trust", "--yes", "--digest", &digest, "--json"]);
    assert!(trusted.status.success(), "{}", stderr(&trusted));
    let value = json(&trusted);
    assert_eq!(value["trusted"], true);
    assert_eq!(value["approval"], "digest");
    let record =
        std::fs::read_to_string(fixture.state.join("mcp/trusted-repositories.json")).unwrap();
    assert!(record.contains("\"approval\": \"digest\""), "{record}");

    let ran = fixture.run(&["mcp", "connect", "repository-server"]);
    assert!(ran.status.success(), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "repository-ran\n");
    let shared = fixture.run(&["mcp", "connect", "shared-name"]);
    assert_eq!(stdout(&shared), "host-ran\n");

    // Any change needs trust again.
    std::fs::write(&repository_file, format!("{declared}# edited\n")).unwrap();
    let edited = fixture.run(&["mcp", "connect", "repository-server"]);
    assert_eq!(edited.status.code(), Some(3), "{}", stderr(&edited));

    let status = json(&fixture.run(&["mcp", "status", "--json"]));
    assert_eq!(status["shadowed"], serde_json::json!(["shared-name"]));
    assert_eq!(status["repository_trust"]["trusted"], false);
}

const HUB_FAKE_SERVER: &str = r#"#!/bin/sh
printf 'started %s\n' "$$" >> "$FAKE_LOG"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  case "$method" in
    initialize)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"capabilities":{"tools":{}},"protocolVersion":"2025-06-18","serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
    tools/call)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"%s"}}\n' "$id" "$$" ;;
  esac
done
"#;

/// One agent session through `connect`: initialize, one call, then leave.
fn hub_session(fixture: &Fixture) -> (String, String) {
    session(fixture, "example-shared", "2025-06-18")
}

/// One agent session for `name`, initializing with `protocol`.
fn session(fixture: &Fixture, name: &str, protocol: &str) -> (String, String) {
    use std::io::{BufRead, Write};
    let mut child = fixture
        .command(&["mcp", "connect", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": protocol, "capabilities": {},
                       "clientInfo": {"name": "t", "version": "1"}}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "pid", "arguments": {}}}),
    );
    stdin.write_all(input.as_bytes()).unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut pid = String::new();
    while let Ok(line) = receiver.recv_timeout(std::time::Duration::from_secs(30)) {
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value["id"] == 2 {
            pid = value["result"]["text"].as_str().unwrap().to_string();
            break;
        }
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    (pid, String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn shared_connects_reach_one_upstream_through_the_hub() {
    let fixture = Fixture::new();
    let script = fixture.work.join("fake-server.sh");
    write_executable(&script, HUB_FAKE_SERVER);
    let log = fixture.work.join("fake.log");
    std::fs::write(
        fixture.work.join("server.env"),
        format!("FAKE_LOG={}\n", log.display()),
    )
    .unwrap();
    fixture.services(&format!(
        "[[servers]]\nname = \"example-shared\"\nmode = \"shared\"\nargv = [{:?}]\ncwd = {:?}\nenv_file = \"server.env\"\n",
        script.display().to_string(),
        fixture.work.display().to_string(),
    ));
    let (first, first_stderr) = hub_session(&fixture);
    let (second, _) = hub_session(&fixture);
    assert!(!first.is_empty(), "{first_stderr}");
    assert_eq!(first, second, "both sessions must reach the same upstream");
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .matches("started ")
            .count(),
        1
    );
    assert!(first_stderr.contains("using stable copy"), "{first_stderr}");

    let hub_dir = fixture.state.join("mcp/hub");
    let pids: Vec<String> = std::fs::read_dir(&hub_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "pid"))
        .map(|path| std::fs::read_to_string(path).unwrap().trim().to_string())
        .collect();
    assert_eq!(pids.len(), 1, "one hub per build");
    let status = json(&fixture.run(&["mcp", "status", "--json"]));
    assert_eq!(status["hub"]["state"], "running");
    assert_eq!(
        status["entries"][0]["hub"]["upstream"]["pid"].to_string(),
        first
    );
    // Stop this test's own hub; its upstream ends with its input.
    let killed = Command::new("kill").arg(&pids[0]).status().unwrap();
    assert!(killed.success());
}

fn shared_fixture(extra: &str) -> (Fixture, std::path::PathBuf) {
    let fixture = Fixture::new();
    let script = fixture.work.join("fake-server.sh");
    write_executable(&script, HUB_FAKE_SERVER);
    let log = fixture.work.join("fake.log");
    std::fs::write(
        fixture.work.join("server.env"),
        format!("FAKE_LOG={}\n", log.display()),
    )
    .unwrap();
    fixture.services(&format!(
        "[[servers]]\nname = \"example-shared\"\nmode = \"shared\"\nargv = [{:?}]\ncwd = {:?}\nenv_file = \"server.env\"\n{extra}\n",
        script.display().to_string(),
        fixture.work.display().to_string(),
    ));
    (fixture, log)
}

fn hub_pids(fixture: &Fixture) -> Vec<String> {
    std::fs::read_dir(fixture.state.join("mcp/hub"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "pid"))
                .map(|path| std::fs::read_to_string(path).unwrap().trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_client_the_hub_cannot_serve_gets_a_private_server_with_its_initialize_replayed() {
    let (fixture, log) = shared_fixture("");
    let (shared, _) = session(&fixture, "example-shared", "2025-06-18");
    assert!(!shared.is_empty());
    // Another protocol version: the hub refuses after the initialize was read.
    let (private, stderr) = session(&fixture, "example-shared", "2024-11-05");
    assert!(!private.is_empty(), "{stderr}");
    assert_ne!(private, shared, "{stderr}");
    assert!(stderr.contains("protocol_version"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .matches("started ")
            .count(),
        2
    );
    for pid in hub_pids(&fixture) {
        Command::new("kill").arg(pid).status().unwrap();
    }
}

#[test]
fn an_entry_requiring_client_features_is_never_shared() {
    let (fixture, _) = shared_fixture("require_client_features = true");
    let (pid, stderr) = session(&fixture, "example-shared", "2025-06-18");
    assert!(!pid.is_empty(), "{stderr}");
    assert!(stderr.contains("require_client_features"), "{stderr}");
    assert!(hub_pids(&fixture).is_empty(), "no hub may start");
}

/// Not a test of its own: the fake streamable-HTTP server the HTTP cases
/// start by running this test binary with `--exact` on this name.
#[test]
fn fake_http_server_process() {
    aethyme_testkit::mcp_http::run_from_env();
}

fn http_fixture(mode: &str) -> Fixture {
    let fixture = Fixture::new();
    let exe = std::env::current_exe().unwrap();
    std::fs::write(
        fixture.work.join("server.env"),
        format!("{}=1\n", aethyme_testkit::mcp_http::FAKE_SERVER_ENV),
    )
    .unwrap();
    fixture.services(&format!(
        "[[servers]]\nname = \"example-http\"\nmode = {mode:?}\ntransport = \"http\"\nargv = [{:?}, \"mcp_cli::fake_http_server_process\", \"--exact\", \"--nocapture\"]\nrequire_argv = [\"--exact\"]\nhttp_auth = \"bearer_env:{}\"\ncwd = {:?}\nenv_file = \"server.env\"\n",
        exe.display().to_string(),
        aethyme_testkit::mcp_http::TOKEN_ENV,
        fixture.work.display().to_string(),
    ));
    fixture
}

#[test]
fn shared_http_connects_reach_one_server_through_the_hub() {
    let fixture = http_fixture("shared");
    let (first, first_stderr) = session(&fixture, "example-http", "2025-06-18");
    let (second, second_stderr) = session(&fixture, "example-http", "2025-06-18");
    assert!(!first.is_empty(), "{first_stderr}");
    assert_eq!(first, second, "{second_stderr}");
    assert!(!first_stderr.contains("not used"), "{first_stderr}");
    let status = json(&fixture.run(&["mcp", "status", "--json"]));
    assert_eq!(status["hub"]["state"], "running", "{status}");
    for pid in hub_pids(&fixture) {
        Command::new("kill").arg(pid).status().unwrap();
    }
}

#[test]
fn direct_http_connects_each_bridge_a_private_server() {
    let fixture = http_fixture("direct");
    let (first, first_stderr) = session(&fixture, "example-http", "2025-06-18");
    let (second, _) = session(&fixture, "example-http", "2025-06-18");
    assert!(!first.is_empty(), "{first_stderr}");
    assert!(!second.is_empty());
    assert_ne!(first, second, "a direct server is never shared");
    assert!(hub_pids(&fixture).is_empty(), "no hub may start");
}
