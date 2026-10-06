//! End-to-end `deploy` / `verify` over throwaway repositories (issue #381).
//!
//! These drive the crate's public entry points the way `aethyme deploy
//! --repo <path>` does, and assert on the files a repository actually
//! receives: every target and its mode, idempotence, overrides, hand edits,
//! the policy rendered from `.aethyme/config.toml`, and the refusals that
//! keep a deploy inside the checkout.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use aethyme_enhance::deploy::{deploy, is_ok, verify, DeployAction, SETTINGS_FILE, TARGETS};
use aethyme_enhance::onboarding::ONBOARDING_JSON_PATH;
use aethyme_enhance::AGENTS_OVERRIDE_PATH;

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aethyme-enhance-it-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        Fixture(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.0.join(relative))
            .unwrap_or_else(|error| panic!("{relative}: {error}"))
    }

    fn commit(&self) {
        git(&self.0, &["init", "-q", "-b", "main"]);
        git(&self.0, &["add", "-A"]);
        git(
            &self.0,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best-effort cleanup of a temp directory; a leftover is harmless.
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn rust_repo(label: &str) -> Fixture {
    let fixture = Fixture::new(label);
    fixture.write(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    fixture.write("src/main.rs", "fn main() {}\n");
    fixture.write("README.md", "# fixture\n");
    fixture.commit();
    fixture
}

fn pnpm_repo(label: &str) -> Fixture {
    let fixture = Fixture::new(label);
    fixture.write(
        "package.json",
        "{\n  \"name\": \"web\",\n  \"private\": true,\n  \"scripts\": {\"test\": \"vitest\"}\n}\n",
    );
    fixture.write("pnpm-workspace.yaml", "packages:\n  - \"apps/*\"\n");
    fixture.write("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    fixture.write(
        "apps/site/package.json",
        "{\n  \"name\": \"site\",\n  \"version\": \"0.0.0\"\n}\n",
    );
    fixture.write("apps/site/src/index.ts", "export const site = 1;\n");
    fixture.commit();
    fixture
}

fn action<'a>(actions: &'a [DeployAction], relative: &str) -> &'a str {
    actions
        .iter()
        .find(|action| action.relative_path == relative)
        .unwrap_or_else(|| panic!("no deploy action for {relative}: {actions:?}"))
        .action
}

fn mode(fixture: &Fixture, relative: &str) -> u32 {
    fs::metadata(fixture.path().join(relative))
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

fn mtime(fixture: &Fixture, relative: &str) -> SystemTime {
    fs::metadata(fixture.path().join(relative))
        .unwrap()
        .modified()
        .unwrap()
}

/// Every path a deploy owns: the static targets, the root policy, the
/// generated onboarding and the merged settings file.
fn owned_paths(fixture: &Fixture) -> Vec<String> {
    let mut paths: Vec<String> = TARGETS.iter().map(|(path, _)| path.to_string()).collect();
    paths.push("AGENTS.md".to_string());
    paths.push(SETTINGS_FILE.to_string());
    paths.push(ONBOARDING_JSON_PATH.to_string());
    for path in &paths {
        assert!(
            fixture.path().join(path).is_file(),
            "deploy did not write {path}"
        );
    }
    paths
}

#[test]
fn fresh_deploy_writes_every_target_with_its_mode_and_verifies() {
    let fixture = rust_repo("fresh");
    let actions = deploy(fixture.path(), false).unwrap();

    for (relative, _) in TARGETS {
        assert_eq!(action(&actions, relative), "created", "{relative}");
    }
    assert_eq!(action(&actions, "AGENTS.md"), "created");
    assert_eq!(action(&actions, SETTINGS_FILE), "created");
    owned_paths(&fixture);

    // Wrappers and hooks are executable; documents are not.
    for (relative, _) in TARGETS {
        let executable = relative.ends_with(".sh") || relative.ends_with("/aethyme-explore");
        let bits = mode(&fixture, relative);
        if executable {
            assert_eq!(
                bits & 0o111,
                0o111,
                "{relative} should be executable: {bits:o}"
            );
        } else {
            assert_eq!(
                bits & 0o111,
                0,
                "{relative} should not be executable: {bits:o}"
            );
        }
    }

    // CLAUDE.md is the rendered agents document, not the raw template.
    let claude = fixture.read("CLAUDE.md");
    assert!(claude.contains("Generated by Aethyme; do not edit."));
    assert!(!claude.contains(aethyme_enhance::PLACEHOLDER));
    assert_eq!(claude, fixture.read("AGENTS.md"));

    // The settings hook is registered exactly once.
    let settings = fixture.read(SETTINGS_FILE);
    assert_eq!(
        settings
            .matches(".claude/hooks/aethyme-load-context.sh")
            .count(),
        1,
        "{settings}"
    );

    let results = verify(fixture.path()).unwrap();
    assert!(is_ok(&results), "{results:#?}");
    assert!(results.iter().all(|result| result.matches_canonical));
}

#[test]
fn second_deploy_is_a_no_op_that_touches_no_file() {
    let fixture = rust_repo("idempotent");
    deploy(fixture.path(), false).unwrap();
    let paths = owned_paths(&fixture);
    let before: Vec<(String, SystemTime, String)> = paths
        .iter()
        .map(|path| (path.clone(), mtime(&fixture, path), fixture.read(path)))
        .collect();
    // Coarse filesystem timestamps would hide a rewrite made in the same tick.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let actions = deploy(fixture.path(), false).unwrap();
    for action in &actions {
        assert_eq!(action.action, "unchanged", "{action:?}");
    }
    for (path, modified, content) in before {
        assert_eq!(fixture.read(&path), content, "{path} changed");
        assert_eq!(mtime(&fixture, &path), modified, "{path} was rewritten");
    }
}

#[test]
fn deploy_restores_a_lost_executable_bit_without_rewriting_the_file() {
    let fixture = rust_repo("exec-bit");
    deploy(fixture.path(), false).unwrap();
    let wrapper = ".claude/skills/aethyme/aethyme-explore";
    let path = fixture.path().join(wrapper);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    let actions = deploy(fixture.path(), false).unwrap();
    assert_eq!(action(&actions, wrapper), "unchanged");
    assert_eq!(mode(&fixture, wrapper) & 0o111, 0o111);
}

#[test]
fn a_pnpm_monorepo_gets_onboarding_for_its_own_toolchain() {
    let fixture = pnpm_repo("pnpm");
    deploy(fixture.path(), false).unwrap();
    let results = verify(fixture.path()).unwrap();
    assert!(is_ok(&results), "{results:#?}");

    let onboarding = fixture.read(ONBOARDING_JSON_PATH);
    assert!(
        onboarding.contains("\"package_manager\": \"pnpm\""),
        "{onboarding}"
    );
    assert!(!onboarding.contains("\"cargo\""), "{onboarding}");
    let agents = fixture.read("AGENTS.md");
    assert!(agents.contains("pnpm"), "{agents}");
    assert!(!agents.contains("cargo test"), "{agents}");
}

#[test]
fn agents_and_onboarding_overrides_shape_the_generated_files() {
    let fixture = rust_repo("overrides");
    fixture.write(
        AGENTS_OVERRIDE_PATH,
        "{\n  \"hard_constraints\": [\"Never touch the billing schema.\"],\n  \"maintainer_markdown\": \"Ask the release captain before tagging.\"\n}\n",
    );
    fixture.write(
        ".aethyme/overrides/onboarding.json",
        "{\n  \"notes\": [\"Fixture note from the override.\"]\n}\n",
    );
    deploy(fixture.path(), false).unwrap();

    let agents = fixture.read("AGENTS.md");
    assert!(agents.contains("## Hard Constraints"), "{agents}");
    assert!(
        agents.contains("Never touch the billing schema."),
        "{agents}"
    );
    assert!(agents.contains("## Maintainer Notes"), "{agents}");
    assert!(agents.contains("Ask the release captain before tagging."));
    assert!(!agents.contains("Aethyme Override Status"));

    let onboarding = fixture.read(ONBOARDING_JSON_PATH);
    assert!(onboarding.contains("Fixture note from the override."));
    assert!(
        onboarding.contains("\"override_invalid\": false"),
        "{onboarding}"
    );
}

#[test]
fn a_malformed_override_is_reported_and_never_rewritten_with_defaults() {
    let fixture = rust_repo("bad-override");
    let broken_agents = "{ \"hard_constraints\": [\"unterminated\" \n";
    let unknown_onboarding = "{\n  \"not_a_field\": true\n}\n";
    fixture.write(AGENTS_OVERRIDE_PATH, broken_agents);
    fixture.write(".aethyme/overrides/onboarding.json", unknown_onboarding);

    deploy(fixture.path(), false).unwrap();

    // The maintainer's files are left exactly as written.
    assert_eq!(fixture.read(AGENTS_OVERRIDE_PATH), broken_agents);
    assert_eq!(
        fixture.read(".aethyme/overrides/onboarding.json"),
        unknown_onboarding
    );
    // The generated policy says so instead of silently dropping the override.
    let agents = fixture.read("AGENTS.md");
    assert!(agents.contains("## Aethyme Override Status"), "{agents}");
    assert!(agents.contains(&format!(
        "Agents override file `{AGENTS_OVERRIDE_PATH}` is invalid JSON"
    )));
    let onboarding = fixture.read(ONBOARDING_JSON_PATH);
    assert!(
        onboarding.contains("\"override_invalid\": true"),
        "{onboarding}"
    );
}

#[test]
fn a_hand_edited_target_is_detected_and_restored() {
    let fixture = rust_repo("hand-edit");
    deploy(fixture.path(), false).unwrap();
    let reference = ".claude/skills/aethyme/references/explore.md";
    let canonical = fixture.read(reference);
    fixture.write(reference, &format!("{canonical}\nLocal edit.\n"));

    let results = verify(fixture.path()).unwrap();
    let edited = results
        .iter()
        .find(|result| result.relative_path == reference)
        .unwrap();
    assert!(!edited.matches_canonical, "{edited:?}");

    let actions = deploy(fixture.path(), false).unwrap();
    assert_eq!(action(&actions, reference), "updated");
    assert_eq!(fixture.read(reference), canonical);
    assert!(is_ok(&verify(fixture.path()).unwrap()));
}

#[test]
fn an_edited_generated_policy_requires_a_reviewed_upgrade() {
    let fixture = rust_repo("policy-edit");
    deploy(fixture.path(), false).unwrap();
    let agents = fixture.read("AGENTS.md");
    let edited = agents.replacen("## Finding code", "## Finding code (edited)", 1);
    assert_ne!(edited, agents, "fixture must change the policy body");
    fixture.write("AGENTS.md", &edited);

    let error = deploy(fixture.path(), false).unwrap_err();
    assert!(
        error.contains("customized generated policy AGENTS.md requires reviewed resolution"),
        "{error}"
    );
    // Refused before writing anything.
    assert_eq!(fixture.read("AGENTS.md"), edited);
}

#[test]
fn the_push_policy_follows_the_repository_config() {
    let cases = [
        (
            "push-on",
            Some("[delivery]\npush_session_branches = true\n"),
            true,
        ),
        (
            "push-off",
            Some("[delivery]\npush_session_branches = false\n"),
            false,
        ),
        (
            "push-string",
            Some("[delivery]\npush_session_branches = \"true\"\n"),
            false,
        ),
        (
            "push-invalid",
            Some("[delivery\npush_session_branches = true\n"),
            false,
        ),
        ("no-config", None, false),
    ];
    for (label, config, granted) in cases {
        let fixture = Fixture::new(label);
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        );
        fixture.write("src/lib.rs", "\n");
        if let Some(config) = config {
            fixture.write(".aethyme/config.toml", config);
        }
        fixture.commit();
        deploy(fixture.path(), false).unwrap();

        let agents = fixture.read("AGENTS.md");
        let grants = agents.contains("policy authorizes pushing your own");
        assert_eq!(grants, granted, "{label}: {agents}");
        // The broker section is rendered only for a broker-configured repo.
        assert_eq!(
            agents.contains("## Broker Coordination"),
            config.is_some(),
            "{label}"
        );
    }
}

#[test]
fn settings_merge_keeps_user_keys_and_backs_up_invalid_json() {
    let fixture = rust_repo("settings");
    fixture.write(
        SETTINGS_FILE,
        "{\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]}\n}\n",
    );
    let actions = deploy(fixture.path(), false).unwrap();
    assert_eq!(action(&actions, SETTINGS_FILE), "updated");
    let merged = fixture.read(SETTINGS_FILE);
    assert!(merged.contains("Bash(ls)"), "{merged}");
    assert!(merged.contains("aethyme-load-context.sh"), "{merged}");

    let other = rust_repo("settings-invalid");
    other.write(SETTINGS_FILE, "not json at all\n");
    let actions = deploy(other.path(), false).unwrap();
    assert_eq!(action(&actions, SETTINGS_FILE), "created");
    assert_eq!(
        other.read(".claude/settings.local.json.bak"),
        "not json at all\n"
    );
    assert!(other
        .read(SETTINGS_FILE)
        .contains("aethyme-load-context.sh"));
}

#[test]
fn deploy_refuses_to_write_through_a_symlinked_target_directory() {
    let fixture = rust_repo("symlink");
    let outside = Fixture::new("symlink-outside");
    fs::create_dir_all(fixture.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.path().join(".claude/skills")).unwrap();

    let error = deploy(fixture.path(), false).unwrap_err();
    assert!(
        error.contains("refusing to write through symlink"),
        "{error}"
    );
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing may land outside the repository"
    );
}
