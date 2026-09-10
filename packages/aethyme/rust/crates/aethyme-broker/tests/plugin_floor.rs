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
