//! Public router coverage for the explicit Local resolution command surface.
//! Each subprocess uses private host state and a fresh synthetic repository.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env_clear()
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Playground")
        .env("GIT_AUTHOR_EMAIL", "playground@example.invalid")
        .env("GIT_COMMITTER_NAME", "Playground")
        .env("GIT_COMMITTER_EMAIL", "playground@example.invalid")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn resolution_commands_route_and_refuse_uninitialized_state_without_creating_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("playground");
    let home = root.path().join("home");
    let state = root.path().join("state");
    let cache = root.path().join("cache");
    for path in [&repo, &home, &state, &cache] {
        std::fs::create_dir(path).unwrap();
    }
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/config.toml"),
        "[collaboration]\ncapture = \"advisory\"\nproject = \"proj-resolution-cli\"\n",
    )
    .unwrap();
    git(&repo, &home, &["add", ".aethyme/config.toml"]);
    git(
        &repo,
        &home,
        &["commit", "-qm", "chore: initialize Playground"],
    );
    let limits = root.path().join("limits.json");
    std::fs::write(
        &limits,
        br#"{"max_manifest_bytes":1048576,"max_manifest_entries":64,"max_file_bytes":65536,"max_total_input_bytes":1048576,"max_response_bytes":65536}"#,
    )
    .unwrap();
    let digest = format!("sha256:{}", "1".repeat(64));
    let limits = limits.to_str().unwrap();
    let cases: &[&[&str]] = &[
        &[
            "prepare",
            "--request",
            &digest,
            "--object",
            &digest,
            "--session-label",
            "operator",
            "--client-key",
            "first",
            "--limits",
            limits,
        ],
        &[
            "status",
            "--attempt",
            "unexported",
            "--session-label",
            "operator",
        ],
        &[
            "packet",
            "--attempt",
            "unexported",
            "--session-label",
            "operator",
        ],
        &[
            "input",
            "--attempt",
            "unexported",
            "--session-label",
            "operator",
            "--snapshot",
            &digest,
            "--path",
            "README.md",
        ],
        &[
            "import",
            "--attempt",
            "unexported",
            "--session-label",
            "operator",
            "--proposal",
            "not-opened.json",
        ],
    ];
    for args in cases {
        let output = Command::new(aethyme_testkit::aethyme_bin())
            .current_dir(&repo)
            .env("HOME", &home)
            .env("AETHYME_HOST_STATE_DIR", &state)
            .env("AETHYME_HOST_CACHE_DIR", &cache)
            .env("AETHYME_WORKTREE_ROOT", root.path().join("worktrees"))
            .env("AETHYME_CHAU7_MCP_BRIDGE", "/__no_playground_bridge__")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .args(["collab", "resolution"])
            .args(*args)
            .arg("--json")
            .output()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: {error}: {output:?}"));
        assert!(!output.status.success(), "{args:?}: {value}");
        assert_eq!(value["schema"], "aethyme.collab-error/experimental-v0");
        assert_eq!(value["code"], "not_initialized", "{args:?}: {value}");
        assert!(!state.join("collaboration").exists(), "{args:?}");
    }
}
