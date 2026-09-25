//! `aethyme explore --format brief` — the one-call Explore surface the
//! generated guidance prescribes (recovery plan P4.6), driven through the
//! built router.
//!
//! What this holds: the brief is text that opens by saying it is a navigation
//! aid to verify, it carries verified source spans, `--repo` defaults to the
//! current directory, and `answer-json` stays the default so every consumer
//! that parses Explore output is unaffected.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("session_hook.ts"),
        "export function handleSessionStart(event: string) {\n  return renderBrief(event);\n}\n",
    )
    .unwrap();
    std::fs::write(src.join("unrelated.ts"), "export const answer = 42;\n").unwrap();
    tmp
}

fn explore(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .arg("explore")
        .args(args)
        .current_dir(cwd)
        .env_remove("AETHYME_REPO")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme explore")
}

#[test]
fn brief_is_one_call_with_a_verification_warning_and_spans() {
    let tmp = fixture();
    let output = explore(
        tmp.path(),
        &[
            "--request",
            "where is handleSessionStart",
            "--format",
            "brief",
        ],
    );
    assert!(
        output.status.success(),
        "explore --format brief failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next().unwrap_or_default();
    assert!(first.contains("navigation aid, not an answer"), "{text}");
    assert!(first.contains("verify the spans"), "{text}");
    assert!(first.contains("safe_to_use_as_answer="), "{text}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_err(),
        "the brief is text, not JSON: {text}"
    );
    assert!(text.contains("── 1. src/session_hook.ts:"), "{text}");
    assert!(text.contains("handleSessionStart"), "{text}");
}

#[test]
fn answer_json_remains_the_default_format() {
    let tmp = fixture();
    let repo = tmp.path().to_string_lossy().to_string();
    let output = explore(
        tmp.path(),
        &["--repo", &repo, "--request", "where is handleSessionStart"],
    );
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("default output is answer-json");
    assert_eq!(value["schema_version"], "aethyme-explore-v1");
}

#[test]
fn an_unknown_format_is_a_usage_error() {
    let tmp = fixture();
    let output = explore(tmp.path(), &["--request", "x", "--format", "yaml"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("answer-json or brief"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
