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
    fs::write(
        exact.join(format!("{archive}.sha256")),
        format!("{digest}  {archive}\n"),
    )
    .unwrap();
    fs::write(
        latest.join("release-manifest.json"),
        format!(
            "{{\n  \"installer\": {{\n    \"sha256\": \"{installer_digest}\"\n  }},\n  \"release_channel\": \"stable\",\n  \"version\": \"0.2.0\",\n  \"archive\": \"{archive}\"\n}}\n"
        ),
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
