#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use sha2::{Digest, Sha256};

use aethyme_testkit::{repo_root, tmp_dir};

#[test]
fn stable_installer_fetches_verifies_and_updates_the_binary_pair() {
    let temp = tmp_dir();
    let download_root = temp.path().join("download-root");
    let latest = download_root.join("releases/latest/download");
    let exact = download_root.join("releases/download/v0.2.0");
    let payload = temp.path().join("payload");
    let install_dir = temp.path().join("bin");
    fs::create_dir_all(&latest).unwrap();
    fs::create_dir_all(&exact).unwrap();
    fs::create_dir_all(&payload).unwrap();

    for binary in ["aethyme", "aethyme-engine-cli"] {
        let path = payload.join(binary);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{binary} 0.2.0 (fixture)'\n"),
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        pair => panic!("unsupported installer test platform: {pair:?}"),
    };
    let archive = format!("aethyme-v0.2.0-{target}.tar.gz");
    let archive_path = exact.join(&archive);
    let tar = Command::new("tar")
        .args(["-C", payload.to_str().unwrap(), "-czf"])
        .arg(&archive_path)
        .args(["aethyme", "aethyme-engine-cli"])
        .status()
        .unwrap();
    assert!(tar.success());

    let bytes = fs::read(&archive_path).unwrap();
    let digest = format!("{:x}", Sha256::digest(bytes));
    fs::write(
        exact.join(format!("{archive}.sha256")),
        format!("{digest}  {archive}\n"),
    )
    .unwrap();
    fs::write(
        latest.join("release-manifest.json"),
        format!(
            "{{\n  \"release_channel\": \"stable\",\n  \"version\": \"0.2.0\",\n  \"archive\": \"{archive}\"\n}}\n"
        ),
    )
    .unwrap();

    let syntax = Command::new("sh")
        .args(["-n", repo_root().join("install.sh").to_str().unwrap()])
        .status()
        .unwrap();
    assert!(syntax.success());

    let output = Command::new("sh")
        .arg(repo_root().join("install.sh"))
        .args(["--install-dir", install_dir.to_str().unwrap()])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", download_root.display()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(install_dir.join("aethyme").is_file());
    assert!(install_dir.join("aethyme-engine-cli").is_file());

    for binary in ["aethyme", "aethyme-engine-cli"] {
        let output = Command::new(install_dir.join(binary))
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("0.2.0"));
    }

    fs::write(&archive_path, "tampered archive").unwrap();
    let refused_dir = temp.path().join("refused-bin");
    let refused = Command::new("sh")
        .arg(repo_root().join("install.sh"))
        .args(["--install-dir", refused_dir.to_str().unwrap()])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", download_root.display()),
        )
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("SHA-256 mismatch"));
    assert!(!refused_dir.exists());
}
