use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn current_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        other => panic!("unsupported test platform {other:?}"),
    }
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

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
    let encoded = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(directory.join("release-manifest.json"), &encoded).unwrap();
    let exact = root.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&exact).unwrap();
    fs::write(exact.join("release-manifest.json"), encoded).unwrap();
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aethyme"));
    command.env(
        "AETHYME_RELEASE_BASE_URL",
        format!("file://{}", root.display()),
    );
    command
}

fn write_fake_archive(root: &Path, version: &str) -> (String, String, u64) {
    let payload = root.join("new-payload");
    fs::create_dir_all(&payload).unwrap();
    fs::write(
        payload.join("aethyme"),
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'aethyme {version}'; exit 0; fi\nif [ \"$1\" = broker ] && [ \"$2\" = quick-test ]; then echo 'broker quick test passed'; exit 0; fi\nexit 2\n"
        ),
    )
    .unwrap();
    fs::write(
        payload.join("aethyme-engine-cli"),
        format!("#!/bin/sh\necho 'aethyme-engine-cli {version}'\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    for binary in ["aethyme", "aethyme-engine-cli"] {
        fs::set_permissions(payload.join(binary), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let archive = format!("aethyme-v{version}-{}.tar.gz", current_target());
    let directory = root.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&directory).unwrap();
    let archive_path = directory.join(&archive);
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&payload)
        .args(["aethyme", "aethyme-engine-cli"])
        .status()
        .unwrap();
    assert!(status.success());
    let size = fs::metadata(&archive_path).unwrap().len();
    (archive, sha256(&archive_path), size)
}

fn write_executable_manifest(root: &Path, version: &str) {
    let (archive, digest, size) = write_fake_archive(root, version);
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ];
    let artifacts = targets
        .iter()
        .map(|target| {
            let selected = *target == current_target();
            json!({
                "archive": if selected { archive.clone() } else { format!("aethyme-v{version}-{target}.tar.gz") },
                "binaries": ["aethyme", "aethyme-engine-cli"],
                "sha256": if selected { digest.clone() } else { "b".repeat(64) },
                "size_bytes": if selected { size } else { 123 },
                "target": target,
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "artifacts": artifacts,
        "compatibility": {
            "broker_storage": {"current_schema": 7, "minimum_readable_schema": 1},
            "engine_protocol": 9,
            "minimum_git_version": "2.38"
        },
        "installer": {"filename": "install.sh", "sha256": "c".repeat(64), "size_bytes": 42},
        "release_channel": "stable",
        "required_binaries": ["aethyme", "aethyme-engine-cli"],
        "schema_version": 1,
        "source_sha": "a".repeat(40),
        "supported_platforms": targets,
        "version": version
    });
    let latest = root.join("releases/latest/download");
    fs::create_dir_all(&latest).unwrap();
    let encoded = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(latest.join("release-manifest.json"), &encoded).unwrap();
    fs::write(
        root.join(format!(
            "releases/download/v{version}/release-manifest.json"
        )),
        encoded,
    )
    .unwrap();
}

fn install_managed_current(root: &Path) -> std::path::PathBuf {
    let install_dir = root.join("install/bin");
    let managed = install_dir.join(".aethyme-managed");
    let bundle = managed.join("versions/v0.2.0-current");
    fs::create_dir_all(&bundle).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_aethyme"), bundle.join("aethyme")).unwrap();
    fs::copy(
        aethyme_testkit::bins::engine_bin(),
        bundle.join("aethyme-engine-cli"),
    )
    .unwrap();
    std::os::unix::fs::symlink("versions/v0.2.0-current", managed.join("current")).unwrap();
    std::os::unix::fs::symlink(
        ".aethyme-managed/current/aethyme",
        install_dir.join("aethyme"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        ".aethyme-managed/current/aethyme-engine-cli",
        install_dir.join("aethyme-engine-cli"),
    )
    .unwrap();
    let receipt = json!({
        "schema_version": 1,
        "method": "aethyme-installer",
        "install_dir": install_dir,
        "managed_root": managed,
        "router_path": install_dir.join("aethyme"),
        "engine_path": install_dir.join("aethyme-engine-cli"),
        "current_link": managed.join("current"),
        "previous_link": managed.join("previous"),
        "versions_dir": managed.join("versions")
    });
    fs::write(
        managed.join("install-receipt.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    install_dir.join("aethyme")
}

fn plan_managed_update(root: &Path, installed_router: &Path) -> Value {
    let output = Command::new(installed_router)
        .args(["update", "plan", "--json"])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", root.display()),
        )
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
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
            .ends_with("/releases/download/v9.1.0/release-manifest.json")
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

#[test]
fn confirmed_execute_switches_the_pair_and_retains_the_previous_bundle() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());

    let plan = plan_managed_update(temp.path(), &installed_router);
    assert_eq!(plan["action"], "execute_installer_update");
    let confirmation = plan["manifest_sha256"].as_str().unwrap();

    let execute = Command::new(&installed_router)
        .args(["update", "execute", "--confirm", confirmation, "--json"])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", temp.path().display()),
        )
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        execute.status.success(),
        "{}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let report: Value = serde_json::from_slice(&execute.stdout).unwrap();
    assert_eq!(report["installed_version"], "9.0.0");
    assert_eq!(report["quick_test_passed"], true);

    let managed = temp.path().join("install/bin/.aethyme-managed");
    assert!(
        fs::read_link(managed.join("current"))
            .unwrap()
            .to_string_lossy()
            .starts_with("versions/v9.0.0-")
    );
    assert_eq!(
        fs::read_link(managed.join("previous")).unwrap(),
        Path::new("versions/v0.2.0-current")
    );
    let version = Command::new(&installed_router)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "aethyme 9.0.0"
    );
    assert!(
        !managed
            .join("update-plans")
            .join(format!("{confirmation}.json"))
            .exists()
    );
}

#[test]
fn checksum_mismatch_is_refused_without_moving_the_active_pair() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());
    let plan = plan_managed_update(temp.path(), &installed_router);
    let confirmation = plan["manifest_sha256"].as_str().unwrap();
    let archive_url = plan["archive"]["url"].as_str().unwrap();
    let archive = Path::new(archive_url.strip_prefix("file://").unwrap());
    let mut bytes = fs::read(archive).unwrap();
    *bytes.last_mut().unwrap() ^= 0xff;
    fs::write(archive, bytes).unwrap();
    let current = temp.path().join("install/bin/.aethyme-managed/current");
    let before = fs::read_link(&current).unwrap();

    let execute = Command::new(&installed_router)
        .args(["update", "execute", "--confirm", confirmation])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", temp.path().display()),
        )
        .current_dir(temp.path())
        .output()
        .unwrap();

    assert!(!execute.status.success());
    assert!(String::from_utf8_lossy(&execute.stderr).contains("SHA-256 mismatch"));
    assert_eq!(fs::read_link(current).unwrap(), before);
}
