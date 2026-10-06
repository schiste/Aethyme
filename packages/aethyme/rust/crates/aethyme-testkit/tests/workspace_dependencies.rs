//! Every registry dependency's requirement lives once, in the workspace's
//! `[workspace.dependencies]` table (#383). A crate manifest names a
//! dependency with `workspace = true` or a `path`, never a version of its
//! own, so two crates cannot drift onto different requirements.
use std::path::Path;

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
}

#[test]
fn crate_manifests_take_registry_requirements_from_the_workspace() {
    let mut offenders = Vec::new();
    let crates = workspace_root().join("crates");
    let mut manifests: Vec<_> = std::fs::read_dir(&crates)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("Cargo.toml"))
        .filter(|manifest| manifest.is_file())
        .collect();
    manifests.sort();
    assert!(!manifests.is_empty(), "no crate manifests under {crates:?}");

    for manifest in &manifests {
        let parsed: toml::Table = std::fs::read_to_string(manifest).unwrap().parse().unwrap();
        for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(dependencies) = parsed.get(table).and_then(toml::Value::as_table) else {
                continue;
            };
            for (name, spec) in dependencies {
                let from_workspace = spec.as_table().is_some_and(|spec| {
                    spec.get("workspace").and_then(toml::Value::as_bool) == Some(true)
                        || spec.contains_key("path")
                });
                if !from_workspace {
                    let crate_name = manifest.parent().unwrap().file_name().unwrap();
                    offenders.push(format!("{}: [{table}] {name}", crate_name.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "declare these in [workspace.dependencies] and use `workspace = true`:\n{}",
        offenders.join("\n")
    );
}
