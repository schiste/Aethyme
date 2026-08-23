use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

fn write_manifest(root: &Path, channel: &str, version: &str, preview: bool) {
    let directory = if preview {
        root.join("releases/download/preview")
    } else {
        root.join("releases/latest/download")
    };
    fs::create_dir_all(&directory).unwrap();
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ];
    let artifacts = targets
        .iter()
        .map(|target| {
            json!({
                "archive": format!("aethyme-v{version}-{target}.tar.gz"),
                "binaries": ["aethyme", "aethyme-engine-cli"],
                "sha256": "b".repeat(64),
                "size_bytes": 123,
                "target": target,
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "artifacts": artifacts,
        "compatibility": {
            "broker_storage": {"current_schema": 7, "minimum_readable_schema": 1},
            "engine_protocol": 1,
            "minimum_git_version": "2.38"
        },
        "installer": {
            "filename": "install.sh",
            "sha256": "c".repeat(64),
            "size_bytes": 42
        },
        "release_channel": channel,
        "required_binaries": ["aethyme", "aethyme-engine-cli"],
        "schema_version": 1,
        "source_sha": "a".repeat(40),
        "supported_platforms": targets,
        "version": version
    });
    fs::write(
        directory.join("release-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aethyme"));
    command.env(
        "AETHYME_RELEASE_BASE_URL",
        format!("file://{}", root.display()),
    );
    command
}

#[test]
fn update_help_is_explicit_and_never_background() {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["update", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "update check",
        "update plan [--channel stable|preview] [--json]",
        "update execute --confirm <manifest-sha256>",
        "never runs in the background",
        "brew upgrade aethyme",
    ] {
        assert!(stderr.contains(expected), "missing {expected:?}\n{stderr}");
    }
}

#[test]
fn stable_plan_returns_manifest_bound_json_without_mutating_manual_installs() {
    let temp = tempfile::tempdir().unwrap();
    write_manifest(temp.path(), "stable", "9.0.0", false);

    let output = command(temp.path())
        .args(["update", "plan", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["schema_version"], 1);
    assert_eq!(plan["channel"], "stable");
    assert_eq!(plan["target_version"], "9.0.0");
    assert_eq!(plan["manifest_sha256"].as_str().unwrap().len(), 64);
    assert!(matches!(
        plan["installation"]["method"].as_str(),
        Some("manual_archive" | "unknown")
    ));
    assert_eq!(plan["action"], "adopt_installer");
}

#[test]
fn preview_plan_uses_only_the_explicit_preview_channel() {
    let temp = tempfile::tempdir().unwrap();
    write_manifest(temp.path(), "preview", "9.1.0", true);

    let output = command(temp.path())
        .args(["update", "plan", "--channel", "preview", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["channel"], "preview");
    assert!(
        plan["manifest_url"]
            .as_str()
            .unwrap()
            .ends_with("/releases/download/preview/release-manifest.json")
    );
}

#[test]
fn check_is_stable_and_channel_mismatches_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    write_manifest(temp.path(), "preview", "9.0.0", false);

    let output = command(temp.path())
        .args(["update", "check", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not requested stable"));
}
