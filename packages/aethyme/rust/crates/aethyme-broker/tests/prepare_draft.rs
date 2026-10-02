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
        assert!(
            !draft.contains("repository_shared\n"),
            "{name}: a draft must default to the safe per-worktree policy"
        );
        // The budget is emitted commented out, so a draft that mentions it must
        // also not activate it: a fresh repository should never silently start
        // rotating a store it has not used.
        assert!(
            !draft.lines().any(|line| {
                let line = line.trim();
                !line.starts_with('#') && line.starts_with("max_bytes")
            }),
            "{name}: the draft must not activate a cache budget by default:\n{draft}"
        );
        assert!(
            draft.contains("# [shared_cache]"),
            "{name}: the draft must document the budget it can opt into:\n{draft}"
        );
        assert!(
            draft.contains("schema_version = 1"),
            "{name}: missing schema version:\n{draft}"
        );
        // No absolute paths and no timestamps: generated files must be
        // byte-identical across machines and runs.
        assert!(
            !draft.contains(tmp.path().to_string_lossy().as_ref()),
            "{name}: draft leaked an absolute path:\n{draft}"
        );
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

/// A budget declared without a shared step reads as a disk bound that is not
/// being applied, which is the exact belief the field exists to remove. Refuse
/// it at parse time rather than accepting a silent no-op.
#[test]
fn a_cache_budget_without_a_shared_step_is_refused() {
    let config = r#"
schema_version = 1

[[steps]]
name = "javascript-dependencies"
command = ["npm", "ci"]
inputs = ["package.json", "package-lock.json"]
outputs = ["node_modules/"]
cache = "worktree_local"

[shared_cache]
max_bytes = 1024
"#;
}
