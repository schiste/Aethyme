//! The hook floor is stated in three places that cannot see each other: a
//! Rust constant the CLI warns from, prose in the plugin README a human
//! reads before installing, and the marketplace source `plugin install`
//! points at. Nothing makes them agree.
//!
//! That is the same shape as the skew the floor exists to catch. A README
//! promising 0.7.17 next to a constant that has moved to 0.8.0 sends
//! someone to install a CLI that still leaves their hooks inert, and the
//! CLI's own warning would be the only thing telling the truth -- after
//! the fact, which is where this problem always lands.

use aethyme_broker::plugin_cli::{
    DEFAULT_MARKETPLACE_SOURCE, MARKETPLACE_NAME, MIN_HOOK_CLI_VERSION, PLUGIN_ID,
    compare_versions, parse_version, plan_install,
};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PLUGIN_MANIFEST_PATH: &str = "packages/aethyme/plugins/aethyme/.claude-plugin/plugin.json";
const PLUGIN_HOOKS_PATH: &str = "packages/aethyme/plugins/aethyme/hooks";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .expect("repository root")
        .to_path_buf()
}

fn plugin_dir() -> PathBuf {
    repo_root().join("packages/aethyme/plugins/aethyme")
}

fn git_output(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("git must be available to check the bundled plugin version")
}

fn plugin_version(manifest: &[u8]) -> semver::Version {
    let manifest: serde_json::Value =
        serde_json::from_slice(manifest).expect("plugin.json must be valid JSON");
    semver::Version::parse(
        manifest["version"]
            .as_str()
            .expect("plugin.json must declare a string version"),
    )
    .expect("plugin.json version must be semantic")
}

fn require_plugin_version_bump(
    hooks_changed: bool,
    base_version: &semver::Version,
    current_version: &semver::Version,
) -> Result<(), String> {
    if hooks_changed && current_version <= base_version {
        return Err(format!(
            "bundled Claude hooks changed but plugin.json version did not increase \
             ({} -> {}); bump {PLUGIN_MANIFEST_PATH}",
            base_version, current_version
        ));
    }
    Ok(())
}

#[test]
fn the_readme_states_the_floor_the_cli_enforces() {
    let readme = std::fs::read_to_string(plugin_dir().join("README.md")).expect("plugin README");
    assert!(
        readme.contains(MIN_HOOK_CLI_VERSION),
        "the plugin README never mentions {MIN_HOOK_CLI_VERSION}, the floor \
         `aethyme plugin status` reports. Whoever bumps the constant has to \
         bump the prose, or the two send readers to different CLIs."
    );
}

/// The floor names a CLI that exists. A floor above the current release is
/// unsatisfiable: every user is below it and the warning fires forever.
#[test]
fn the_floor_is_not_ahead_of_the_shipping_version() {
    let floor = parse_version(MIN_HOOK_CLI_VERSION).expect("floor parses");
    let shipping = parse_version(env!("CARGO_PKG_VERSION")).expect("crate version parses");
    assert_ne!(
        compare_versions(&floor, &shipping),
        Ordering::Greater,
        "floor {MIN_HOOK_CLI_VERSION} is ahead of the workspace version {}; \
         no released CLI can satisfy it",
        env!("CARGO_PKG_VERSION")
    );
}

/// `<plugin>@<marketplace>` has to name the manifests actually shipped, or
/// `plugin install` registers a marketplace and then asks for a plugin that
/// is not in it.
#[test]
fn the_installed_id_matches_the_shipped_manifests() {
    let plugin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(plugin_dir().join(".claude-plugin/plugin.json"))
            .expect("plugin.json"),
    )
    .expect("plugin.json parses");
    let marketplace: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(".claude-plugin/marketplace.json"))
            .expect("marketplace.json"),
    )
    .expect("marketplace.json parses");

    assert_eq!(
        PLUGIN_ID,
        format!(
            "{}@{}",
            plugin["name"].as_str().expect("plugin name"),
            marketplace["name"].as_str().expect("marketplace name")
        )
    );
    assert_eq!(MARKETPLACE_NAME, marketplace["name"].as_str().unwrap());
}

/// Claude Code caches a plugin by its declared version. Keep this check on the
/// full pull-request diff: it catches the failure from #687 before packaging,
/// even when each individual hook commit was otherwise valid.
#[test]
fn changed_plugin_hooks_require_a_strictly_newer_plugin_version() {
    let root = aethyme_testkit::paths::repo_root();
    let base_ref = std::env::var("GITHUB_BASE_REF")
        .map(|branch| format!("refs/remotes/origin/{branch}"))
        .unwrap_or_else(|_| "refs/remotes/origin/main".to_string());
    let merge_base = git_output(&root, &["merge-base", "HEAD", &base_ref]);
    assert!(
        merge_base.status.success(),
        "cannot find the pull request base ({base_ref}); fetch the base branch before running \
         the plugin hook version guard: {}",
        String::from_utf8_lossy(&merge_base.stderr).trim()
    );
    let merge_base = String::from_utf8(merge_base.stdout)
        .expect("git merge-base output must be UTF-8")
        .trim()
        .to_string();

    let hooks_diff = git_output(
        &root,
        &["diff", "--quiet", &merge_base, "--", PLUGIN_HOOKS_PATH],
    );
    assert!(
        hooks_diff.status.success() || hooks_diff.status.code() == Some(1),
        "git could not compare the bundled hooks with {merge_base}: {}",
        String::from_utf8_lossy(&hooks_diff.stderr).trim()
    );
    let hooks_changed = hooks_diff.status.code() == Some(1);
    let base_manifest = git_output(
        &root,
        &["show", &format!("{merge_base}:{PLUGIN_MANIFEST_PATH}")],
    );
    assert!(
        base_manifest.status.success(),
        "cannot read {PLUGIN_MANIFEST_PATH} from base {merge_base}: {}",
        String::from_utf8_lossy(&base_manifest.stderr).trim()
    );
    let current_manifest = std::fs::read(root.join(PLUGIN_MANIFEST_PATH))
        .expect("read the current Claude plugin manifest");
    let base_version = plugin_version(&base_manifest.stdout);
    let current_version = plugin_version(&current_manifest);
    require_plugin_version_bump(hooks_changed, &base_version, &current_version)
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn the_plugin_version_guard_rejects_an_unchanged_or_lower_version_for_hook_changes() {
    let base = semver::Version::parse("0.1.1").unwrap();
    let unchanged = semver::Version::parse("0.1.1").unwrap();
    let older = semver::Version::parse("0.1.0").unwrap();
    let newer = semver::Version::parse("0.1.2").unwrap();

    assert!(require_plugin_version_bump(true, &base, &unchanged).is_err());
    assert!(require_plugin_version_bump(true, &base, &older).is_err());
    assert!(require_plugin_version_bump(true, &base, &newer).is_ok());
    assert!(require_plugin_version_bump(false, &base, &unchanged).is_ok());
}

/// Every planned command drives the surface's own binary and stays inside
/// its `plugin` namespace. `install` writes to machine-global state under
/// `~/.codex` and `~/.claude`, so the blast radius is worth pinning.
#[test]
fn install_only_ever_runs_plugin_subcommands() {
    for surface in aethyme_broker::plugin_cli::Surface::ALL {
        for step in plan_install(surface, DEFAULT_MARKETPLACE_SOURCE) {
            assert_eq!(step.argv[0], surface.binary(), "{}", step.rendered());
            assert_eq!(step.argv[1], "plugin", "{}", step.rendered());
        }
    }
}
