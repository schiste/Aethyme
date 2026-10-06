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
    assert!(workflow.contains("--tag \"$REF_NAME\""));
    assert!(workflow.contains("--source-sha \"$source_sha\""));
    assert!(workflow.contains("--output \"$GITHUB_WORKSPACE/dist/aethyme.rb\""));
    assert!(workflow.contains("prerelease: ${{ contains(github.ref_name, '-') }}"));
}

#[test]
#[cfg(unix)]
fn homebrew_tap_publication_preflights_broker_writes_and_verifies_readback() {
    use std::os::unix::fs::PermissionsExt;

    fn formula(version: &str, digest_byte: char) -> String {
        let version_tag = format!("v{version}");
        let digest = digest_byte.to_string().repeat(64);
        format!(
            "class Aethyme < Formula\n  version \"{version}\"\n\n  on_macos do\n    if Hardware::CPU.arm?\n      url \"https://github.com/schiste/Aethyme/releases/download/{version_tag}/aethyme-{version_tag}-aarch64-apple-darwin.tar.gz\"\n      sha256 \"{digest}\"\n    else\n      url \"https://github.com/schiste/Aethyme/releases/download/{version_tag}/aethyme-{version_tag}-x86_64-apple-darwin.tar.gz\"\n      sha256 \"{digest}\"\n    end\n  end\n\n  on_linux do\n    url \"https://github.com/schiste/Aethyme/releases/download/{version_tag}/aethyme-{version_tag}-x86_64-unknown-linux-gnu.tar.gz\"\n    sha256 \"{digest}\"\n  end\nend\n"
        )
    }

    fn git_blob_sha(path: &Path) -> String {
        let output = Command::new("git")
            .args(["hash-object"])
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn base64(path: &Path) -> String {
        let output = Command::new("openssl")
            .args(["base64", "-A", "-in"])
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let fixture = tempfile::tempdir().unwrap();
    let formula_path = fixture.path().join("aethyme.rb");
    let old_formula_path = fixture.path().join("old-aethyme.rb");
    std::fs::write(&formula_path, formula("0.8.21", 'a')).unwrap();
    std::fs::write(&old_formula_path, formula("0.8.20", 'b')).unwrap();
    let expected_file_sha = git_blob_sha(&old_formula_path);
    let candidate_file_sha = git_blob_sha(&formula_path);
    let old_content = base64(&old_formula_path);
    let target_content = base64(&formula_path);
    let real_openssl = Command::new("sh")
        .args(["-c", "command -v openssl"])
        .output()
        .unwrap();
    assert!(real_openssl.status.success());
    let real_openssl = String::from_utf8(real_openssl.stdout)
        .unwrap()
        .trim()
        .to_string();

    let bin = fixture.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state_path = fixture.path().join("published");
    let call_log = fixture.path().join("aethyme-calls");
    executable(
        &bin.join("gh"),
        r#"#!/bin/sh
set -eu
if [ "$1" = auth ]; then
    [ "${TEST_AUTH_FAIL:-false}" != true ] || exit 1
    exit 0
fi
[ "$1" = api ] || exit 2
endpoint=$2
shift 2
selector=
while [ "$#" -gt 0 ]; do
    if [ "$1" = --jq ]; then selector=$2; shift 2; else shift; fi
done
case "$endpoint" in
    repos/schiste/homebrew-tap)
        [ "$selector" = .default_branch ] || exit 2
        printf 'main\n'
        ;;
    repos/schiste/homebrew-tap/branches/main)
        [ "$selector" = .commit.sha ] || exit 2
        printf '%s\n' "$TEST_BRANCH_SHA"
        ;;
    'repos/schiste/homebrew-tap/contents/Formula/aethyme.rb?ref=main')
        if [ -f "$TEST_PUBLICATION_STATE" ]; then
            file_sha=$TEST_TARGET_SHA
            content=$TEST_TARGET_CONTENT
        else
            file_sha=$TEST_OLD_SHA
            content=$TEST_OLD_CONTENT
        fi
        case "$selector" in
            .sha) printf '%s\n' "$file_sha" ;;
            .content) printf '%s\n' "$content" ;;
            *) exit 2 ;;
        esac
        ;;
    repos/schiste/homebrew-tap/commits\?path=Formula/aethyme.rb\&sha=main\&per_page=1)
        [ "$selector" = '.[0].sha' ] || exit 2
        printf '%s\n' "$TEST_COMMIT_SHA"
        ;;
    *) printf 'unexpected gh API endpoint: %s\n' "$endpoint" >&2; exit 2 ;;
esac
"#,
    );
    executable(
        &bin.join("openssl"),
        r#"#!/bin/sh
set -eu
[ "${TEST_OPENSSL_FAIL:-false}" != true ] || exit 1
exec "$TEST_REAL_OPENSSL" "$@"
"#,
    );
    executable(
        &bin.join("aethyme"),
        r#"#!/bin/sh
set -eu
[ "$1" = broker ] && [ "$2" = advanced ] && [ "$3" = gh ] || exit 2
printf '%s\n' "$@" >> "$TEST_AETHYME_CALL_LOG"
[ "${TEST_WRITE_FAIL:-false}" != true ] || exit 7
: > "$TEST_PUBLICATION_STATE"
"#,
    );

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let script = aethyme_testkit::paths::repo_root().join("scripts/publish-homebrew-tap.sh");
    let common_args = vec![
        "--formula".to_string(),
        formula_path.display().to_string(),
        "--tag".to_string(),
        "v0.8.21".to_string(),
        "--release-repo".to_string(),
        "schiste/Aethyme".to_string(),
        "--tap-repo".to_string(),
        "schiste/homebrew-tap".to_string(),
        "--branch".to_string(),
        "main".to_string(),
        "--expected-file-sha".to_string(),
        expected_file_sha.clone(),
    ];
    let run = |args: &[String], auth_fail: bool, encoding_fail: bool, write_fail: bool| {
        let mut command = Command::new("sh");
        command
            .arg(&script)
            .args(args)
            .env("PATH", &path)
            .env("GH_TOKEN", "fixture-token")
            .env("TEST_AUTH_FAIL", if auth_fail { "true" } else { "false" })
            .env(
                "TEST_OPENSSL_FAIL",
                if encoding_fail { "true" } else { "false" },
            )
            .env("TEST_WRITE_FAIL", if write_fail { "true" } else { "false" })
            .env("TEST_REAL_OPENSSL", &real_openssl)
            .env("AETHYME_BIN", bin.join("aethyme"))
            .env("TEST_PUBLICATION_STATE", &state_path)
            .env("TEST_AETHYME_CALL_LOG", &call_log)
            .env("TEST_OLD_SHA", &expected_file_sha)
            .env("TEST_TARGET_SHA", &candidate_file_sha)
            .env("TEST_OLD_CONTENT", &old_content)
            .env("TEST_TARGET_CONTENT", &target_content)
            .env(
                "TEST_BRANCH_SHA",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .env(
                "TEST_COMMIT_SHA",
                "cccccccccccccccccccccccccccccccccccccccc",
            )
            .output()
            .unwrap()
    };

    let mut dry_run_args = common_args.clone();
    dry_run_args.push("--dry-run".to_string());
    let dry_run = run(&dry_run_args, false, false, false);
    assert!(
        dry_run.status.success(),
        "preflight failed: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    assert!(String::from_utf8_lossy(&dry_run.stdout).contains("Preflight passed"));
    assert!(!state_path.exists(), "dry-run must not write the tap");
    assert!(
        !call_log.exists(),
        "dry-run must not create a broker operation"
    );

    let mut read_only_preflight = common_args.clone();
    read_only_preflight.push("--dry-run".to_string());
    let auth_failure = run(&read_only_preflight, true, false, false);
    assert!(!auth_failure.status.success());
    assert!(String::from_utf8_lossy(&auth_failure.stderr).contains("GitHub authentication failed"));
    assert!(
        !call_log.exists(),
        "auth failure must precede broker operation creation"
    );

    let encoding_failure = run(&read_only_preflight, false, true, false);
    assert!(!encoding_failure.status.success());
    assert!(
        String::from_utf8_lossy(&encoding_failure.stderr).contains("could not encode the formula")
    );
    assert!(
        !call_log.exists(),
        "encoding failure must precede broker operation creation"
    );

    for (name, invalid_formula) in [
        ("wrong-version", formula("0.8.20", 'a')),
        (
            "wrong-url",
            formula("0.8.21", 'a')
                .replace("releases/download/v0.8.21/", "releases/download/v0.8.20/"),
        ),
        (
            "bad-digest",
            formula("0.8.21", 'a').replace(
                &format!("sha256 \"{}\"", "a".repeat(64)),
                "sha256 \"not-a-digest\"",
            ),
        ),
    ] {
        let invalid_path = fixture.path().join(format!("{name}.rb"));
        std::fs::write(&invalid_path, invalid_formula).unwrap();
        let mut invalid_args = read_only_preflight.clone();
        let formula_index = invalid_args
            .iter()
            .position(|argument| argument == "--formula")
            .unwrap()
            + 1;
        invalid_args[formula_index] = invalid_path.display().to_string();
        let invalid = run(&invalid_args, false, false, false);
        assert!(!invalid.status.success(), "{name} unexpectedly passed");
        assert!(
            String::from_utf8_lossy(&invalid.stderr)
                .contains("formula version, release URLs, or SHA-256 digests"),
            "{name} was not rejected by formula validation: {}",
            String::from_utf8_lossy(&invalid.stderr)
        );
    }
    assert!(
        !state_path.exists(),
        "invalid preflight inputs must not write the tap"
    );
    assert!(
        !call_log.exists(),
        "invalid preflight inputs must not create a broker operation"
    );

    let mut stale_args = common_args.clone();
    let sha_index = stale_args
        .iter()
        .position(|argument| argument == "--expected-file-sha")
        .unwrap()
        + 1;
    stale_args[sha_index] = "dddddddddddddddddddddddddddddddddddddddd".to_string();
    stale_args.push("--dry-run".to_string());
    let stale = run(&stale_args, false, false, false);
    assert!(!stale.status.success());
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale expected file SHA"),
        "unexpected stale-SHA error: {}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(
        !state_path.exists(),
        "a stale SHA must not reach the broker write"
    );

    let mut wrong_branch_args = common_args.clone();
    let branch_index = wrong_branch_args
        .iter()
        .position(|argument| argument == "--branch")
        .unwrap()
        + 1;
    wrong_branch_args[branch_index] = "release".to_string();
    wrong_branch_args.push("--dry-run".to_string());
    let wrong_branch = run(&wrong_branch_args, false, false, false);
    assert!(!wrong_branch.status.success());
    assert!(
        String::from_utf8_lossy(&wrong_branch.stderr)
            .contains("is not schiste/homebrew-tap default branch")
    );
    assert!(
        !state_path.exists(),
        "a wrong target branch must not reach the broker write"
    );

    let mut publish_args = common_args.clone();
    publish_args.extend(["--session".to_string(), "42".to_string()]);
    let rejected_write = run(&publish_args, false, false, true);
    assert!(!rejected_write.status.success());
    assert!(
        String::from_utf8_lossy(&rejected_write.stderr)
            .contains("inspect and reconcile the broker operation before retrying")
    );
    assert!(
        !state_path.exists(),
        "a refused write must leave the tap formula unchanged"
    );

    let published = run(&publish_args, false, false, false);
    assert!(
        published.status.success(),
        "publication failed: {}\n{}",
        String::from_utf8_lossy(&published.stdout),
        String::from_utf8_lossy(&published.stderr)
    );
    let stdout = String::from_utf8_lossy(&published.stdout);
    assert!(stdout.contains("Published and verified"));
    assert!(stdout.contains("cccccccccccccccccccccccccccccccccccccccc"));
    assert!(state_path.exists());

    let broker_call = std::fs::read_to_string(&call_log).unwrap();
    assert!(broker_call.lines().any(|line| line == "advanced"));
    assert!(broker_call.lines().any(|line| line == "gh"));
    assert!(broker_call.lines().any(|line| line == "--repo"));
    assert!(
        broker_call
            .lines()
            .any(|line| line == "schiste/homebrew-tap")
    );
    assert!(broker_call.lines().any(|line| line == "--session"));
    assert!(broker_call.lines().any(|line| line == "42"));
    assert!(broker_call.lines().any(|line| line == "PUT"));
    assert!(broker_call.contains(&format!("sha={expected_file_sha}")));
    assert!(broker_call.contains("branch=main"));
    assert!(
        broker_call
            .lines()
            .any(|line| line == format!("content={target_content}"))
    );

    let repeat = run(&publish_args, false, false, false);
    assert!(
        repeat.status.success(),
        "repeat publication failed: {}",
        String::from_utf8_lossy(&repeat.stderr)
    );
    assert!(String::from_utf8_lossy(&repeat.stdout).contains("already matches"));
    assert_eq!(std::fs::read_to_string(&call_log).unwrap(), broker_call);
}

#[test]
fn homebrew_release_workflow_uses_signed_manifest_and_brokered_publication() {
    let root = aethyme_testkit::paths::repo_root();
    let workflow = std::fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let script = std::fs::read_to_string(root.join("scripts/publish-homebrew-tap.sh")).unwrap();

    assert!(workflow.contains("cosign verify-blob"));
    assert!(workflow.contains("--source-sha \"$SOURCE_SHA\""));
    assert!(workflow.contains("broker advanced exec --session"));
    assert!(workflow.contains("--expected-file-sha"));
    assert!(workflow.contains("Finish broker publication session"));
    assert!(!workflow.contains("git push origin HEAD:main"));
    assert!(!workflow.contains("repository: schiste/homebrew-tap"));

    assert!(script.contains("openssl base64 -A"));
    assert!(script.contains("broker advanced gh"));
    assert!(script.contains("--method PUT"));
    assert!(script.contains("cmp -s"));
    assert!(script.contains("--dry-run"));
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

/// The release body is rendered from `CHANGELOG.md` and `UPGRADING.md`
/// (see `aethyme_testkit::release_notes`), not read from a per-version file,
/// so a release needs no workflow edit and no new guide unless it is breaking.
#[test]
fn release_workflow_renders_its_body_from_changelog_and_upgrading() {
    let workflow = std::fs::read_to_string(
        aethyme_testkit::paths::repo_root().join(".github/workflows/release.yml"),
    )
    .unwrap();
    let render = workflow
        .find("-p aethyme-testkit --example release_notes")
        .expect("release workflow must render the body with the release_notes example");
    let publish = workflow.find("- name: Create or update release").unwrap();
    assert!(
        render < publish,
        "the body must be rendered before publishing"
    );
    for fragment in [
        "--repo \"$GITHUB_WORKSPACE\"",
        "--tag \"$REF_NAME\"",
        "--output \"$RUNNER_TEMP/release-notes.md\"",
        "body_path: ${{ runner.temp }}/release-notes.md",
    ] {
        assert!(
            workflow.contains(fragment),
            "release workflow is missing {fragment}"
        );
    }
    assert!(
        !workflow.contains("upgrading-to-"),
        "per-version upgrade guides were merged into UPGRADING.md"
    );
}

#[test]
fn every_release_pairs_its_breaking_marker_with_an_upgrade_section() {
    let root = aethyme_testkit::paths::repo_root();
    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    let upgrading = std::fs::read_to_string(root.join("UPGRADING.md")).unwrap();
    let problems = aethyme_testkit::release_notes::contract_violations(&changelog, &upgrading);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(changelog.contains("release-manifest.json"));
}

#[test]
fn the_current_version_renders_release_notes() {
    let root = aethyme_testkit::paths::repo_root();
    let version = product_version();
    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    let upgrading = std::fs::read_to_string(root.join("UPGRADING.md")).unwrap();
    let body =
        aethyme_testkit::release_notes::render(&format!("v{version}"), &changelog, &upgrading)
            .unwrap_or_else(|error| {
                panic!(
                    "release notes for v{version} (the workspace version) do not render: {error}"
                )
            });
    let breaking =
        aethyme_testkit::release_notes::upgrading_sections(&upgrading).contains_key(&version);
    assert_eq!(body.contains("\n## Upgrading\n"), breaking);
}
