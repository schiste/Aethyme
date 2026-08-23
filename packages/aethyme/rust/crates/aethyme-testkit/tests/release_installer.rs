#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_json::json;
use sha2::{Digest, Sha256};

use aethyme_testkit::{
    bins::{aethyme_bin, engine_bin},
    repo_root, tmp_dir,
};

#[test]
fn stable_installer_fetches_verifies_and_updates_the_binary_pair() {
    let router_binary = aethyme_bin();
    let engine_binary = engine_bin();
    let version_output = Command::new(&router_binary)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version_output.status.success());
    let version = String::from_utf8(version_output.stdout)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
    let temp = tmp_dir();
    let download_root = temp.path().join("download-root");
    let latest = download_root.join("releases/latest/download");
    let exact = download_root.join(format!("releases/download/v{version}"));
    let payload = temp.path().join("payload");
    let install_dir = temp.path().join("bin");
    fs::create_dir_all(&latest).unwrap();
    fs::create_dir_all(&exact).unwrap();
    fs::create_dir_all(&payload).unwrap();

    fs::copy(router_binary, payload.join("aethyme")).unwrap();
    fs::copy(engine_binary, payload.join("aethyme-engine-cli")).unwrap();

    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        pair => panic!("unsupported installer test platform: {pair:?}"),
    };
    let archive = format!("aethyme-v{version}-{target}.tar.gz");
    let archive_path = exact.join(&archive);
    let installer_path = repo_root().join("install.sh");
    let installer_digest = format!("{:x}", Sha256::digest(fs::read(&installer_path).unwrap()));
    let tar = Command::new("tar")
        .args(["-C", payload.to_str().unwrap(), "-czf"])
        .arg(&archive_path)
        .args(["aethyme", "aethyme-engine-cli"])
        .status()
        .unwrap();
    assert!(tar.success());

    let bytes = fs::read(&archive_path).unwrap();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let archive_size = fs::metadata(&archive_path).unwrap().len();
    fs::write(
        exact.join(format!("{archive}.sha256")),
        format!("{digest}  {archive}\n"),
    )
    .unwrap();
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ];
    let artifacts = targets
        .iter()
        .map(|artifact_target| {
            let selected = *artifact_target == target;
            json!({
                "archive": format!("aethyme-v{version}-{artifact_target}.tar.gz"),
                "binaries": ["aethyme", "aethyme-engine-cli"],
                "sha256": if selected { digest.clone() } else { "b".repeat(64) },
                "size_bytes": if selected { archive_size } else { 1 },
                "target": artifact_target,
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
            "sha256": installer_digest,
            "size_bytes": fs::metadata(&installer_path).unwrap().len()
        },
        "release_channel": "stable",
        "required_binaries": ["aethyme", "aethyme-engine-cli"],
        "schema_version": 1,
        "source_sha": "a".repeat(40),
        "supported_platforms": targets,
        "version": version
    });
    fs::write(
        latest.join("release-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        latest.join("release-manifest.sigstore.json"),
        "fixture bundle\n",
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
    for binary in ["aethyme", "aethyme-engine-cli"] {
        let public = install_dir.join(binary);
        assert_eq!(
            fs::read_link(&public).unwrap(),
            std::path::Path::new(&format!(".aethyme-managed/current/{binary}"))
        );
        assert!(public.is_file());
    }
    assert!(install_dir.join(".aethyme-managed/current").is_symlink());

    for binary in ["aethyme", "aethyme-engine-cli"] {
        let output = Command::new(install_dir.join(binary))
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(&version));
    }

    let mock_bin = temp.path().join("mock-bin");
    fs::create_dir(&mock_bin).unwrap();
    let cosign = mock_bin.join("cosign");
    fs::write(&cosign, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&cosign).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cosign, permissions).unwrap();
    let signature_dir = temp.path().join("signature-bin");
    let path = format!(
        "{}:{}",
        mock_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let verified = Command::new("sh")
        .arg(&installer_path)
        .args([
            "--verify-signature",
            "--install-dir",
            signature_dir.to_str().unwrap(),
        ])
        .env("PATH", &path)
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", download_root.display()),
        )
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "signature stderr:\n{}",
        String::from_utf8_lossy(&verified.stderr)
    );

    let tampered_installer = temp.path().join("tampered-install.sh");
    let mut tampered = fs::read_to_string(&installer_path).unwrap();
    tampered.push_str("\n# tampered after review\n");
    fs::write(&tampered_installer, tampered).unwrap();
    let refused_installer = Command::new("sh")
        .arg(&tampered_installer)
        .arg("--verify-signature")
        .env("PATH", &path)
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", download_root.display()),
        )
        .output()
        .unwrap();
    assert!(!refused_installer.status.success());
    assert!(
        String::from_utf8_lossy(&refused_installer.stderr)
            .contains("does not match the signed manifest")
    );

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
