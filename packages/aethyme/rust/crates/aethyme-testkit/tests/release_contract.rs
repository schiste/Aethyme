use std::path::Path;
use std::process::Command;

use aethyme_testkit::bins::{aethyme_bin, engine_bin};
use aethyme_testkit::paths::rust_workspace_root;

const PRODUCTION_CRATES: &[&str] = &[
    "aethyme-broker",
    "aethyme-cli",
    "aethyme-engine",
    "aethyme-enhance",
    "aethyme-graph-indexer",
    "aethyme-graph-schema",
    "aethyme-graph-storage",
    "aethyme-producers",
    "aethyme-quality",
];

fn manifest(path: &Path) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn product_version() -> String {
    manifest(&rust_workspace_root().join("Cargo.toml"))["workspace"]["package"]["version"]
        .as_str()
        .expect("workspace.package.version must be a string")
        .to_string()
}

fn version_output(binary: &Path) -> String {
    let output = Command::new(binary).arg("--version").output().unwrap();
    assert!(
        output.status.success(),
        "{} --version failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn production_crates_and_binaries_share_the_release_version() {
    let root = rust_workspace_root();
    let expected = product_version();
    for crate_name in PRODUCTION_CRATES {
        let package =
            &manifest(&root.join("crates").join(crate_name).join("Cargo.toml"))["package"];
        assert_eq!(
            package["version"]["workspace"].as_bool(),
            Some(true),
            "{crate_name} must inherit workspace.package.version"
        );
    }

    let router = version_output(&aethyme_bin());
    let engine = version_output(&engine_bin());
    assert_eq!(router.split_whitespace().nth(1), Some(expected.as_str()));
    assert_eq!(engine.split_whitespace().nth(1), Some(expected.as_str()));

    if let Ok(tag) = std::env::var("AETHYME_RELEASE_TAG") {
        assert_eq!(tag, format!("v{expected}"));
        for output in [router.as_str(), engine.as_str()] {
            let embedded_tag = output
                .split_once('(')
                .and_then(|(_, details)| details.split_whitespace().next());
            assert_eq!(embedded_tag, Some(tag.as_str()), "{output}");
        }
    }
}

/// A version bump must carry the committed graph engine pin with it.
///
/// `graph_integrity` compares `.aethyme/engine-version` against the verifier's
/// own `CARGO_PKG_VERSION` and returns `Incompatible` when they differ, so a
/// release that moves the workspace version and leaves the pin behind makes
/// every subsequent `broker submit` refuse. The refusal names
/// `aethyme graph refresh plan`, and that command deliberately will not help:
/// it never rewrites the pin, because the pin records which engine authored
/// the fragments and rewriting it would let a mismatched engine claim
/// authorship silently.
///
/// So the pin can only move as a deliberate act of the release, and nothing
/// enforced that until this test. v0.7.24 shipped with the pin left at 0.7.23
/// and blocked graph verification for the whole repository.
#[test]
fn the_graph_engine_pin_moves_with_the_release_version() {
    let pin_path = aethyme_testkit::paths::repo_root().join(".aethyme/engine-version");
    let pinned = std::fs::read_to_string(&pin_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", pin_path.display()));
    assert_eq!(
        pinned.trim(),
        product_version(),
        "committed .aethyme/engine-version is {} but the workspace is {}; bump the pin and \
         regenerate the committed graph fragments in the same commit as the version, or \
         graph integrity refuses every submission and `graph refresh` will not fix it",
        pinned.trim(),
        product_version()
    );
}

#[test]
fn release_workflow_smokes_the_installed_archive_contract() {
    let workflow = std::fs::read_to_string(
        aethyme_testkit::paths::repo_root().join(".github/workflows/release.yml"),
    )
    .unwrap();
    let smoke = workflow
        .split("- name: Smoke installed archive")
        .nth(1)
        .and_then(|tail| tail.split("- name: Upload artifact").next())
        .expect("release workflow must smoke each matrix archive before upload");

    for command in [
        "tar -xzf",
        "\"$smoke_root/aethyme\" --version",
        "\"$smoke_root/aethyme-engine-cli\" --version",
        "\"$smoke_root/aethyme\" broker quick-test",
        "\"$smoke_root/aethyme\" graph status",
        "\"$smoke_root/aethyme\" graph refresh plan",
        "\"$smoke_root/aethyme\" graph refresh execute",
        ".aethyme/graph_store.redb",
    ] {
        assert!(smoke.contains(command), "smoke step is missing {command}");
    }
}

#[test]
fn release_workflow_renders_the_homebrew_formula_from_the_manifest() {
    let workflow = std::fs::read_to_string(
        aethyme_testkit::paths::repo_root().join(".github/workflows/release.yml"),
    )
    .unwrap();
    let manifest_position = workflow.find("--example release_manifest").unwrap();
    let formula_position = workflow.find("--example homebrew_formula").unwrap();

    assert!(manifest_position < formula_position);
    assert!(workflow.contains("*-*) release_channel=preview"));
    assert!(workflow.contains("--channel \"$release_channel\""));
    assert!(workflow.contains("if [ \"$release_channel\" = stable ]; then"));
    assert!(workflow.contains("--manifest \"$GITHUB_WORKSPACE/dist/release-manifest.json\""));
    assert!(workflow.contains("--output \"$GITHUB_WORKSPACE/dist/aethyme.rb\""));
    assert!(workflow.contains("prerelease: ${{ contains(github.ref_name, '-') }}"));
}

#[test]
fn reviewed_homebrew_formula_installs_the_published_binary_pair() {
    let formula = std::fs::read_to_string(
        aethyme_testkit::paths::repo_root().join("packaging/homebrew/Formula/aethyme.rb"),
    )
    .unwrap();

    assert!(formula.contains("bin.install \"aethyme\", \"aethyme-engine-cli\""));
    assert_eq!(formula.matches("url \"").count(), 3);
    assert_eq!(formula.matches("sha256 \"").count(), 3);
    assert!(formula.contains("depends_on arch: :x86_64"));
    assert!(formula.contains("system bin/\"aethyme\", \"broker\", \"quick-test\""));
    assert!(!formula.contains("crates.io"));
}

#[test]
fn release_installer_delegates_pair_activation_to_the_native_transaction() {
    let installer =
        std::fs::read_to_string(aethyme_testkit::paths::repo_root().join("install.sh")).unwrap();
    let bootstrap = installer
        .split("\"$payload/aethyme\" update bootstrap")
        .nth(1)
        .expect("installer must delegate activation to the staged router");

    for argument in ["--payload", "--install-dir", "--manifest", "--target"] {
        assert!(bootstrap.contains(argument), "bootstrap omitted {argument}");
    }
    assert!(!installer.contains("mv \"$engine_stage\""));
    assert!(!installer.contains("mv \"$router_stage\""));
}

#[test]
#[cfg(unix)]
fn installer_authenticates_archive_before_running_payload() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    fn script(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let bin = root.join("bin");
    let payload = root.join("payload");
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir(&payload).unwrap();
    script(
        &bin.join("uname"),
        "#!/bin/sh\ncase $1 in -s) echo Linux;; -m) echo x86_64;; esac\n",
    );
    script(
        &bin.join("curl"),
        r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) destination="$2"; shift 2;;
        --*) shift;;
        *) url="$1"; shift;;
    esac
done
cp "$INSTALL_FIXTURE/${url##*/}" "$destination"
"#,
    );
    // Stub signature verification to isolate the artifact-binding contract;
    // this is not a test of Cosign's cryptographic implementation.
    script(&bin.join("cosign"), "#!/bin/sh\nexit \"$SIGNATURE_EXIT\"\n");
    for name in ["aethyme", "aethyme-engine-cli"] {
        script(
            &payload.join(name),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$EXECUTED_PAYLOAD\"\necho aethyme 0.0.1\n",
        );
    }
    let archive = "aethyme-v0.0.1-x86_64-unknown-linux-gnu.tar.gz";
    assert!(
        Command::new("tar")
            .args(["-czf"])
            .arg(root.join(archive))
            .arg("-C")
            .arg(&payload)
            .args(["aethyme", "aethyme-engine-cli"])
            .status()
            .unwrap()
            .success()
    );
    let actual = format!(
        "{:x}",
        Sha256::digest(std::fs::read(root.join(archive)).unwrap())
    );
    // A matching unsigned checksum must not override the manifest digest.
    std::fs::write(
        root.join(format!("{archive}.sha256")),
        format!("{actual}  {archive}\n"),
    )
    .unwrap();
    std::fs::write(root.join("release-manifest.sigstore.json"), "{}").unwrap();
    let installer = aethyme_testkit::paths::repo_root().join("install.sh");
    let installer_digest = format!("{:x}", Sha256::digest(std::fs::read(&installer).unwrap()));
    for case in [
        "valid",
        "mismatch",
        "duplicate",
        "missing",
        "bad-signature",
        "bad-installer",
    ] {
        let artifact = serde_json::json!({
            "archive": archive, "target": "x86_64-unknown-linux-gnu",
            "sha256": if case == "mismatch" { "0".repeat(64) } else { actual.clone() }
        });
        let artifacts = match case {
            "duplicate" => vec![artifact.clone(), artifact],
            "missing" => vec![],
            _ => vec![artifact],
        };
        let manifest = serde_json::json!({
            "version": "0.0.1", "release_channel": "stable", "artifacts": artifacts,
            "installer": { "sha256": if case == "bad-installer" { "0".repeat(64) } else { installer_digest.clone() } }
        });
        // Compact JSON deliberately exercises real parsing, not line matching.
        std::fs::write(root.join("release-manifest.json"), manifest.to_string()).unwrap();
        let executed = root.join(format!("executed-{case}"));
        let output = Command::new("sh")
            .arg(&installer)
            .arg("--verify-signature")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("AETHYME_RELEASE_BASE_URL", "https://fixture.invalid")
            .env("AETHYME_INSTALL_DIR", root.join("installed"))
            .env("INSTALL_FIXTURE", root)
            .env("EXECUTED_PAYLOAD", &executed)
            .env(
                "SIGNATURE_EXIT",
                if case == "bad-signature" { "1" } else { "0" },
            )
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            case == "valid",
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            executed.exists(),
            case == "valid",
            "payload execution in {case}"
        );
    }
}

#[test]
fn release_notes_publish_migration_compatibility_rollback_and_known_issues() {
    let root = aethyme_testkit::paths::repo_root();
    let workflow = std::fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let version = product_version();
    let guide_path = format!("packages/aethyme/docs/guides/upgrading-to-v{version}.md");
    // The release body is the guide named by the tag, so a release needs no
    // workflow edit; the guide for the current version must still exist.
    assert!(workflow.contains(
        "body_path: packages/aethyme/docs/guides/upgrading-to-${{ github.ref_name }}.md"
    ));

    let guide = std::fs::read_to_string(root.join(&guide_path)).unwrap();
    for heading in [
        "## Compatibility",
        "## Before upgrading",
        "## Install or update",
        "## Migrate and verify",
        "## Rollback",
        "## Known issues",
    ] {
        assert!(
            guide.contains(heading),
            "upgrade guide is missing {heading}"
        );
    }

    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    assert!(changelog.contains(&format!("## [{version}] - ")));
    assert!(changelog.contains("release-manifest.json"));
}
