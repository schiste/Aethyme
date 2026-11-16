//! Unit tests for managed servers: the matcher, the managed fields, the
//! match file and shim decisions, interception install/status, the JSON
//! span editor, config repair and bypass counting. Every path is a
//! temporary directory; no real agent configuration is read.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::checkouts::{Checkout, Checkouts};
use super::config::{Scope, parse_and_validate};
use super::intercept::{self, Decision, decide, find_real_tool, parse_match_file, strip_path};
use super::json_span;
use super::managed::{MatchEntry, MatchSet, RepairMode, simple_command};
use super::repair::{self, Locations, Options, Target, TargetKind};
use super::{bypass, telemetry};

const SECRET: &str = "sk-test-SECRET-value-0123";

struct NoCheckouts;

impl Checkouts for NoCheckouts {
    fn checkout(&self, _: &Path) -> Option<Checkout> {
        None
    }
    fn session_worktree(&self, _: &Path) -> Option<Checkout> {
        None
    }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn write_executable(path: &Path, text: &str) {
    write(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn devtools_set() -> MatchSet {
    MatchSet {
        entries: vec![
            MatchEntry {
                name: "example-devtools".into(),
                packages: strings(&["example-devtools-mcp", "@example/devtools-mcp"]),
                executables: strings(&["example-devtools-mcp"]),
                repair: RepairMode::Auto,
            },
            MatchEntry {
                name: "example-reported".into(),
                packages: strings(&["example-reported-mcp"]),
                executables: Vec::new(),
                repair: RepairMode::Report,
            },
        ],
    }
}

#[test]
fn the_matcher_recognises_every_launcher_shape() {
    let set = devtools_set();
    let matching: &[&[&str]] = &[
        &["npx", "-y", "example-devtools-mcp@latest", "--flag"],
        &["npx", "example-devtools-mcp"],
        &["/usr/local/bin/npx", "--yes", "@example/devtools-mcp@2.1.0"],
        &["npx", "-p", "example-devtools-mcp", "example-devtools-mcp"],
        &["npx", "--package=example-devtools-mcp", "run-it"],
        &["npm", "exec", "example-devtools-mcp", "--", "--x"],
        &["npm", "x", "-y", "example-devtools-mcp"],
        &["pnpm", "exec", "example-devtools-mcp", "--stdio"],
        &["pnpm", "dlx", "example-devtools-mcp"],
        &["pnpx", "example-devtools-mcp"],
        &["bunx", "example-devtools-mcp"],
        &["bun", "x", "example-devtools-mcp"],
        &["yarn", "dlx", "example-devtools-mcp"],
        &["env", "FOO=1", "npx", "-y", "example-devtools-mcp"],
        &[
            "/usr/bin/env",
            "-i",
            "PATH=/x",
            "npx",
            "example-devtools-mcp",
        ],
        &["sh", "-c", "npx -y example-devtools-mcp@1.2.3 --flag"],
        &[
            "/bin/bash",
            "-c",
            "exec npx -y 'example-devtools-mcp' \"--a b\"",
        ],
        &["/opt/tools/example-devtools-mcp", "--stdio"],
        &[
            "node",
            "/x/lib/node_modules/npm/bin/npx-cli.js",
            "-y",
            "example-devtools-mcp",
        ],
    ];
    for argv in matching {
        let found = set
            .managed_match(&strings(argv))
            .unwrap_or_else(|| panic!("{argv:?} should match"));
        assert_eq!(found.name, "example-devtools", "{argv:?}");
    }
    let found = set
        .managed_match(&strings(&["npx", "-y", "example-devtools-mcp@latest"]))
        .unwrap();
    assert_eq!(found.kind, "package");
    assert_eq!(found.matched, "example-devtools-mcp");
    assert_eq!(found.launcher, Some("npx"));
    let found = set
        .managed_match(&strings(&["/opt/tools/example-devtools-mcp"]))
        .unwrap();
    assert_eq!(found.kind, "executable");
    assert_eq!(found.launcher, None);
    let reported = set
        .managed_match(&strings(&["npx", "example-reported-mcp"]))
        .unwrap();
    assert_eq!(reported.repair, RepairMode::Report);
}

#[test]
fn the_matcher_never_guesses_or_matches_connect() {
    let set = devtools_set();
    let not_matching: &[&[&str]] = &[
        &[
            "aethyme",
            "mcp",
            "connect",
            "example-devtools",
            "--fallback",
            "--",
            "npx",
            "-y",
            "example-devtools-mcp",
        ],
        &[
            "env",
            "A=1",
            "/usr/local/bin/aethyme",
            "mcp",
            "connect",
            "example-devtools",
        ],
        &["npx", "-y", "other-mcp"],
        &["npx", "example-devtools-mcp-extra"],
        &["npx", "--unknown-option", "example-devtools-mcp"],
        &["npm", "install", "example-devtools-mcp"],
        &["sh", "-c", "npx example-devtools-mcp; rm -rf x"],
        &["sh", "-c", "FOO=1 npx example-devtools-mcp"],
        &["sh", "-c", "npx $PKG"],
        &["sh", "-c", "npx example-devtools-mcp > log"],
        &["bash", "-c", "npx example-devtools-mcp", "extra"],
        &["env", "-S", "npx example-devtools-mcp"],
        &["node", "server.js", "example-devtools-mcp"],
        &[],
    ];
    for argv in not_matching {
        assert_eq!(set.managed_match(&strings(argv)), None, "{argv:?}");
    }
    assert_eq!(
        simple_command("a 'b c' \"d\""),
        Some(strings(&["a", "b c", "d"]))
    );
    assert_eq!(simple_command("a \"$HOME\""), None);
    assert_eq!(simple_command("a 'open"), None);
}

#[test]
fn only_the_host_file_can_mark_an_entry_managed() {
    let host = "schema = 1\n[[servers]]\nname = \"example-devtools\"\nargv = [\"npx\", \"-y\", \
                \"example-devtools-mcp\"]\nmanaged = true\nmatch_packages = \
                [\"example-devtools-mcp@latest\"]\nmatch_executables = [\"example-devtools-mcp\"]\n\
                repair = \"report\"\n";
    let (entries, errors) = parse_and_validate(
        host,
        Path::new("/host/mcp/services.toml"),
        Scope::Host,
        None,
        &NoCheckouts,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let spec = entries[0].managed.clone().unwrap();
    assert_eq!(spec.match_packages, strings(&["example-devtools-mcp"]));
    assert_eq!(spec.repair, RepairMode::Report);

    let (root, anchor_root) = canonical_tempdir();
    let anchor = super::config::RepositoryAnchor {
        main_root: anchor_root.clone(),
        identity: "repo-1".into(),
    };
    let repository = "schema = 1\n[[servers]]\nname = \"x\"\nargv = [\"x\"]\nmanaged = true\n\
                      match_executables = [\"x\"]\n";
    let (entries, errors) = parse_and_validate(
        repository,
        &anchor_root.join(".aethyme/mcp.toml"),
        Scope::Repository,
        Some(&anchor),
        &NoCheckouts,
    );
    drop(root);
    assert!(entries.is_empty());
    assert_eq!(errors[0].code, "managed_in_repository");

    let refused = |body: &str| {
        let text = format!("schema = 1\n[[servers]]\nname = \"x\"\nargv = [\"x\"]\n{body}");
        parse_and_validate(
            &text,
            Path::new("/host/mcp/services.toml"),
            Scope::Host,
            None,
            &NoCheckouts,
        )
        .1[0]
            .code
    };
    assert_eq!(
        refused("match_packages = [\"a\"]\n"),
        "managed_fields_without_managed"
    );
    assert_eq!(refused("managed = true\n"), "managed_without_matchers");
    assert_eq!(
        refused("managed = true\nmatch_packages = [\"Not A Package\"]\n"),
        "invalid_package"
    );
    assert_eq!(
        refused("managed = true\nmatch_executables = [\"npx\"]\n"),
        "reserved_executable"
    );
    assert_eq!(
        refused("managed = true\nmatch_executables = [\"a/b\"]\n"),
        "invalid_executable"
    );
}

#[test]
fn the_match_file_round_trips_and_corruption_is_detected() {
    let set = devtools_set();
    let text = intercept::render_match_file(&set, Path::new("/state/mcp/shims dir")).unwrap();
    let parsed = parse_match_file(text.as_bytes()).unwrap();
    assert_eq!(parsed.set, set);
    assert_eq!(
        parsed.shim_dir.as_deref(),
        Some(Path::new("/state/mcp/shims dir"))
    );
    let truncated = &text[..text.len() - 4];
    assert_eq!(
        parse_match_file(truncated.as_bytes()),
        Err("match_file_truncated")
    );
    assert_eq!(
        parse_match_file(b"garbage\nend\n"),
        Err("match_file_header")
    );
    assert!(parse_match_file(format!("{text}package x\n").as_bytes()).is_err());
    assert!(parse_match_file(&[0xff, 0xfe]).is_err());
}

/// A shim directory with `npx` pointing at a stand-in for the aethyme
/// binary, a real `npx`, and the canonical stand-in path.
fn shim_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let (dir, root) = canonical_tempdir();
    let self_bin = root.join("bin/aethyme");
    write_executable(&self_bin, "#!/bin/sh\n");
    let shims = root.join("shims");
    std::fs::create_dir_all(&shims).unwrap();
    std::os::unix::fs::symlink(&self_bin, shims.join("npx")).unwrap();
    let real = root.join("real");
    write_executable(&real.join("npx"), "#!/bin/sh\n");
    (dir, root, self_bin, shims, real)
}

fn path_of<P: AsRef<Path>>(dirs: &[P]) -> OsString {
    let owned: Vec<PathBuf> = dirs.iter().map(|dir| dir.as_ref().to_path_buf()).collect();
    std::env::join_paths(owned).unwrap()
}

#[test]
fn a_shim_passes_through_hands_off_and_honours_the_guard() {
    let (_dir, _root, self_bin, shims, real) = shim_fixture();
    let file = intercept::MatchFile {
        set: devtools_set(),
        shim_dir: Some(shims.clone()),
    };
    let path = path_of(&[&shims, &real]);
    let args = |words: &[&str]| -> Vec<OsString> { words.iter().map(OsString::from).collect() };
    let pass = decide(
        "npx",
        &args(&["-y", "other-mcp"]),
        Some(&file),
        false,
        Some(path.as_os_str()),
        Some(self_bin.as_path()),
    );
    assert_eq!(pass, Decision::PassThrough(real.join("npx")));
    let handoff = decide(
        "npx",
        &args(&["-y", "example-devtools-mcp@latest", "--flag"]),
        Some(&file),
        false,
        Some(path.as_os_str()),
        Some(self_bin.as_path()),
    );
    assert_eq!(
        handoff,
        Decision::HandOff {
            entry: "example-devtools".into(),
            fallback: Some(real.join("npx")),
        }
    );
    let guarded = decide(
        "npx",
        &args(&["-y", "example-devtools-mcp"]),
        Some(&file),
        true,
        Some(path.as_os_str()),
        Some(self_bin.as_path()),
    );
    assert_eq!(guarded, Decision::PassThrough(real.join("npx")));
    let no_file = decide(
        "npx",
        &args(&["-y", "example-devtools-mcp"]),
        None,
        false,
        Some(path.as_os_str()),
        Some(self_bin.as_path()),
    );
    assert_eq!(no_file, Decision::PassThrough(real.join("npx")));
    let missing = decide(
        "npx",
        &args(&["-y", "other"]),
        Some(&file),
        false,
        Some(path_of(&[&shims]).as_os_str()),
        Some(self_bin.as_path()),
    );
    assert_eq!(missing, Decision::NotFound);
    assert!(intercept::guard_active(
        Some(std::ffi::OsStr::new("1")),
        None
    ));
    assert!(intercept::guard_active(
        None,
        Some(std::ffi::OsStr::new("2"))
    ));
    assert!(!intercept::guard_active(
        None,
        Some(std::ffi::OsStr::new("1"))
    ));
}

#[test]
fn the_real_tool_is_found_past_every_spelling_of_the_shim_directory() {
    let (_dir, root, self_bin, shims, real) = shim_fixture();
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&shims, &alias).unwrap();
    // Twice, and through a symlink, ahead of the real tool.
    let path = path_of(&[&shims, &alias, &shims, &real]);
    assert_eq!(
        find_real_tool(
            "npx",
            Some(path.as_os_str()),
            Some(self_bin.as_path()),
            Some(shims.as_path())
        ),
        Some(real.join("npx"))
    );
    // Even without the shim directory: anything resolving to this binary.
    assert_eq!(
        find_real_tool(
            "npx",
            Some(path.as_os_str()),
            Some(self_bin.as_path()),
            None
        ),
        Some(real.join("npx"))
    );
    assert_eq!(
        find_real_tool("npx", Some(path.as_os_str()), None, None),
        None
    );
    let stripped = strip_path(Some(path.as_os_str()), Some(shims.as_path())).unwrap();
    let kept: Vec<PathBuf> = std::env::split_paths(&stripped).collect();
    assert_eq!(kept, vec![real.clone()]);
    assert_eq!(
        strip_path(Some(path_of(&[&real]).as_os_str()), Some(shims.as_path())),
        None
    );
}

#[test]
fn install_status_and_uninstall_manage_only_their_own_files() {
    let (_dir, root) = canonical_tempdir();
    let host = root.join("state/mcp");
    let target = root.join("bin/aethyme");
    write_executable(&target, "#!/bin/sh\n");
    let set = devtools_set();
    let report = intercept::install(&host, None, &set, &target).unwrap();
    let shims = host.join("shims");
    assert_eq!(report.shim_dir, shims);
    for name in [
        "npx",
        "npm",
        "pnpm",
        "pnpx",
        "bunx",
        "yarn",
        "example-devtools-mcp",
    ] {
        let link = std::fs::read_link(shims.join(name)).unwrap();
        assert_eq!(link, target, "{name}");
    }
    let mode = std::fs::metadata(&shims).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    assert!(report.path_line.starts_with("export PATH=\""));

    let real = root.join("real");
    write_executable(&real.join("npx"), "#!/bin/sh\n");
    let status = intercept::status(&host, &set, Some(path_of(&[&shims, &real]).as_os_str()));
    assert!(status.complete, "{status:?}");
    assert_eq!(status.match_file_state, "current");
    assert!(status.on_path);
    let behind = intercept::status(&host, &set, Some(path_of(&[&real, &shims]).as_os_str()));
    assert!(behind.on_path && !behind.ahead_of_real_tools);
    let fewer = MatchSet {
        entries: set.entries[1..].to_vec(),
    };
    assert_eq!(
        intercept::status(&host, &fewer, None).match_file_state,
        "stale"
    );

    // A refresh with fewer executables removes the stale shim.
    let refreshed = intercept::regenerate_if_installed(&host, &fewer)
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.removed, strings(&["example-devtools-mcp"]));
    assert!(!shims.join("example-devtools-mcp").exists());

    // A foreign file in the directory survives uninstall.
    write(&shims.join("keep-me"), "mine\n");
    let removed = intercept::uninstall(&host).unwrap();
    assert!(removed.match_file_removed);
    assert!(!removed.directory_removed);
    assert!(shims.join("keep-me").exists());
    assert!(!shims.join("npx").exists());

    // A non-empty directory install did not create is refused.
    let foreign = root.join("foreign");
    write(&foreign.join("tool"), "x");
    assert!(intercept::install(&host, Some(foreign.as_path()), &set, &target).is_err());
    assert!(intercept::install(&host, Some(Path::new("relative")), &set, &target).is_err());
}

#[test]
fn the_json_span_reader_keeps_ranges_and_rejects_invalid_json() {
    let text = "{\n  \"a\": [1, \"x\\u00e9\\ud83d\\ude00\"],\n  \"b\": {\"c\": null}\n}";
    let root = json_span::parse(text).unwrap();
    let a = root.get("a").unwrap();
    assert_eq!(
        &text[a.value.start..a.value.end],
        "[1, \"x\\u00e9\\ud83d\\ude00\"]"
    );
    match &a.value.kind {
        json_span::Kind::Array(items) => assert_eq!(items[1].as_str(), Some("x\u{e9}\u{1f600}")),
        other => panic!("{other:?}"),
    }
    assert_eq!(&text[root.get("b").unwrap().key_start..][..3], "\"b\"");
    for invalid in ["{\"a\": 1,}", "[1 2]", "{\"a\" 1}", "\"open", "01", "{} x"] {
        assert!(json_span::parse(invalid).is_err(), "{invalid}");
    }
    let edited = json_span::apply(
        "abcdef",
        vec![
            json_span::Edit {
                start: 4,
                end: 5,
                text: "E".into(),
            },
            json_span::Edit {
                start: 1,
                end: 1,
                text: "+".into(),
            },
        ],
    )
    .unwrap();
    assert_eq!(edited, "a+bcdEf");
}

const CODEX: &str = "# my settings\nmodel = \"x\"\n\n[mcp_servers.devtools]\ncommand = \"npx\"\n\
args = [\"-y\", \"example-devtools-mcp@latest\", \"--headless\"]  # keep me\nstartup_timeout_sec = 20\n\n\
[mcp_servers.devtools.env]\nAPI_TOKEN = \"sk-test-SECRET-value-0123\"\n\n[mcp_servers.other]\n\
command = \"other-server\"\nargs = [\"--x\"]\n";

fn claude_json() -> String {
    format!(
        "{{\n  \"numStartups\": 3,\n  \"mcpServers\": {{\n    \"devtools\": {{\n      \"type\": \"stdio\",\n      \
         \"command\": \"npx\",\n      \"args\": [\n        \"-y\",\n        \"example-devtools-mcp\"\n      ],\n      \
         \"env\": {{\n        \"API_TOKEN\": \"{SECRET}\"\n      }}\n    }},\n    \"remote\": {{\n      \"type\": \"http\",\n      \
         \"url\": \"https://example.invalid/{SECRET}\"\n    }}\n  }},\n  \"projects\": {{\n    \"/work/private-project\": {{\n      \
         \"mcpServers\": {{\n        \"local-devtools\": {{\n          \"command\": \"/opt/tools/example-devtools-mcp\"\n        \
         }},\n        \"reported\": {{\n          \"command\": \"npx\",\n          \"args\": [\"example-reported-mcp\"]\n        \
         }}\n      }},\n      \"unknownKey\": [1, 2]\n    }}\n  }}\n}}\n"
    )
}

struct RepairFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    codex: PathBuf,
    claude: PathBuf,
    backups: PathBuf,
}

impl RepairFixture {
    fn new() -> Self {
        let (dir, root) = canonical_tempdir();
        let codex = root.join("home/.codex/config.toml");
        let claude = root.join("home/.claude.json");
        write(&codex, CODEX);
        write(&claude, &claude_json());
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            backups: root.join("state/mcp/repair-backups"),
            _dir: dir,
            root,
            codex,
            claude,
        }
    }

    fn locations(&self) -> Locations {
        Locations {
            home: Some(self.root.join("home")),
            codex: Some(self.codex.clone()),
            claude: Some(self.claude.clone()),
            project: None,
        }
    }

    fn options(&self, apply: bool, auto: bool) -> Options {
        Options {
            apply,
            auto,
            deadline: None,
            backup_dir: self.backups.clone(),
        }
    }

    fn run(&self, apply: bool, auto: bool) -> repair::Report {
        let locations = self.locations();
        repair::run(
            &devtools_set(),
            &locations.targets(None),
            &locations,
            &self.options(apply, auto),
        )
    }

    fn backups(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.backups)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

#[test]
fn check_lists_matches_and_writes_nothing() {
    let fixture = RepairFixture::new();
    let report = fixture.run(false, false);
    assert_eq!(report.mode, "check");
    let servers: Vec<(&str, &str, &str)> = report
        .files
        .iter()
        .flat_map(|file| &file.findings)
        .map(|finding| {
            (
                finding.server.as_str(),
                finding.entry.as_str(),
                finding.state,
            )
        })
        .collect();
    assert_eq!(
        servers,
        vec![
            ("devtools", "example-devtools", "would_repair"),
            ("devtools", "example-devtools", "would_repair"),
            ("local-devtools", "example-devtools", "would_repair"),
            ("reported", "example-reported", "would_repair"),
        ]
    );
    assert_eq!(std::fs::read_to_string(&fixture.codex).unwrap(), CODEX);
    assert_eq!(
        std::fs::read_to_string(&fixture.claude).unwrap(),
        claude_json()
    );
    assert!(fixture.backups().is_empty());
}

#[test]
fn apply_rewrites_only_managed_entries_and_keeps_everything_else() {
    let fixture = RepairFixture::new();
    let report = fixture.run(true, false);
    assert_eq!(report.repaired, 4, "{report:?}");

    let codex = std::fs::read_to_string(&fixture.codex).unwrap();
    let header = "[mcp_servers.devtools]\n";
    let before_header = &CODEX[..CODEX.find(header).unwrap()];
    assert!(codex.starts_with(before_header));
    let env_onwards = &CODEX[CODEX.find("startup_timeout_sec").unwrap()..];
    assert!(codex.ends_with(env_onwards), "{codex}");
    let parsed: toml::Table = toml::from_str(&codex).unwrap();
    let devtools = &parsed["mcp_servers"]["devtools"];
    assert_eq!(devtools["command"].as_str(), Some("aethyme"));
    let args: Vec<&str> = devtools["args"]
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
            "example-devtools",
            "--fallback",
            "--",
            "npx",
            "-y",
            "example-devtools-mcp@latest",
            "--headless"
        ]
    );
    assert_eq!(devtools["env"]["API_TOKEN"].as_str(), Some(SECRET));
    assert!(codex.contains("# keep me"));

    let claude = std::fs::read_to_string(&fixture.claude).unwrap();
    let original = claude_json();
    // Everything outside the edited values is byte-identical.
    let env_block = format!(
        "\"env\": {{\n        \"API_TOKEN\": \"{SECRET}\"\n      }}\n    }},\n    \"remote\": {{"
    );
    for kept in [
        "{\n  \"numStartups\": 3,\n  \"mcpServers\": {\n    \"devtools\": {\n      \"type\": \"stdio\",\n      \"command\": ",
        env_block.as_str(),
        "\"unknownKey\": [1, 2]\n    }\n  }\n}\n",
    ] {
        assert!(
            original.contains(kept) && claude.contains(kept),
            "{kept}\n{claude}"
        );
    }
    let value: serde_json::Value = serde_json::from_str(&claude).unwrap();
    assert_eq!(value["mcpServers"]["devtools"]["command"], "aethyme");
    assert_eq!(
        value["mcpServers"]["devtools"]["args"][2],
        "example-devtools"
    );
    assert_eq!(value["mcpServers"]["devtools"]["env"]["API_TOKEN"], SECRET);
    assert_eq!(value["mcpServers"]["remote"]["type"], "http");
    let local = &value["projects"]["/work/private-project"]["mcpServers"]["local-devtools"];
    assert_eq!(local["command"], "aethyme");
    assert_eq!(local["args"][5], "/opt/tools/example-devtools-mcp");
    let mode = std::fs::metadata(&fixture.claude)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let backups = fixture.backups();
    assert_eq!(backups.len(), 2, "{backups:?}");
    for name in &backups {
        let mode = std::fs::metadata(fixture.backups.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert!(!backups.iter().any(|name| name.contains("claude.json")));

    // A second run changes nothing and writes no backup.
    let again = fixture.run(true, false);
    assert_eq!(again.repaired, 0);
    assert_eq!(again.matched, 0);
    assert_eq!(
        again
            .files
            .iter()
            .map(|file| file.already_managed)
            .sum::<usize>(),
        4
    );
    assert_eq!(std::fs::read_to_string(&fixture.claude).unwrap(), claude);
    assert_eq!(fixture.backups().len(), 2);
}

#[test]
fn auto_repairs_only_auto_entries() {
    let fixture = RepairFixture::new();
    let report = fixture.run(true, true);
    let reported: Vec<_> = report
        .files
        .iter()
        .flat_map(|file| &file.findings)
        .filter(|finding| finding.server == "reported")
        .collect();
    assert_eq!(reported[0].state, "report_only");
    assert_eq!(reported[0].reason, Some("repair_report"));
    assert_eq!(report.repaired, 3);
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture.claude).unwrap()).unwrap();
    assert_eq!(
        value["projects"]["/work/private-project"]["mcpServers"]["reported"]["command"],
        "npx"
    );
}

#[test]
fn no_value_from_a_configuration_reaches_output_or_events() {
    let fixture = RepairFixture::new();
    let report = fixture.run(true, false);
    let host = fixture.root.join("state/mcp");
    repair::record(&host, &report, 1);
    let rendered = serde_json::to_string(&report).unwrap();
    let events = std::fs::read_to_string(host.join(telemetry::EVENTS_FILE)).unwrap();
    for text in [&rendered, &events] {
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("private-project"), "{text}");
        assert!(!text.contains("--headless"), "{text}");
        assert!(!text.contains("example.invalid"), "{text}");
    }
    assert!(rendered.contains("\"program\":\"npx\""));
    assert!(rendered.contains("\"scope\":\"project #1\""));
}

#[test]
fn a_contended_file_is_never_written() {
    let fixture = RepairFixture::new();
    let locations = fixture.locations();
    let targets: Vec<Target> = locations
        .targets(Some("claude"))
        .into_iter()
        .filter(|target| target.kind == TargetKind::ClaudeUser)
        .collect();
    let claude = fixture.claude.clone();
    let mut writes = 0;
    let report = repair::run_with_hook(
        &devtools_set(),
        &targets,
        &locations,
        &fixture.options(true, false),
        &mut |_, attempt| {
            writes += 1;
            // The owning application rewrites the file under the writer.
            std::fs::write(
                &claude,
                format!("{}\n{}", claude_json(), " ".repeat(attempt)),
            )
            .unwrap();
        },
    );
    assert_eq!(writes, repair::ATTEMPTS);
    assert_eq!(report.files[0].state, "contended");
    assert!(
        report.files[0]
            .findings
            .iter()
            .all(|finding| finding.state == "contended")
    );
    let now = std::fs::read_to_string(&fixture.claude).unwrap();
    assert_eq!(now, format!("{}\n{}", claude_json(), " ".repeat(3)));
    let leftovers: Vec<_> = std::fs::read_dir(fixture.claude.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".aethyme-repair-")
        })
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn backups_are_pruned_to_the_newest_five() {
    let fixture = RepairFixture::new();
    for _ in 0..7 {
        write(&fixture.claude, &claude_json());
        fixture.run(true, false);
    }
    let claude_backups: Vec<String> = fixture
        .backups()
        .into_iter()
        .filter(|name| name.starts_with("claude-"))
        .collect();
    assert_eq!(claude_backups.len(), repair::KEEP_BACKUPS);
}

#[test]
fn unparsable_symlinked_and_tracked_files_are_not_written() {
    let fixture = RepairFixture::new();
    // Not JSON: reported with a position only, never written.
    let broken = format!("{{\"mcpServers\": {{\"x\": \"{SECRET}\",}}}}");
    write(&fixture.claude, &broken);
    let report = fixture.run(true, false);
    let claude = report
        .files
        .iter()
        .find(|file| file.kind == TargetKind::ClaudeUser)
        .unwrap();
    assert_eq!(claude.state, "parse_error");
    assert!(!claude.detail.clone().unwrap_or_default().contains(SECRET));
    assert_eq!(std::fs::read_to_string(&fixture.claude).unwrap(), broken);

    // A symlink to a file outside HOME is refused; inside HOME it is followed
    // and stays a symlink.
    let outside = fixture.root.join("elsewhere/claude.json");
    write(&outside, &claude_json());
    std::fs::remove_file(&fixture.claude).unwrap();
    std::os::unix::fs::symlink(&outside, &fixture.claude).unwrap();
    let report = fixture.run(true, false);
    assert!(
        report
            .files
            .iter()
            .any(|file| file.state == "refused_symlink")
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), claude_json());
    let inside = fixture.root.join("home/dotfiles/claude.json");
    write(&inside, &claude_json());
    std::fs::remove_file(&fixture.claude).unwrap();
    std::os::unix::fs::symlink(&inside, &fixture.claude).unwrap();
    fixture.run(true, false);
    assert!(
        std::fs::symlink_metadata(&fixture.claude)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::read_to_string(&inside)
            .unwrap()
            .contains("\"aethyme\"")
    );

    // A tracked repository .mcp.json is only reported.
    let repo = fixture.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    let project = "{\n  \"mcpServers\": {\n    \"devtools\": {\"command\": \"npx\", \"args\": [\"example-devtools-mcp\"]}\n  }\n}\n";
    write(&repo.join(".mcp.json"), project);
    git(&["add", ".mcp.json"]);
    git(&["commit", "-qm", "init"]);
    let locations = Locations {
        home: Some(fixture.root.join("home")),
        codex: None,
        claude: None,
        project: Some((repo.clone(), repo.join(".mcp.json"))),
    };
    let targets = locations.targets(Some("claude"));
    assert!(!targets[0].writable);
    let report = repair::run(
        &devtools_set(),
        &targets,
        &locations,
        &fixture.options(true, false),
    );
    assert_eq!(report.files[0].findings[0].state, "report_only");
    assert_eq!(
        report.files[0].findings[0].reason,
        Some("tracked_repository_file")
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(".mcp.json")).unwrap(),
        project
    );
}

#[test]
fn a_single_line_server_object_gains_args_on_its_line() {
    let fixture = RepairFixture::new();
    write(
        &fixture.claude,
        "{\"mcpServers\": {\"d\": {\"command\": \"example-devtools-mcp\", \"env\": {}}}}",
    );
    fixture.run(true, false);
    assert_eq!(
        std::fs::read_to_string(&fixture.claude).unwrap(),
        "{\"mcpServers\": {\"d\": {\"command\": \"aethyme\", \"args\": [\"mcp\", \"connect\", \
         \"example-devtools\", \"--fallback\", \"--\", \"example-devtools-mcp\"], \"env\": {}}}}"
    );
}

#[test]
fn the_session_start_switch_is_off_unless_set() {
    let (_dir, host) = canonical_tempdir();
    assert_eq!(repair::repair_on_session_start(&host), Ok(false));
    write(
        &host.join(repair::SETTINGS_FILE),
        "[mcp]\nrepair_on_session_start = true\n",
    );
    assert_eq!(repair::repair_on_session_start(&host), Ok(true));
    write(
        &host.join(repair::SETTINGS_FILE),
        "[mcp]\nrepair_on_session_start = \"yes\"\n",
    );
    assert!(repair::repair_on_session_start(&host).is_err());
}

#[test]
fn bypass_counts_launchers_but_not_connect_started_servers() {
    let rows = bypass::parse_ps(
        "  10 npm exec example-devtools-mcp@latest\n\
         11 node /x/npm/bin/npx-cli.js -y example-devtools-mcp\n\
         12 /opt/node /srv/devtools/index.js --stdio\n\
         13 aethyme mcp connect example-devtools --fallback -- npx example-devtools-mcp\n\
         14 npx -y example-devtools-mcp\n\
         15 unrelated --thing\n",
    );
    assert_eq!(rows.len(), 6);
    let mut remembered = std::collections::BTreeMap::new();
    remembered.insert(
        "example-devtools".to_string(),
        "/opt/node /srv/devtools/index.js --stdio".to_string(),
    );
    let counts = bypass::count(&devtools_set(), &rows, &remembered, 14);
    let devtools = counts
        .iter()
        .find(|entry| entry.name == "example-devtools")
        .unwrap();
    assert_eq!(devtools.processes, 2);
    assert_eq!(devtools.matched["example-devtools-mcp"], 2);
    assert_eq!(devtools.via_connect, 0);
    let mut exact = remembered.clone();
    exact.insert(
        "example-devtools".to_string(),
        "npm exec example-devtools-mcp@latest".to_string(),
    );
    let counts = bypass::count(&devtools_set(), &rows, &exact, 0);
    assert_eq!(counts[0].via_connect, 1);
    assert_eq!(counts[0].processes, 2);
}
