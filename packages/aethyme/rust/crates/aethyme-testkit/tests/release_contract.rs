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
        assert!(router.contains(&format!("({tag})")), "{router}");
        assert!(engine.contains(&format!("({tag})")), "{engine}");
    }
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
    assert!(workflow.contains("--manifest \"$GITHUB_WORKSPACE/dist/release-manifest.json\""));
    assert!(workflow.contains("--output \"$GITHUB_WORKSPACE/dist/aethyme.rb\""));
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
fn release_notes_publish_migration_compatibility_rollback_and_known_issues() {
    let root = aethyme_testkit::paths::repo_root();
    let workflow = std::fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let guide_path = "packages/aethyme/docs/guides/upgrading-to-v0.2.0.md";
    assert!(workflow.contains(&format!("body_path: {guide_path}")));

    let guide = std::fs::read_to_string(root.join(guide_path)).unwrap();
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
    assert!(changelog.contains("## [0.2.0] - Unreleased"));
    assert!(changelog.contains("release-manifest.json"));
}
