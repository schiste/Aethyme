//! #1223: every `.aethyme` configuration stem behaves identically whether it
//! is written as TOML or as its YAML translation, and two twins are an error.
//!
//! The fixtures are this repository's own `.aethyme/*.toml` files, translated
//! to YAML by serializing the parsed value, and each subsystem's loader runs
//! over both formats.

use std::path::{Path, PathBuf};

use aethyme_broker::config_source;

fn repository_config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../.aethyme")
}

fn fixture(stem: &str) -> String {
    std::fs::read_to_string(repository_config_dir().join(format!("{stem}.toml")))
        .unwrap_or_else(|error| panic!("fixture .aethyme/{stem}.toml: {error}"))
}

fn yaml_translation(toml_text: &str) -> String {
    let value: toml::Value = toml::from_str(toml_text).unwrap();
    noyalib::to_string(&value).unwrap()
}

/// A checkout root holding `.aethyme/<stem>.<extension>` files.
fn checkout(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
    for (name, text) in files {
        std::fs::write(root.path().join(".aethyme").join(name), text).unwrap();
    }
    root
}

/// The same subsystem loader over a TOML checkout and its YAML twin.
fn both<T: std::fmt::Debug>(stem: &str, load: impl Fn(&Path) -> T) -> (String, String) {
    let toml_text = fixture(stem);
    let yaml_text = yaml_translation(&toml_text);
    let toml_root = checkout(&[(&format!("{stem}.toml"), &toml_text)]);
    let yaml_root = checkout(&[(&format!("{stem}.yaml"), &yaml_text)]);
    (
        format!("{:?}", load(toml_root.path())),
        format!("{:?}", load(yaml_root.path())),
    )
}

#[test]
fn every_fixture_translates_to_an_identical_value() {
    for stem in ["config", "gates", "prepare"] {
        let toml_text = fixture(stem);
        let toml_document = config_source::parse_config_text(
            "x.toml",
            config_source::ConfigFormat::Toml,
            &toml_text,
        )
        .unwrap();
        let yaml_document = config_source::parse_config_text(
            "x.yaml",
            config_source::ConfigFormat::Yaml,
            &yaml_translation(&toml_text),
        )
        .unwrap_or_else(|error| panic!("{stem}: {error}"));
        assert_eq!(toml_document.value, yaml_document.value, "{stem}");
        assert_eq!(
            toml_document.normalized_bytes(),
            yaml_document.normalized_bytes(),
            "{stem}"
        );
    }
}

#[test]
fn gates_load_identically_from_both_formats() {
    let (toml, yaml) = both("gates", |root| {
        aethyme_broker::load_gates(root)
            .unwrap()
            .into_iter()
            .map(|gate| (gate.name, gate.command))
            .collect::<Vec<_>>()
    });
    assert_eq!(toml, yaml);
    assert!(toml.len() > 10, "{toml}");
}

#[test]
fn config_policies_load_identically_from_both_formats() {
    let checks: Vec<(&str, (String, String))> = vec![
        (
            "review.trigger",
            both("config", |root| {
                aethyme_broker::ReviewTriggerPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
        (
            "review.classification",
            both("config", |root| {
                aethyme_broker::ChangeClassificationPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
        (
            "review.routing",
            both("config", |root| {
                aethyme_broker::ReviewRoutingPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
        (
            "review.projection",
            both("config", |root| {
                aethyme_broker::PrProjectionPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
        (
            "review.reporting",
            both("config", |root| {
                aethyme_broker::ReviewReportingPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
        (
            "review lifecycle",
            both("config", |root| {
                aethyme_broker::ReviewPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
        (
            "cleanup",
            both("config", |root| {
                aethyme_broker::AutoCleanupPolicy::load(root)
            }),
        ),
        (
            "graph",
            both("config", |root| {
                aethyme_broker::GraphIntegrityPolicy::load(root).map_err(|e| e.to_string())
            }),
        ),
    ];
    for (name, (toml, yaml)) in checks {
        assert_eq!(toml, yaml, "{name}");
        assert!(!toml.contains("Err("), "{name}: {toml}");
    }
}

#[test]
fn broker_retention_loads_identically_from_both_formats() {
    let toml_text = "schema_version = 1\n";
    let toml_root = checkout(&[("broker.toml", toml_text)]);
    let yaml_root = checkout(&[("broker.yaml", &yaml_translation(toml_text))]);
    assert_eq!(
        format!(
            "{:?}",
            aethyme_broker::load_retention_policy_report(toml_root.path()).map(|r| r.policy)
        ),
        format!(
            "{:?}",
            aethyme_broker::load_retention_policy_report(yaml_root.path()).map(|r| r.policy)
        )
    );
}

#[test]
fn gates_toml_and_gates_yaml_together_is_an_error_naming_both() {
    let toml_text = fixture("gates");
    let root = checkout(&[
        ("gates.toml", &toml_text),
        ("gates.yaml", &yaml_translation(&toml_text)),
    ]);
    let error = aethyme_broker::load_gates(root.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("gates.toml"), "{error}");
    assert!(error.contains("gates.yaml"), "{error}");
}

#[test]
fn config_yml_and_config_toml_together_refuse_the_review_policies() {
    let root = checkout(&[
        ("config.toml", "[review]\n"),
        ("config.yml", "review: {}\n"),
    ]);
    let error = aethyme_broker::ReviewTriggerPolicy::load(root.path())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("config.toml") && error.contains("config.yml"),
        "{error}"
    );
}

#[test]
fn a_yaml_boolean_written_as_no_fails_the_typed_policy() {
    let root = checkout(&[("config.yaml", "cleanup:\n  auto_remove: no\n")]);
    let error = aethyme_broker::AutoCleanupPolicy::load(root.path()).unwrap_err();
    assert!(
        error.contains("auto_remove must be true or false"),
        "{error}"
    );
}
