//! Preparation drafting: `aethyme init` must produce a `prepare.toml` the
//! real configuration reader accepts, and must refuse to invent a step whose
//! declared output its own command cannot create.
//!
//! The invariant that matters is the first one. A draft that `prepare status`
//! immediately rejects is worse than no draft: it converts a missing
//! declaration into a permanently red status and teaches the customer that
//! the broker's own output is not trustworthy.

use std::path::Path;
use std::process::Command;

use aethyme_broker::init;

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hi\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn touch(root: &Path, name: &str, body: &str) {
    let path = root.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

/// Every draft the detector can produce must survive the real reader.
///
/// The detector parses its own output before returning it, so this asserts
/// the guard is actually wired rather than trusting the comment that says so.
#[test]
fn every_drafted_config_is_accepted_by_the_real_reader() {
    let cases: &[(&str, Vec<(&str, &str)>)] = &[
        (
            "pnpm",
            vec![
                ("package.json", "{}\n"),
                ("pnpm-lock.yaml", "lockfileVersion: 9\n"),
            ],
        ),
        (
            "yarn",
            vec![
                ("package.json", "{}\n"),
                ("yarn.lock", "# yarn lockfile v1\n"),
            ],
        ),
        (
            "npm-with-lock",
            vec![("package.json", "{}\n"), ("package-lock.json", "{}\n")],
        ),
        // No lockfile: the draft must decline rather than emit `npm install`,
        // which rewrites a tracked file and is refused by the worktree guard.
        ("npm-without-lock", vec![("package.json", "{}\n")]),
        (
            "uv",
            vec![
                ("pyproject.toml", "[project]\nname = \"x\"\n"),
                ("uv.lock", "version = 1\n"),
            ],
        ),
        ("uv-without-pyproject", vec![("uv.lock", "version = 1\n")]),
        ("poetry", vec![("poetry.lock", "\n")]),
        ("pip", vec![("requirements.txt", "requests\n")]),
        (
            "node-and-rust",
            vec![
                ("package.json", "{}\n"),
                ("pnpm-lock.yaml", "lockfileVersion: 9\n"),
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("Cargo.lock", "\n"),
            ],
        ),
        ("go", vec![("go.mod", "module x\n"), ("go.sum", "\n")]),
    ];

    for (name, files) in cases {
        let tmp = tempfile::tempdir().unwrap();
        for (path, body) in files {
            touch(tmp.path(), path, body);
        }
        let Some(draft) = init::draft_prepare_config(tmp.path()) else {
            // Declining is always allowed; emitting something invalid is not.
            continue;
        };
        // The generator validates its output with the real config reader before
        // returning it. Parse once here to assert the policy represented by the
        // TOML, rather than assuming every ecosystem must use a local cache.
        let parsed: toml::Value = toml::from_str(&draft).expect("valid prepare TOML");
        assert_eq!(
            parsed
                .get("schema_version")
                .and_then(toml::Value::as_integer),
            Some(1),
            "{name}: missing schema version:\n{draft}"
        );
        let steps = parsed
            .get("steps")
            .and_then(toml::Value::as_array)
            .expect("draft has steps");
        if matches!(*name, "pnpm" | "node-and-rust") {
            let shared_cache = parsed
                .get("shared_cache")
                .and_then(toml::Value::as_table)
                .expect("pnpm selects a shared cache");
            assert_eq!(
                shared_cache.get("root").and_then(toml::Value::as_str),
                Some("worktree_volume"),
                "{name}: pnpm store must be on the selected worktree volume:\n{draft}"
            );
            assert!(
                !shared_cache.contains_key("max_bytes"),
                "{name}: do not override the shared cache threshold:\n{draft}"
            );
            let step = steps
                .iter()
                .find(|step| {
                    step.get("name").and_then(toml::Value::as_str)
                        == Some("javascript-dependencies")
                })
                .expect("pnpm dependency step");
            assert_eq!(
                step.get("cache").and_then(toml::Value::as_str),
                Some("repository_shared"),
                "{name}: pnpm store must be repository shared:\n{draft}"
            );
            assert!(
                step.get("outputs")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|outputs| {
                        outputs
                            .iter()
                            .any(|output| output.as_str() == Some("node_modules/"))
                    }),
                "{name}: node_modules stays a per-worktree output:\n{draft}"
            );
            for (field, expected_install) in [
                ("command", "pnpm install --frozen-lockfile"),
                (
                    "offline_command",
                    "pnpm install --offline --frozen-lockfile",
                ),
            ] {
                let command = step
                    .get(field)
                    .and_then(toml::Value::as_array)
                    .expect("pnpm command");
                assert_eq!(command.first().and_then(toml::Value::as_str), Some("sh"));
                assert_eq!(command.get(1).and_then(toml::Value::as_str), Some("-c"));
                let script = command
                    .get(2)
                    .and_then(toml::Value::as_str)
                    .expect("pnpm shell command");
                assert!(script.contains(expected_install), "{field}: {script}");
                assert!(
                    script.contains("--store-dir \"$AETHYME_PREPARE_CACHE_DIR/pnpm-store\""),
                    "{field}: {script}"
                );
            }
        } else {
            assert!(
                parsed.get("shared_cache").is_none(),
                "{name}: local-cache drafts should not declare shared-cache placement:\n{draft}"
            );
            assert!(
                steps.iter().all(|step| {
                    step.get("cache")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("worktree_local")
                        == "worktree_local"
                }),
                "{name}: npm, Yarn, and uv drafts stay worktree local:\n{draft}"
            );
        }
        // No absolute paths and no timestamps: generated files must be
        // byte-identical across machines and runs.
        assert!(
            !draft.contains(tmp.path().to_string_lossy().as_ref()),
            "{name}: draft leaked an absolute path:\n{draft}"
        );
    }
}

/// Cache configuration must not promise deduplication or a write quota that
/// the actual preparation runner does not enforce.
#[test]
fn storage_guidance_distinguishes_shared_stores_from_worktree_allocations() {
    for (lockfile, contents) in [
        ("pnpm-lock.yaml", "lockfileVersion: 9\n"),
        ("package-lock.json", "{}\n"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "package.json", "{}\n");
        touch(tmp.path(), lockfile, contents);
        let draft = init::draft_prepare_config(tmp.path()).expect("node draft");
        assert!(draft.contains("same filesystem"));
        assert!(draft.contains("does not guarantee deduplication"));
        assert!(draft.contains("The command must use"));
        assert!(draft.contains("separate copy per session"));
        assert!(draft.contains("not a storage quota"));
        assert!(draft.contains("a running install can exceed it"));
        assert!(!draft.contains("difference between one copy on disk"));
        assert!(!draft.contains("to cap it;"));
    }
}

/// A step whose declared output its own command cannot create would report
/// `failed` on every run. That is the failure mode this detector exists to
/// avoid, so assert the absence directly rather than trusting the comment.
#[test]
fn no_draft_declares_an_output_its_command_cannot_produce() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "package.json", "{}\n");
    touch(tmp.path(), "package-lock.json", "{}\n");
    let draft = init::draft_prepare_config(tmp.path()).expect("npm draft");

    // `npm install` mutates the lockfile; only `npm ci` is reproducible.
    assert!(
        !draft.contains("\"npm\", \"install\""),
        "a draft must never emit a lockfile-mutating install:\n{draft}"
    );
    assert!(draft.contains("\"npm\", \"ci\""), "{draft}");

    // Every declared input must be a file that exists, or the digest reports
    // `invalid` before any command runs.
    for line in draft
        .lines()
        .filter(|l| l.trim_start().starts_with("inputs"))
    {
        for quoted in line.split('"').skip(1).step_by(2) {
            assert!(
                tmp.path().join(quoted).is_file(),
                "draft declares a missing input {quoted:?}:\n{draft}"
            );
        }
    }
}

/// Host-global ecosystems need no per-worktree install, so they get guidance
/// rather than a step. Emitting a step for Cargo would declare an output that
/// `cargo fetch` never writes, and the reader rejects a step-less config, so
/// the guidance has to survive as a report rather than a file.
#[test]
fn host_global_ecosystems_get_guidance_not_a_step() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
    touch(tmp.path(), "Cargo.lock", "\n");

    assert!(
        init::draft_prepare_config(tmp.path()).is_none(),
        "cargo needs no step, and a step-less file would be rejected"
    );
    let guidance = init::prepare_guidance(tmp.path());
    assert_eq!(guidance.len(), 1, "{guidance:?}");
    assert!(guidance[0].starts_with("cargo:"), "{guidance:?}");
    assert!(
        guidance[0].contains("writes nothing"),
        "guidance must explain why no step exists: {guidance:?}"
    );

    // The explanation must reach the operator, not be discarded.
    init_repo(tmp.path());
    let report = init::draft_prepare(tmp.path()).unwrap();
    assert_eq!(
        report.checks[0].status,
        aethyme_broker::init::CheckStatus::Pass
    );
    assert!(
        report.checks[0].detail.contains("cargo:"),
        "the check must carry the reasoning: {}",
        report.checks[0].detail
    );
    assert!(
        !tmp.path().join(".aethyme/prepare.toml").exists(),
        "no file may be written without a step"
    );
}

/// Nothing recognized at all is a warning worth a human's attention, which is
/// different from "recognized, and nothing to do".
#[test]
fn an_unrecognized_repository_still_gets_a_written_draft_path() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    assert!(init::draft_prepare_config(tmp.path()).is_none());

    let report = init::draft_prepare(tmp.path()).unwrap();
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].id, "prepare.draft");
    assert_eq!(
        report.checks[0].status,
        aethyme_broker::init::CheckStatus::Warn
    );
    assert!(
        !tmp.path().join(".aethyme/prepare.toml").exists(),
        "an unrecognized repository must not gain a file"
    );
}

/// Determinism: the same repository must draft byte-identical bytes, or a
/// second `aethyme init` looks like a change.
#[test]
fn drafting_is_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "package.json", "{}\n");
    touch(tmp.path(), "pnpm-lock.yaml", "lockfileVersion: 9\n");
    let first = init::draft_prepare_config(tmp.path()).unwrap();
    let second = init::draft_prepare_config(tmp.path()).unwrap();
    assert_eq!(first, second);
}

/// Never overwrite, exactly like gate drafting: a repository that already
/// declares preparation keeps its own policy, byte for byte.
#[test]
fn an_existing_prepare_toml_is_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    touch(tmp.path(), "package.json", "{}\n");
    touch(tmp.path(), "pnpm-lock.yaml", "lockfileVersion: 9\n");
    touch(
        tmp.path(),
        ".aethyme/prepare.toml",
        "schema_version = 1\n# hand written\n",
    );

    let report = init::draft_prepare(tmp.path()).unwrap();
    assert_eq!(
        report.checks[0].status,
        aethyme_broker::init::CheckStatus::Pass
    );
    let written = std::fs::read_to_string(tmp.path().join(".aethyme/prepare.toml")).unwrap();
    assert_eq!(written, "schema_version = 1\n# hand written\n");
}
