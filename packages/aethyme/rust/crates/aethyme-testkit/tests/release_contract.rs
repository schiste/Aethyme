use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_testkit::bins::{aethyme_bin, engine_bin};
use aethyme_testkit::paths::rust_workspace_root;

const RELEASE_BINARY_DIR_ENV: &str = "AETHYME_RELEASE_BINARY_DIR";

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

fn workflow_step_block<'a>(workflow: &'a str, name: &str) -> &'a str {
    let marker = format!("      - name: {name}\n");
    let start = workflow
        .find(&marker)
        .unwrap_or_else(|| panic!("workflow step {name:?} is missing"));
    let tail = &workflow[start..];
    let end = tail[marker.len()..]
        .find("\n      - name:")
        .map(|end| marker.len() + end)
        .unwrap_or(tail.len());
    &tail[..end]
}

fn workflow_step_script(workflow: &str, name: &str) -> String {
    let block = workflow_step_block(workflow, name);
    let marker = "        run: |\n";
    let start = block
        .find(marker)
        .unwrap_or_else(|| panic!("workflow step {name:?} has no literal run block"))
        + marker.len();
    let mut script = String::new();
    for line in block[start..].lines() {
        if line.is_empty() {
            script.push('\n');
        } else if let Some(line) = line.strip_prefix("          ") {
            script.push_str(line);
            script.push('\n');
        } else {
            break;
        }
    }
    assert!(
        !script.is_empty(),
        "workflow step {name:?} has an empty run block"
    );
    script
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

fn release_binary_filename(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn version_binary(
    name: &str,
    release_binary_dir: Option<PathBuf>,
    build_if_missing: impl FnOnce() -> PathBuf,
) -> Result<PathBuf, String> {
    let Some(release_binary_dir) = release_binary_dir else {
        return Ok(build_if_missing());
    };

    let path = release_binary_dir.join(release_binary_filename(name));
    if !path.is_file() {
        return Err(format!(
            "{RELEASE_BINARY_DIR_ENV} is set but the release binary is missing: {}",
            path.display()
        ));
    }
    Ok(path)
}

#[test]
fn release_binary_override_selects_existing_artifact_without_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let expected = directory.path().join(release_binary_filename("aethyme"));
    std::fs::write(&expected, b"fixture binary").unwrap();

    let mut fallback_called = false;
    let actual = version_binary("aethyme", Some(directory.path().to_path_buf()), || {
        fallback_called = true;
        PathBuf::from("unexpected-fallback")
    })
    .unwrap();

    assert_eq!(actual, expected);
    assert!(!fallback_called);
}

#[test]
fn release_binary_override_refuses_missing_artifact_without_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let mut fallback_called = false;
    let error = version_binary(
        "aethyme-engine-cli",
        Some(directory.path().to_path_buf()),
        || {
            fallback_called = true;
            PathBuf::from("unexpected-fallback")
        },
    )
    .unwrap_err();

    assert!(error.contains(RELEASE_BINARY_DIR_ENV));
    assert!(error.contains("aethyme-engine-cli"));
    assert!(!fallback_called);
}

fn is_full_git_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn release_metadata_error(
    tag: &str,
    expected_version: &str,
    source_commit: &str,
    binary: &str,
    output: &str,
) -> Option<String> {
    if tag != format!("v{expected_version}") {
        return Some(format!(
            "tag {tag:?} does not match expected version {expected_version}"
        ));
    }
    if !is_full_git_sha(source_commit) {
        return Some(format!(
            "release source commit is not a full Git SHA: {source_commit:?}"
        ));
    }
    if output.contains('\n') {
        return Some("version output must be a single line".to_owned());
    }

    let prefix = format!("{binary} {expected_version} (build_revision=");
    let Some(metadata) = output.strip_prefix(&prefix) else {
        return Some(format!(
            "version output must begin with {prefix:?}; got {output:?}"
        ));
    };
    let Some((revision, metadata)) = metadata.split_once(" build_commit=") else {
        return Some(format!(
            "version output has no build_commit field: {output:?}"
        ));
    };
    let Some((commit, build_date)) = metadata.split_once(") build_date=") else {
        return Some(format!(
            "version output has no build_date field: {output:?}"
        ));
    };
    if revision != source_commit {
        return Some(format!(
            "build_revision must equal the clean source SHA {source_commit}, got {revision}"
        ));
    }
    if commit != source_commit {
        return Some(format!(
            "build_commit must equal the tagged source SHA {source_commit}, got {commit}"
        ));
    }
    if build_date.is_empty() || !build_date.ends_with('Z') {
        return Some(format!(
            "build_date must be a UTC timestamp, got {build_date:?}"
        ));
    }
    None
}

#[test]
fn repository_engine_pin_matches_the_newline_terminated_release_version() {
    let pin = aethyme_testkit::paths::repo_root().join(".aethyme/engine-version");
    assert_eq!(
        std::fs::read(pin).unwrap(),
        format!("{}\n", product_version()).into_bytes(),
        "the engine pin must contain the exact workspace version followed by a newline"
    );
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

    let release_binary_dir = std::env::var_os(RELEASE_BINARY_DIR_ENV).map(PathBuf::from);
    let router_binary = version_binary("aethyme", release_binary_dir.clone(), aethyme_bin)
        .unwrap_or_else(|error| panic!("{error}"));
    let engine_binary = version_binary("aethyme-engine-cli", release_binary_dir, engine_bin)
        .unwrap_or_else(|error| panic!("{error}"));
    let router = version_output(&router_binary);
    let engine = version_output(&engine_binary);
    assert_eq!(router.split_whitespace().nth(1), Some(expected.as_str()));
    assert_eq!(engine.split_whitespace().nth(1), Some(expected.as_str()));

    match (
        std::env::var("AETHYME_RELEASE_TAG"),
        std::env::var("AETHYME_RELEASE_COMMIT"),
    ) {
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {}
        (Ok(tag), Ok(source_commit)) => {
            for (binary, output) in [
                ("aethyme", router.as_str()),
                ("aethyme-engine-cli", engine.as_str()),
            ] {
                if let Some(error) =
                    release_metadata_error(&tag, &expected, &source_commit, binary, output)
                {
                    panic!("{binary} release metadata is invalid: {error}; output: {output}");
                }
            }
        }
        _ => panic!("AETHYME_RELEASE_TAG and AETHYME_RELEASE_COMMIT must be set together"),
    }
}

#[test]
fn release_tag_metadata_fixture_requires_exact_clean_source_identity() {
    let version = "0.8.27";
    let tag = "v0.8.27";
    let source_commit = "0123456789abcdef0123456789abcdef01234567";
    let router = format!(
        "aethyme {version} (build_revision={source_commit} build_commit={source_commit}) build_date=2026-10-21T12:34:56Z"
    );
    let engine = format!(
        "aethyme-engine-cli {version} (build_revision={source_commit} build_commit={source_commit}) build_date=2026-10-21T12:34:56Z"
    );

    for (binary, output) in [
        ("aethyme", router.as_str()),
        ("aethyme-engine-cli", engine.as_str()),
    ] {
        assert_eq!(
            release_metadata_error(tag, version, source_commit, binary, output),
            None,
            "{output}"
        );
    }

    let dirty = router.replace(
        &format!("build_revision={source_commit}"),
        &format!("build_revision={source_commit}-dirty"),
    );
    assert!(release_metadata_error(tag, version, source_commit, "aethyme", &dirty).is_some());

    let other_commit = "fedcba9876543210fedcba9876543210fedcba98";
    let mismatched = router.replace(
        &format!("build_commit={source_commit}"),
        &format!("build_commit={other_commit}"),
    );
    assert!(release_metadata_error(tag, version, source_commit, "aethyme", &mismatched).is_some());
    assert!(
        release_metadata_error("v0.8.26", version, source_commit, "aethyme", &router).is_some()
    );
    assert!(
        release_metadata_error("v0.8.28", "0.8.28", source_commit, "aethyme", &router).is_some()
    );
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
fn release_targets_exercises_native_linux_arm64_runtime_contract() {
    let workflow = std::fs::read_to_string(
        aethyme_testkit::paths::repo_root().join(".github/workflows/release-targets.yml"),
    )
    .unwrap();
    assert!(workflow.contains("os: ubuntu-24.04-arm"));
    assert!(workflow.contains("target: aarch64-unknown-linux-gnu"));
    assert!(workflow.contains("if: matrix.target == 'aarch64-unknown-linux-gnu'"));

    for command in [
        "cargo test --locked -p aethyme-testkit --test release_installer",
        "cargo test --locked -p aethyme-broker --lib bootstrap_switches_the_pair_once_and_retains_one_rollback_bundle",
        "cargo test --locked -p aethyme-broker --lib failed_staged_quick_test_never_moves_the_active_pair",
        "cargo test --locked -p aethyme-broker --test broker two_session_worktrees_are_distinct_and_registered",
        "cargo test --locked -p aethyme-broker --test host_operations_e2e",
        "broker quick-test",
        "graph refresh plan",
        "graph refresh execute",
    ] {
        assert!(
            workflow.contains(command),
            "native Linux arm64 job is missing {command}"
        );
    }
}

#[test]
fn release_workflow_uses_ephemeral_checkouts_and_source_sha_contract() {
    let workflow = std::fs::read_to_string(
        aethyme_testkit::paths::repo_root().join(".github/workflows/release.yml"),
    )
    .unwrap();
    let checkout_steps: Vec<_> = workflow
        .split("\n      - ")
        .filter(|step| step.contains("uses: actions/checkout@"))
        .collect();
    assert_eq!(
        checkout_steps.len(),
        4,
        "review checkout coverage when it changes"
    );
    for step in checkout_steps {
        assert!(
            step.contains("persist-credentials: false"),
            "checkout must not persist the release token into Git config: {step}"
        );
    }

    let contract_step = workflow
        .split("- name: Verify release version contract")
        .nth(1)
        .and_then(|tail| tail.split("\n      - ").next())
        .expect("release workflow must run the release version contract");
    assert!(contract_step.contains("AETHYME_RELEASE_TAG: ${{ github.ref_name }}"));
    assert!(contract_step.contains("AETHYME_RELEASE_COMMIT: ${{ github.sha }}"));
    assert!(contract_step.contains(
        "AETHYME_RELEASE_BINARY_DIR: ${{ github.workspace }}/packages/aethyme/rust/target/${{ matrix.target }}/release"
    ));

    let matrix_release_build = workflow
        .find("cargo build --release --locked --target \"${{ matrix.target }}\"")
        .expect("release binaries must be built for the matrix target");
    let contract_step_position = workflow
        .find("- name: Verify release version contract")
        .expect("release version contract step must be present");
    assert!(
        matrix_release_build < contract_step_position,
        "the release contract must inspect binaries after they are built"
    );
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
fn homebrew_tap_publication_checks_writes_once_and_verifies_readback() {
    use std::os::unix::fs::PermissionsExt;

    // Shaped like the formula release.yml ships: no `version` line, because
    // Homebrew infers it from the URLs.
    fn formula(tag: &str, digest_byte: char) -> String {
        let digest = digest_byte.to_string().repeat(64);
        let target = |triple: &str| {
            format!(
                "      url \"https://github.com/schiste/Aethyme/releases/download/{tag}/aethyme-{tag}-{triple}.tar.gz\"\n      sha256 \"{digest}\"\n"
            )
        };
        format!(
            "class Aethyme < Formula\n  on_macos do\n    on_arm do\n{}    end\n  end\n  on_linux do\n    on_intel do\n{}    end\n  end\nend\n",
            target("aarch64-apple-darwin"),
            target("x86_64-unknown-linux-gnu"),
        )
    }

    fn git_blob_sha(path: &Path) -> String {
        let output = Command::new("git")
            .arg("hash-object")
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
    std::fs::write(&formula_path, formula("v0.8.21", 'a')).unwrap();
    std::fs::write(&old_formula_path, formula("v0.8.20", 'b')).unwrap();
    let old_sha = git_blob_sha(&old_formula_path);
    let target_sha = git_blob_sha(&formula_path);

    // A fake `gh` serving the tap from TEST_TAP_STATE (absent: the old
    // formula) and recording each PUT. TEST_WRITE=refuse fails without
    // writing; TEST_WRITE=lost writes but reports failure, like a dropped
    // response.
    let bin = fixture.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state_path = fixture.path().join("published");
    let put_log = fixture.path().join("puts");
    executable(
        &bin.join("gh"),
        r#"#!/bin/sh
set -eu
[ "$1" != auth ] || exit 0
[ "$1" = api ] || exit 2
endpoint=$2
shift 2
method=GET
selector=
fields=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --method) method=$2; shift 2 ;;
        --jq) selector=$2; shift 2 ;;
        --raw-field) fields="$fields $2"; shift 2 ;;
        *) printf 'unexpected gh argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
if [ "$method" = PUT ]; then
    [ "$endpoint" = repos/schiste/homebrew-tap/contents/Formula/aethyme.rb ] || exit 2
    printf '%s\n' "$fields" >> "$TEST_PUT_LOG"
    case "${TEST_WRITE:-ok}" in
        refuse) exit 1 ;;
        lost) : > "$TEST_TAP_STATE"; exit 1 ;;
        *) : > "$TEST_TAP_STATE" ;;
    esac
    exit 0
fi
case "$endpoint" in
    repos/schiste/homebrew-tap) printf 'main\n' ;;
    'repos/schiste/homebrew-tap/contents/Formula/aethyme.rb?ref=main')
        if [ -f "$TEST_TAP_STATE" ]; then
            sha=$TEST_TARGET_SHA; content=$TEST_TARGET_CONTENT
        else
            sha=$TEST_OLD_SHA; content=$TEST_OLD_CONTENT
        fi
        case "$selector" in
            .sha) printf '%s\n' "$sha" ;;
            .content) printf '%s\n' "$content" ;;
            *) exit 2 ;;
        esac
        ;;
    'repos/schiste/homebrew-tap/commits?path=Formula/aethyme.rb&sha=main&per_page=1')
        printf 'cccccccccccccccccccccccccccccccccccccccc\n' ;;
    *) printf 'unexpected gh API endpoint: %s\n' "$endpoint" >&2; exit 2 ;;
esac
"#,
    );

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let script = aethyme_testkit::paths::repo_root().join("scripts/publish-homebrew-tap.sh");
    let run = |formula: &Path, tag: &str, extra: &[&str], env: &[(&str, &str)]| {
        let mut command = Command::new("sh");
        command
            .arg(&script)
            .arg("--formula")
            .arg(formula)
            .args(["--tag", tag])
            .args(["--release-repo", "schiste/Aethyme"])
            .args(["--tap-repo", "schiste/homebrew-tap"])
            .args(extra)
            .env("PATH", &path)
            .env_remove("GITHUB_ACTIONS")
            .env("HOMEBREW_TAP_READBACK_DELAY", "0")
            .env("TEST_TAP_STATE", &state_path)
            .env("TEST_PUT_LOG", &put_log)
            .env("TEST_OLD_SHA", &old_sha)
            .env("TEST_TARGET_SHA", &target_sha)
            .env("TEST_OLD_CONTENT", base64(&old_formula_path))
            .env("TEST_TARGET_CONTENT", base64(&formula_path));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    };
    let stderr =
        |output: &std::process::Output| String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout =
        |output: &std::process::Output| String::from_utf8_lossy(&output.stdout).into_owned();
    let in_actions = [("GITHUB_ACTIONS", "true")];

    let dry_run = run(&formula_path, "v0.8.21", &["--dry-run"], &[]);
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    assert!(stdout(&dry_run).contains("Preflight passed"));
    assert!(!put_log.exists(), "a dry run never writes");

    let workstation = run(&formula_path, "v0.8.21", &[], &[]);
    assert!(!workstation.status.success());
    assert!(stderr(&workstation).contains("writes run only in the Homebrew tap workflow"));
    assert!(
        !put_log.exists(),
        "writes are refused outside GitHub Actions"
    );

    let invalid = [
        ("wrong-tag", formula("v0.8.20", 'a'), "v0.8.21"),
        (
            "bad-digest",
            formula("v0.8.21", 'a').replace(&"a".repeat(64), "not-a-digest"),
            "v0.8.21",
        ),
        (
            "wrong-version-line",
            formula("v0.8.21", 'a').replace(
                "class Aethyme < Formula\n",
                "class Aethyme < Formula\n  version \"0.8.20\"\n",
            ),
            "v0.8.21",
        ),
        ("preview-tag", formula("v0.9.0-rc.1", 'a'), "v0.9.0-rc.1"),
    ];
    for (name, contents, tag) in invalid {
        let invalid_path = fixture.path().join(format!("{name}.rb"));
        std::fs::write(&invalid_path, contents).unwrap();
        let output = run(&invalid_path, tag, &[], &in_actions);
        assert!(!output.status.success(), "{name} unexpectedly passed");
    }
    assert!(
        !put_log.exists(),
        "an invalid formula never reaches the write"
    );

    let refused = run(
        &formula_path,
        "v0.8.21",
        &[],
        &[in_actions[0], ("TEST_WRITE", "refuse")],
    );
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("re-run the workflow"),
        "{}",
        stderr(&refused)
    );
    assert!(!state_path.exists());
    let put = std::fs::read_to_string(&put_log).unwrap();
    assert!(
        put.contains(&format!("sha={old_sha}")),
        "the write carries the read blob SHA: {put}"
    );
    assert!(put.contains("branch=main"));
    assert!(put.contains(&format!("content={}", base64(&formula_path))));
    std::fs::remove_file(&put_log).unwrap();

    // A write that landed but lost its response is judged by the read-back.
    let lost = run(
        &formula_path,
        "v0.8.21",
        &[],
        &[in_actions[0], ("TEST_WRITE", "lost")],
    );
    assert!(lost.status.success(), "{}", stderr(&lost));
    assert!(stdout(&lost).contains("Published and verified"));
    assert!(stderr(&lost).contains("but the tap read-back matches"));
    std::fs::remove_file(&state_path).unwrap();
    std::fs::remove_file(&put_log).unwrap();

    let published = run(&formula_path, "v0.8.21", &[], &in_actions);
    assert!(published.status.success(), "{}", stderr(&published));
    assert!(stdout(&published).contains("cccccccccccccccccccccccccccccccccccccccc"));
    assert_eq!(
        std::fs::read_to_string(&put_log).unwrap().lines().count(),
        1
    );

    let repeat = run(&formula_path, "v0.8.21", &[], &in_actions);
    assert!(repeat.status.success(), "{}", stderr(&repeat));
    assert!(stdout(&repeat).contains("nothing to write"));
    assert_eq!(
        std::fs::read_to_string(&put_log).unwrap().lines().count(),
        1,
        "republishing the same formula does not write"
    );
}

#[test]
fn homebrew_tap_workflow_publishes_from_the_signed_manifest() {
    let root = aethyme_testkit::paths::repo_root();
    let release = std::fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let tap = std::fs::read_to_string(root.join(".github/workflows/homebrew-tap.yml")).unwrap();

    // release.yml hands stable tags to the reusable workflow, which is also
    // the retry path and the pull-request rehearsal.
    assert!(release.contains("uses: ./.github/workflows/homebrew-tap.yml"));
    assert!(release.contains("if: ${{ !contains(github.ref_name, '-') }}"));
    assert!(!release.contains("git push origin HEAD:main"));
    for trigger in ["workflow_call:", "workflow_dispatch:", "pull_request:"] {
        assert!(tap.contains(trigger), "homebrew-tap.yml lost {trigger}");
    }

    let verify = tap.find("cosign verify-blob").unwrap();
    let render = tap.find("--example homebrew_formula").unwrap();
    let publish = tap.find("Publish and verify formula").unwrap();
    assert!(verify < render && render < publish);
    assert!(tap.contains("release.yml@refs/tags/${TAG}"));
    assert!(tap.contains("--source-sha \"$(git rev-parse \"refs/tags/${TAG}^{commit}\")\""));

    // Only a publication sees the tap token; a rehearsal passes --dry-run
    // with the read-only job token.
    let rehearsal = &tap[tap
        .find("Rehearse publication against the live tap")
        .unwrap()..];
    assert!(rehearsal.contains("GH_TOKEN: ${{ github.token }}"));
    assert!(rehearsal.contains("--dry-run"));
    assert_eq!(tap.matches("secrets.HOMEBREW_TAP_TOKEN").count(), 2);
}

#[test]
#[cfg(unix)]
fn release_signing_uses_retry_safe_legacy_cosign_and_preserves_standard_bundle() {
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let repository = aethyme_testkit::paths::repo_root();
    let release =
        std::fs::read_to_string(repository.join(".github/workflows/release.yml")).unwrap();
    let homebrew =
        std::fs::read_to_string(repository.join(".github/workflows/homebrew-tap.yml")).unwrap();
    let legacy_install = workflow_step_block(&release, "Install retry-safe legacy Cosign signer");
    assert!(legacy_install.contains("cosign-release: v2.6.5"));
    assert!(legacy_install.contains("install-dir: ${{ runner.temp }}/cosign-v2.6.5"));
    let modern_install =
        workflow_step_block(&release, "Install standard-bundle converter and verifier");
    assert!(modern_install.contains("cosign-release: v3.1.3"));
    assert!(modern_install.contains("install-dir: ${{ runner.temp }}/cosign-v3.1.3"));
    let homebrew_install = workflow_step_block(&homebrew, "Install Cosign");
    assert!(homebrew_install.contains("cosign-release: v3.1.3"));
    let homebrew_verify =
        workflow_step_block(&homebrew, "Verify signed manifest and regenerate formula");
    assert!(homebrew_verify.contains("--certificate-identity"));
    assert!(homebrew_verify.contains("--certificate-oidc-issuer"));
    assert!(!homebrew_verify.contains("--ignore-tlog"));

    let sign_script = workflow_step_script(
        &release,
        "Sign release assets with retry-safe legacy Cosign",
    );
    let convert_script = workflow_step_script(
        &release,
        "Create standard bundles and verify release assets",
    );
    assert!(
        workflow_step_block(
            &release,
            "Sign release assets with retry-safe legacy Cosign"
        )
        .contains("COSIGN_V2: ${{ runner.temp }}/cosign-v2.6.5/cosign")
    );
    assert!(
        workflow_step_block(
            &release,
            "Create standard bundles and verify release assets"
        )
        .contains("COSIGN_V3: ${{ runner.temp }}/cosign-v3.1.3/cosign")
    );
    for forbidden in ["--ignore-tlog", "--tlog-upload=false", "--offline"] {
        assert!(!sign_script.contains(forbidden));
        assert!(!convert_script.contains(forbidden));
    }

    let fixture = tempfile::Builder::new()
        .prefix("aethyme cosign release contract ")
        .tempdir()
        .unwrap();
    let fixture_root = fixture.path();
    let runner_temp = fixture_root.join("runner temp");
    let dist = fixture_root.join("dist");
    std::fs::create_dir_all(&runner_temp).unwrap();
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("release-manifest.json"), "fixture manifest\n").unwrap();
    let zip_relative = "dist/aethyme-v0.8.29-x86_64-pc-windows-msvc.zip";
    std::fs::write(fixture_root.join(zip_relative), "fixture zip\n").unwrap();

    let legacy_cosign = runner_temp.join("cosign-v2.6.5/cosign");
    let modern_cosign = runner_temp.join("cosign-v3.1.3/cosign");
    std::fs::create_dir_all(legacy_cosign.parent().unwrap()).unwrap();
    std::fs::create_dir_all(modern_cosign.parent().unwrap()).unwrap();
    let trace = fixture_root.join("cosign trace.txt");
    executable(
        &legacy_cosign,
        r#"#!/bin/sh
set -eu
printf 'v2:%s\n' "${1:-}" >> "$COSIGN_TRACE"
[ "${1:-}" = sign-blob ] || exit 91
[ "$#" -eq 7 ] || exit 92
[ "$2" = --yes ] && [ "$3" = --new-bundle-format=false ] && [ "$4" = --use-signing-config=false ] && [ "$5" = --bundle ] || exit 93
bundle="$6"
artifact="$7"
case "$artifact" in
  dist/release-manifest.json|dist/aethyme-v0.8.29-x86_64-pc-windows-msvc.zip) ;;
  *) exit 94 ;;
esac
mkdir -p "$(dirname "$bundle")"
[ -f "$artifact" ] || exit 95
printf 'legacy bundle\n' > "$bundle"
"#,
    );
    executable(
        &modern_cosign,
        r#"#!/bin/sh
set -eu
for arg do
  case "$arg" in --ignore-tlog|--tlog-upload=false|--offline) exit 94 ;; esac
done
printf 'v3:%s %s\n' "${1:-}" "${2:-}" >> "$COSIGN_TRACE"
case "${1:-} ${2:-}" in
  "bundle create")
    shift 2
    artifact=
    bundle=
    out=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --artifact) shift; artifact="${1:?}" ;;
        --bundle) shift; bundle="${1:?}" ;;
        --out) shift; out="${1:?}" ;;
        *) exit 95 ;;
      esac
      shift
    done
    [ -f "$artifact" ] && [ -f "$bundle" ] && [ -n "$out" ] || exit 96
    printf 'standard bundle\n' > "$out"
    ;;
  verify-blob*)
    shift
    bundle=
    identity=
    issuer=
    artifact=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --bundle) shift; bundle="${1:?}" ;;
        --certificate-identity) shift; identity="${1:?}" ;;
        --certificate-oidc-issuer) shift; issuer="${1:?}" ;;
        --*) exit 97 ;;
        *) [ -z "$artifact" ] || exit 98; artifact="$1" ;;
      esac
      shift
    done
    expected_identity="https://github.com/${GITHUB_REPOSITORY}/.github/workflows/release.yml@refs/tags/${REF_NAME}"
    [ -f "$bundle" ] && [ -f "$artifact" ] || exit 99
    [ "$identity" = "$expected_identity" ] || exit 100
    [ "$issuer" = https://token.actions.githubusercontent.com ] || exit 101
    ;;
  *) exit 102 ;;
esac
"#,
    );

    let run_script = |script: &str| {
        Command::new("bash")
            .args(["-euo", "pipefail", "-c", script])
            .current_dir(fixture_root)
            .env("RUNNER_TEMP", &runner_temp)
            .env("COSIGN_V2", &legacy_cosign)
            .env("COSIGN_V3", &modern_cosign)
            .env("COSIGN_TRACE", &trace)
            .env("GITHUB_REPOSITORY", "schiste/Aethyme")
            .env("REF_NAME", "v0.8.29")
            .output()
            .unwrap()
    };
    let sign = run_script(&sign_script);
    assert!(
        sign.status.success(),
        "legacy signing step failed: {}",
        String::from_utf8_lossy(&sign.stderr)
    );
    let convert = run_script(&convert_script);
    assert!(
        convert.status.success(),
        "standard bundle conversion/verification step failed: {}",
        String::from_utf8_lossy(&convert.stderr)
    );

    let signature_dir = runner_temp.join("aethyme-release-signatures");
    assert!(signature_dir.join("release-manifest.legacy.json").exists());
    assert!(
        signature_dir
            .join("aethyme-v0.8.29-x86_64-pc-windows-msvc.legacy.json")
            .exists()
    );
    assert!(dist.join("release-manifest.sigstore.json").exists());
    assert!(
        fixture_root
            .join(format!("{zip_relative}.sigstore.json"))
            .exists()
    );
    assert!(!dist.join("release-manifest.legacy.json").exists());

    let expected_trace = [
        "v2:sign-blob",
        "v2:sign-blob",
        "v3:bundle create",
        "v3:verify-blob --bundle",
        "v3:bundle create",
        "v3:verify-blob --bundle",
    ];
    let trace = std::fs::read_to_string(trace).unwrap();
    assert_eq!(trace.lines().collect::<Vec<_>>(), expected_trace);
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

#[test]
fn windows_release_builds_and_smokes_the_native_zip() {
    let root = aethyme_testkit::paths::repo_root();
    let release = std::fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let ci = std::fs::read_to_string(root.join(".github/workflows/windows-release.yml")).unwrap();
    let smoke = std::fs::read_to_string(root.join("scripts/windows-release-smoke.ps1")).unwrap();

    for fragment in [
        "build-windows:",
        "runs-on: windows-latest",
        "x86_64-pc-windows-msvc",
        "Compress-Archive",
        "windows-release-smoke.ps1",
        "needs: [build, build-windows]",
        "windows_count",
        "test \"$windows_count\" = \"1\"",
        "Sign release assets with retry-safe legacy Cosign",
        "Create standard bundles and verify release assets",
    ] {
        assert!(
            release.contains(fragment),
            "release workflow is missing {fragment}"
        );
    }
    for fragment in [
        "runs-on: windows-latest",
        "cargo build --release --locked --target x86_64-pc-windows-msvc",
        "aethyme-graph-indexer --bin aethyme-graph-index",
        "windows-release-smoke.ps1",
    ] {
        assert!(
            ci.contains(fragment),
            "Windows CI workflow is missing {fragment}"
        );
    }
    for fragment in [
        "--repo-root",
        "--repo-name aethyme-windows-smoke",
        "index --repo",
        "explore --repo",
        "graph overview --repo",
        "deploy --generated-only --repo",
        "broker quick-test",
        "the broker is not yet supported on Windows",
    ] {
        assert!(
            smoke.contains(fragment),
            "Windows smoke is missing {fragment}"
        );
    }
}
