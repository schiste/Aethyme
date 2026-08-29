//! Contract checks for `aethyme explore-summary --from <answer-json>`.
//!
//! Ported from `tests/local/test_explore_summary_cli.py`
//! (python-retirement Phase 7). The command replaced the
//! `.venv/bin/python` projection heredoc that deployed skill templates
//! used to carry (Phase 5.5); these assertions encode the byte contract
//! that heredoc had, so a future reimplementation cannot silently drift.
//!
//! **How the oracle survived the port.** The Python suite rebuilt the
//! expected projection in-process with `json.dumps(..., indent=2)` and
//! compared bytes. A Rust reimplementation of that oracle would be a
//! second guess at the same answer, free to drift toward the
//! implementation it is checking. Instead the CPython output was
//! captured once and frozen under `tests/fixtures/explore_summary/`
//! — same bytes, same guarantee, and now immune to drift. See that
//! directory's README.

use std::path::{Path, PathBuf};

use aethyme_testkit::repos::{read, write};
use aethyme_testkit::{Invoke, invoke_aethyme, tmp_dir};
use serde_json::Value;

const SUBSYSTEM_KEYS: [&str; 7] = [
    "rank",
    "id",
    "label",
    "role",
    "confidence",
    "token_subsystems",
    "missing_coverage_warnings",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/explore_summary")
        .join(name)
}

fn case(name: &str) -> (PathBuf, String) {
    (
        fixture(&format!("{name}.input.json")),
        read(fixture(&format!("{name}.expected.json"))),
    )
}

fn run(path: &Path) -> String {
    invoke_aethyme(["explore-summary", "--from", &path.display().to_string()])
        .ok()
        .to_string()
}

/// Key names in EMITTED order for every object whose keys sit at exactly
/// `indent` spaces, one inner `Vec` per object.
///
/// Key order is read from the raw text rather than from a parsed
/// `serde_json::Value`, because `serde_json::Map` is a `BTreeMap` and
/// sorts keys — asking it about order would silently assert the
/// alphabet instead of the contract. (Turning on its `preserve_order`
/// feature is not an option: Cargo unifies features, so a dev-dependency
/// flag would change map ordering in the product build too.) The format
/// assumption — `indent=2` pretty JSON — is itself pinned by the byte
/// goldens compared in the tests below.
fn key_groups(text: &str, indent: usize) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let leading = line.len() - trimmed.len();
        if leading + 2 == indent && trimmed.starts_with('{') {
            groups.push(Vec::new());
            open = true;
        } else if leading + 2 == indent && trimmed.starts_with('}') {
            open = false;
        } else if open
            && leading == indent
            && let Some(rest) = trimmed.strip_prefix('"')
            && let Some(key) = rest.split('"').next()
        {
            groups
                .last_mut()
                .expect("object opened before its keys")
                .push(key.to_string());
        }
    }
    groups
}

/// The body of a top-level array-valued key, exclusive of its brackets.
fn top_level_block<'a>(text: &'a str, key: &str) -> &'a str {
    let marker = format!("\n  \"{key}\": [\n");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("no {key} block in:\n{text}"))
        + marker.len();
    let rest = &text[start..];
    &rest[..rest.find("\n  ]").expect("unterminated block")]
}

#[test]
fn projection_is_byte_identical_to_the_cpython_oracle() {
    let (input, expected) = case("full");
    assert_eq!(run(&input), expected);
}

#[test]
fn missing_fields_render_as_null_not_omitted() {
    let (input, expected) = case("empty");
    let output = run(&input);
    assert_eq!(output, expected);

    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        key_groups(&output, 2),
        [[
            "safe_to_use_as_answer",
            "trust_policy",
            "subsystems",
            "top_verification_targets",
            "verification_steps",
            "observability",
        ]]
    );
    assert!(payload["safe_to_use_as_answer"].is_null());
    assert!(payload["trust_policy"].is_null());
    assert_eq!(
        payload["observability"],
        serde_json::json!({"readiness": null})
    );
    // Contract decision (Phase 5.5): no schema_version key, unlike
    // verify-targets — byte-parity with the retired heredoc keeps the
    // skill's "inspect only these fields" list exactly true.
    assert!(payload.get("schema_version").is_none());
}

#[test]
fn truncation_and_subsystem_tagging() {
    let (input, _) = case("full");
    let output = run(&input);
    let payload: Value = serde_json::from_str(&output).unwrap();

    let lanes = payload["subsystems"].as_array().unwrap();
    assert_eq!(lanes.len(), 3);
    let lane_keys = key_groups(top_level_block(&output, "subsystems"), 6);
    assert_eq!(lane_keys.len(), 3);
    for keys in &lane_keys {
        assert_eq!(keys.as_slice(), SUBSYSTEM_KEYS);
    }
    assert!(lanes[2]["label"].is_null());

    let targets = payload["top_verification_targets"].as_array().unwrap();
    let paths: Vec<&str> = targets
        .iter()
        .map(|target| target["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "gcp-run-proxy/src/worker.mjs",
            "gcp-run-proxy/src/auth.mjs",
            "backend/api_keys/middleware.py",
            "store/redb.rs",
        ]
    );
    // role wins when truthy; an existing subsystem key is left alone; an
    // empty-string role falls through to the lane id.
    assert_eq!(targets[0]["subsystem"], "edge credential transport");
    assert_eq!(targets[2]["subsystem"], "pinned");
    assert_eq!(targets[3]["subsystem"], "storage");
    // `subsystem` is appended last when the projection adds it.
    let target_keys = key_groups(top_level_block(&output, "top_verification_targets"), 6);
    assert_eq!(
        target_keys[0],
        ["kind", "path", "subsystem"],
        "projection must append `subsystem` after the target's own keys"
    );

    assert_eq!(
        payload["verification_steps"],
        serde_json::json!(["step one", "step two", "step three"])
    );
}

#[test]
fn overall_target_cap_is_six() {
    let (input, expected) = case("cap");
    let output = run(&input);
    assert_eq!(output, expected);
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        payload["top_verification_targets"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
}

#[test]
fn non_ascii_is_escaped_like_cpython_ensure_ascii() {
    let (input, expected) = case("non_ascii");
    let output = run(&input);
    assert_eq!(output, expected);
    assert!(output.is_ascii(), "{output}");
    assert!(output.contains("\\u00e9rifier"));
    assert!(output.contains("\\ud83d\\ude80")); // astral char as a surrogate pair
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["subsystems"][0]["label"], "日本語");
}

#[test]
fn reads_from_stdin_with_dash() {
    let (input, expected) = case("full");
    // `--from -` is the same reader idiom verify-targets exposes.
    let result = Invoke::new(["explore-summary", "--from", "-"])
        .stdin(read(&input))
        .run();
    result.ok();
    assert_eq!(result.output, expected);
}

#[test]
fn missing_from_option_is_a_usage_error() {
    let result = invoke_aethyme(["explore-summary"]);
    result.expect_code(2);
    result.assert_contains("--from");
}

#[test]
fn unparseable_json_is_a_usage_error() {
    let tmp = tmp_dir();
    let path = tmp.path().join("broken.json");
    write(&path, "not json at all");
    let result = invoke_aethyme(["explore-summary", "--from", &path.display().to_string()]);
    result.expect_code(2);
    result.assert_contains("parse --from JSON");
}

#[test]
fn non_object_document_is_a_usage_error() {
    let tmp = tmp_dir();
    let path = tmp.path().join("array.json");
    write(&path, "[]");
    let result = invoke_aethyme(["explore-summary", "--from", &path.display().to_string()]);
    result.expect_code(2);
    result.assert_contains("must be an object");
}

#[test]
fn missing_file_fails() {
    let tmp = tmp_dir();
    let result = invoke_aethyme([
        "explore-summary",
        "--from",
        &tmp.path().join("nope.json").display().to_string(),
    ]);
    result.expect_code(1);
    result.assert_contains("read --from");
}

#[test]
fn listed_in_router_help() {
    let result = invoke_aethyme(["--help"]);
    result.ok();
    result.assert_contains("explore-summary --from");
}
