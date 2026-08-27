//! Implementation-blind CLI tests for `aethyme enhance` and the
//! generated onboarding/telemetry artifacts.
//!
//! Ported verbatim from `tests/local/test_enhance.py` (python-retirement
//! Phase 7). Every assertion still drives the built router as a
//! subprocess and reads the files it wrote — the Python `deploy()`
//! function has been gone since the Phase 2 flip, so the CLI surface is
//! the contract.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use aethyme_testkit::repos::{build_pnpm_demo_repo, read, write};
use aethyme_testkit::{aethyme_bin, invoke_aethyme, package_root, tmp_dir};
use serde_json::Value;

/// Frozen contract path (was `src.enhance.AGENTS_OVERRIDE_PATH`; the
/// Python module is deleted — the aethyme-enhance crate owns the
/// constant now).
const AGENTS_OVERRIDE_PATH: &str = ".aethyme/overrides/agents.json";

/// Run `aethyme enhance deploy` and return stdout+stderr.
///
/// Output lines are `  {action:9}  {relative_path}`, golden-verified
/// byte-stable by the retired enhance-golden.sh.
fn deploy(repo: &Path, force: bool) -> String {
    let mut args = vec![
        "enhance".to_string(),
        "deploy".to_string(),
        "--repo".to_string(),
        repo.display().to_string(),
    ];
    if force {
        args.push("--force".to_string());
    }
    invoke_aethyme(args).ok().to_string()
}

/// Relative paths from deploy action lines (the old DeployAction list).
fn deployed_paths(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("Enhanced:"))
        .map(|line| line.chars().skip(13).collect::<String>())
        .collect()
}

fn telemetry_events(repo: &Path) -> Vec<Value> {
    read(repo.join(".aethyme/generated/experience-telemetry.jsonl"))
        .lines()
        .map(|line| serde_json::from_str(line).expect("telemetry line is JSON"))
        .collect()
}

fn event_types(repo: &Path) -> Vec<String> {
    telemetry_events(repo)
        .iter()
        .map(|event| event["event_type"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn demo_repo(tmp: &Path) -> std::path::PathBuf {
    build_pnpm_demo_repo(&tmp.join("demo-repo"))
}

#[test]
fn agents_document_includes_broker_protocol_only_when_configured() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());

    // No broker config -> no broker section, no failing instructions.
    deploy(&repo, false);
    assert!(!read(repo.join("AGENTS.md")).contains("Broker Coordination"));

    // Broker-configured repo -> the protocol appears, with the
    // essentials: status-before-editing, verified submit as the default,
    // the action-required file, and authority-based Git operations.
    write(
        repo.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"ok\"\ncommand = \"true\"\n",
    );
    deploy(&repo, true);
    let agents = read(repo.join("AGENTS.md"));
    for needle in [
        "## Broker Coordination",
        "broker status --json",
        "broker start --task",
        "broker start --task \"<your task>\" --path <planned-path>",
        "Repeat `--path` for every file",
        "explicit leases atomically",
        "atomic `start/adopt --path` declaration",
        "broker adopt",
        "broker leases claim",
        "broker exec --session",
        "broker submit",
        "broker ship plan --entry <promoted-entry-id>",
        "broker ship execute --entry <promoted-entry-id> --confirm <full-publication-sha>",
        "Prefer this reviewed broker ship workflow over a raw push",
        "Without publication authority, stop after submit",
        "AETHYME_TEST_DB_SUFFIX",
        ".aethyme/broker-action-required.md",
        "aethyme/integration",
        "Git operations remain available to agents",
        "clone, fetch, pull, switch, branch, add, commit",
        "stash, merge, cherry-pick, rebase, revert, reset, tag, push",
        "force-push when explicitly authorized",
        "deletion of an exact ref",
        "Operations that require coordination **must go through the broker**",
        "Direct Git is limited to read-only inspection",
        "must not run outside the broker",
        "broker git --session <your-session-id>",
        "broker gh --session <your-session-id>",
        "Every GitHub repository or account mutation",
        "broker operations reconcile",
        "--effect read|write|destructive --scope <resource>",
        "Do not infer permission to publish",
    ] {
        assert!(agents.contains(needle), "AGENTS.md missing {needle:?}");
    }
    assert!(!agents.contains("**Never** push"));
    assert!(!agents.contains("Direct Git commands are also permitted when appropriate"));
    // CLAUDE.md renders from the same generated document.
    assert!(read(repo.join("CLAUDE.md")).contains("## Broker Coordination"));
}

#[test]
fn enhance_deploy_writes_generated_onboarding() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());

    let written = deployed_paths(&deploy(&repo, false));
    for relative in [
        ".codex/skills/aethyme/SKILL.md",
        ".codex/skills/aethyme/aethyme-explore",
        "AGENTS.md",
        ".codex/skills/repo-onboarding/SKILL.md",
        ".claude/skills/repo-onboarding/SKILL.md",
        ".codex/skills/repo-act/SKILL.md",
        ".claude/skills/repo-act/SKILL.md",
        ".aethyme/generated/onboarding.json",
        ".aethyme/generated/act-starter.json",
    ] {
        assert!(
            written.contains(relative),
            "deploy did not write {relative:?}"
        );
    }

    let artifact: Value =
        serde_json::from_str(&read(repo.join(".aethyme/generated/onboarding.json"))).unwrap();
    let act: Value =
        serde_json::from_str(&read(repo.join(".aethyme/generated/act-starter.json"))).unwrap();
    assert!(
        artifact["telemetry"]["counts"]["commands"]
            .as_i64()
            .unwrap()
            >= 1
    );
    assert!(
        !artifact["summon"]["task_signals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !act["starter_checklists"]["debugging"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        event_types(&repo)
            .iter()
            .any(|kind| kind == "enhance.deploy")
    );

    invoke_aethyme(["enhance", "verify", "--repo", &repo.display().to_string()])
        .assert_contains("All discoverability files present and substituted.")
        .ok();

    let wrapper = read(repo.join(".claude/hooks/aethyme-load-context.sh"));
    assert!(wrapper.contains("repo record-wrapper-invocation"));
    assert!(wrapper.contains("--wrapper aethyme-sessionstart-hook"));

    let codex_wrapper = repo.join(".codex/skills/aethyme/aethyme-explore");
    let codex_text = read(&codex_wrapper);
    assert!(codex_text.contains("repo record-wrapper-invocation"));
    assert!(codex_text.contains("--wrapper aethyme-explore"));
    assert!(
        is_executable(&codex_wrapper),
        "deployed wrapper must be executable"
    );

    let agents = read(repo.join("AGENTS.md"));
    for needle in [
        "This file is generated by Aethyme. Do not edit it directly.",
        // Phase 6 (2026-08-01): the notice covers the whole Python CLI
        // now, not just `explore` — the package is deleted, so every
        // spelling fails. The "Do not run" marker must stay:
        // verify-playground and the contract checker exempt lines
        // carrying it from their greps.
        "Do not run `python -m src.cli ...` for anything",
        "## Aethyme Repo Routing",
        "Primary fast test: `pnpm test`",
        "Primary app entrypoint: `src/main.ts`",
        ".aethyme/generated/experience-status.md",
        "## Commit Hygiene",
        "may use a subject-only message",
        "Section content may start on the header line (`Problem: text`)",
        "repo lint-commit-message .git/COMMIT_EDITMSG",
    ] {
        assert!(agents.contains(needle), "AGENTS.md missing {needle:?}");
    }
}

#[test]
fn installed_binary_deploys_without_a_source_checkout() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let isolated_config = tmp.path().join("empty-config");
    let output = std::process::Command::new(aethyme_bin())
        .args(["enhance", "deploy", "--repo"])
        .arg(&repo)
        .env_remove("AETHYME_ROOT")
        .env("XDG_CONFIG_HOME", isolated_config)
        .current_dir(&repo)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for relative in [
        "AGENTS.md",
        ".codex/skills/aethyme/SKILL.md",
        ".claude/hooks/aethyme-load-context.sh",
    ] {
        let content = read(repo.join(relative));
        assert!(!content.contains("AETHYME_ROOT"), "{relative}");
        assert!(
            !content.contains("/rust/target/release/aethyme"),
            "{relative}"
        );
    }
    assert!(read(repo.join("AGENTS.md")).contains("aethyme explore"));
}

#[test]
fn enhance_deploy_migrates_human_agents_content_into_override() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    write(
        repo.join("AGENTS.md"),
        "# Maintainer Rules\n\nRun focused tests before broad suites.\n",
    );

    deploy(&repo, false);
    let first = read(repo.join("AGENTS.md"));
    let override_payload: Value =
        serde_json::from_str(&read(repo.join(AGENTS_OVERRIDE_PATH))).unwrap();
    assert_eq!(
        override_payload["maintainer_markdown"],
        "# Maintainer Rules\n\nRun focused tests before broad suites."
    );
    assert!(first.contains("## Maintainer Notes"));
    assert!(first.contains("# Maintainer Rules"));
    assert!(first.contains("Run focused tests before broad suites."));

    deploy(&repo, true);
    let second = read(repo.join("AGENTS.md"));
    assert!(!second.contains("<!-- AETHYME:BEGIN generated -->"));
    assert!(second.contains("# Maintainer Rules"));
    assert!(second.contains("## Aethyme Repo Routing"));
}

#[test]
fn enhance_deploy_migrates_old_full_file_agents_template() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let root = package_root();
    let rendered = read(root.join("skills/aethyme/AGENTS.md"))
        .replace("{{AETHYME_ROOT}}", &root.display().to_string());
    write(repo.join("AGENTS.md"), &rendered);

    deploy(&repo, false);
    let agents = read(repo.join("AGENTS.md"));
    assert!(agents.starts_with("# Agent Instructions"));
    assert_eq!(agents.matches("# Agent Instructions").count(), 1);
    assert!(!agents.contains("<!-- AETHYME:BEGIN generated -->"));
}

#[test]
fn init_and_validate_onboarding_overrides_cli() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();

    invoke_aethyme(["repo", "init-onboarding-overrides", &repo_arg]).ok();
    assert!(repo.join(".aethyme/overrides/onboarding.json").exists());

    invoke_aethyme(["repo", "validate-onboarding-overrides", &repo_arg])
        .assert_contains("Valid override file")
        .ok();

    let types = event_types(&repo);
    assert!(
        types
            .iter()
            .any(|kind| kind == "repo.init-onboarding-overrides")
    );
    assert!(
        types
            .iter()
            .any(|kind| kind == "repo.validate-onboarding-overrides")
    );
}

#[test]
fn init_and_validate_agents_overrides_cli() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();

    invoke_aethyme(["repo", "init-agents-overrides", &repo_arg]).ok();
    assert!(repo.join(AGENTS_OVERRIDE_PATH).exists());

    invoke_aethyme(["repo", "validate-agents-overrides", &repo_arg])
        .assert_contains("Valid override file")
        .ok();

    let types = event_types(&repo);
    assert!(
        types
            .iter()
            .any(|kind| kind == "repo.init-agents-overrides")
    );
    assert!(
        types
            .iter()
            .any(|kind| kind == "repo.validate-agents-overrides")
    );
}

#[test]
fn enhance_verify_prints_summary() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    deploy(&repo, false);

    let result = invoke_aethyme(["enhance", "verify", "--repo", &repo.display().to_string()]);
    result.ok();
    result.assert_contains("Enhancement summary:");
    result.assert_contains("Recommendation: load `repo-onboarding` then run `explore`");
    result.assert_contains("Experience telemetry:");
    assert!(
        event_types(&repo)
            .iter()
            .any(|kind| kind == "enhance.verify")
    );
}

#[test]
fn static_wrapper_template_records_invocation_signal() {
    let wrapper = read(package_root().join("skills/aethyme/aethyme-explore"));
    assert!(wrapper.contains("repo record-wrapper-invocation"));
    assert!(wrapper.contains("--wrapper aethyme-explore"));
}

/// Phase 3 exit criterion: the deployed hook fires end-to-end natively.
///
/// Runs the deployed `.claude/hooks/aethyme-load-context.sh` exactly as
/// Claude Code would (CLAUDE_PROJECT_DIR set, `aethyme` on PATH) and
/// asserts the wrapper invocation landed in the repo-local ledger.
#[test]
fn deployed_session_hook_records_native_wrapper_invocation() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    deploy(&repo, false);

    let hook = repo.join(".claude/hooks/aethyme-load-context.sh");
    assert!(read(&hook).contains("command -v aethyme"));

    let bin_dir = aethyme_bin().parent().unwrap().to_path_buf();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("bash")
        .arg(&hook)
        .env("CLAUDE_PROJECT_DIR", &repo)
        .env("PATH", path)
        .output()
        .expect("run deployed hook");
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let envelope: Value = serde_json::from_slice(&output.stdout).expect("hook stdout is JSON");
    assert_eq!(
        envelope["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );

    assert!(
        telemetry_events(&repo).iter().any(|event| {
            event["event_type"] == "wrapper.invocation"
                && event["payload"]["wrapper_name"] == "aethyme-sessionstart-hook"
        }),
        "hook did not ledger a wrapper.invocation row"
    );
}

#[test]
fn repo_experience_telemetry_reports_json_and_text() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();
    deploy(&repo, false);

    invoke_aethyme([
        "repo",
        "record-wrapper-invocation",
        &repo_arg,
        "--wrapper",
        "aethyme-explore",
        "--detail",
        "source=test",
    ]);

    let json_result = invoke_aethyme(["repo", "experience-telemetry", &repo_arg, "--json-output"]);
    json_result.ok();
    let payload = json_result.json();
    assert!(payload["event_count"].as_i64().unwrap() >= 2);
    assert!(
        payload["wrapper_invocations"]["aethyme-explore"]
            .as_i64()
            .unwrap()
            >= 1
    );
    assert!(payload["by_type"].get("enhance.deploy").is_some());
    assert!(payload["kpis"]["wrapper_total"].as_i64().unwrap() >= 1);
    assert_eq!(payload["kpis"]["act_has_fast_test"], true);

    let text_result = invoke_aethyme(["repo", "experience-telemetry", &repo_arg]);
    text_result.ok();
    text_result.assert_contains("Wrapper invocations:");
    text_result.assert_contains("KPIs:");
    text_result.assert_contains("aethyme-explore");
}

#[test]
fn repo_experience_status_writes_artifacts() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    deploy(&repo, false);

    let result = invoke_aethyme([
        "repo",
        "experience-status",
        &repo.display().to_string(),
        "--json-output",
    ]);
    result.ok();
    let payload = result.json();
    assert_eq!(payload["schema_version"], "aethyme-experience-status-v1");
    assert!(payload.get("recommended_next_action").is_some());
    assert!(
        repo.join(".aethyme/generated/experience-status.json")
            .exists()
    );

    let markdown = read(repo.join(".aethyme/generated/experience-status.md"));
    assert!(markdown.contains("# Aethyme Experience Status"));
    assert!(markdown.contains("Recommended Next Action"));
}

#[test]
fn repo_experience_telemetry_flags_no_wrapper_usage() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();
    deploy(&repo, false);

    let result = invoke_aethyme(["repo", "experience-telemetry", &repo_arg, "--json-output"]);
    result.ok();
    let payload = result.json();
    assert!(
        codes(&payload["kpis"]["signals"]).contains(&"enhanced_but_no_wrapper_usage".to_string())
    );
    assert!(
        codes(&payload["kpis"]["suggestions"])
            .contains(&"load_onboarding_and_use_wrapper".to_string())
    );

    let text_result = invoke_aethyme(["repo", "experience-telemetry", &repo_arg]);
    text_result.ok();
    text_result.assert_contains("Suggestions:");
    text_result.assert_contains("load_onboarding_and_use_wrapper");

    invoke_aethyme(["repo", "experience-telemetry", &repo_arg, "--check"]).expect_code(1);
}

#[test]
fn repo_experience_telemetry_detects_stale_override_artifacts() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();
    deploy(&repo, false);

    let override_path = repo.join(".aethyme/overrides/onboarding.json");
    write(
        &override_path,
        &(serde_json::json!({
            "notes": ["Local maintainer hint"],
            "commands": [{
                "kind": "test",
                "command": "./scripts/test-fast.sh",
                "source": "manual-override",
                "confidence": "high",
            }],
        })
        .to_string()
            + "\n"),
    );
    // Make the override strictly newer than both generated artifacts;
    // the Python original used os.utime with the same +5s offset.
    let newest = [
        repo.join(".aethyme/generated/onboarding.json"),
        repo.join(".aethyme/generated/act-starter.json"),
    ]
    .iter()
    .map(|path| std::fs::metadata(path).unwrap().modified().unwrap())
    .max()
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&override_path)
        .unwrap()
        .set_modified(newest + Duration::from_secs(5))
        .unwrap();

    let result = invoke_aethyme(["repo", "experience-telemetry", &repo_arg, "--json-output"]);
    result.ok();
    let payload = result.json();
    assert!(
        codes(&payload["kpis"]["signals"]).contains(&"override_regeneration_required".to_string())
    );
    assert_eq!(payload["freshness"]["regeneration_required"], true);
    let stale: BTreeSet<String> = payload["freshness"]["stale_targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        stale,
        BTreeSet::from(["onboarding".to_string(), "act".to_string()])
    );
    assert!(
        codes(&payload["kpis"]["suggestions"])
            .contains(&"regenerate_onboarding_artifacts".to_string())
    );

    let text_result = invoke_aethyme(["repo", "experience-telemetry", &repo_arg]);
    text_result.ok();
    text_result.assert_contains("Override freshness:");
    text_result.assert_contains("regeneration_required: yes");

    invoke_aethyme(["repo", "experience-telemetry", &repo_arg, "--check"]).expect_code(1);
}

#[test]
fn enhance_verify_refreshes_experience_status() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    deploy(&repo, false);

    let result = invoke_aethyme(["enhance", "verify", "--repo", &repo.display().to_string()]);
    result.ok();
    result.assert_contains("Experience status:");

    let status_json = repo.join(".aethyme/generated/experience-status.json");
    assert!(status_json.exists());
    assert!(
        repo.join(".aethyme/generated/experience-status.md")
            .exists()
    );

    let payload: Value = serde_json::from_str(&read(&status_json)).unwrap();
    assert!(
        payload["recommended_next_action"]["command"]
            .as_str()
            .is_some_and(|command| !command.is_empty())
    );
}

#[test]
fn enhance_verify_fails_on_direct_agents_edit() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    deploy(&repo, false);

    let agents_path = repo.join("AGENTS.md");
    let edited = read(&agents_path) + "\n\nManual direct edit.\n";
    write(&agents_path, &edited);

    let result = invoke_aethyme(["enhance", "verify", "--repo", &repo.display().to_string()]);
    result.expect_code(1);
    result.assert_contains("AGENTS.md");
    result.assert_contains("direct edits unsupported; use .aethyme/overrides/agents.json");
}

fn codes(values: &Value) -> Vec<String> {
    values
        .as_array()
        .expect("signal/suggestion list")
        .iter()
        .map(|entry| entry["code"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
