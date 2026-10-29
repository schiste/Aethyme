use std::path::Path;
use std::time::{Duration, Instant};

use super::*;

fn yaml(text: &str) -> Result<ConfigDocument, ConfigSourceError> {
    parse_config_text("gates.yaml", ConfigFormat::Yaml, text)
}

fn toml_doc(text: &str) -> Result<ConfigDocument, ConfigSourceError> {
    parse_config_text("gates.toml", ConfigFormat::Toml, text)
}

fn scratch() -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "aethyme-config-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

const TOML_FIXTURE: &str = r#"
schema = 1
name = "ci"
ratio = 0.5
enabled = true

[review]
enabled = false
labels = ["a", "b"]

[[gate]]
name = "test"
command = ["cargo", "test"]
timeout_secs = 600

[[gate]]
name = "lint"
command = ["cargo", "clippy"]
env = { RUST_LOG = "warn" }
"#;

const YAML_FIXTURE: &str = r#"
schema: 1
name: ci
ratio: 0.5
enabled: true
review:
  enabled: false
  labels: [a, b]
gate:
  - name: test
    command: [cargo, test]
    timeout_secs: 600
  - name: lint
    command:
      - cargo
      - clippy
    env: {RUST_LOG: warn}
"#;

#[test]
fn yaml_translation_produces_the_identical_value_and_normalised_bytes() {
    let toml_document = toml_doc(TOML_FIXTURE).unwrap();
    let yaml_document = yaml(YAML_FIXTURE).unwrap();
    assert_eq!(toml_document.value, yaml_document.value);
    assert_eq!(
        toml_document.normalized_bytes(),
        yaml_document.normalized_bytes()
    );
    assert_eq!(yaml_document.format, ConfigFormat::Yaml);
}

#[test]
fn yaml_renders_to_toml_text_that_parses_back_to_the_same_value() {
    let text = ConfigText {
        path: PathBuf::from("gates.yaml"),
        format: ConfigFormat::Yaml,
        text: YAML_FIXTURE.to_string(),
    };
    let rendered = text.toml_text().unwrap();
    let reparsed: toml::Value = toml::from_str(&rendered).unwrap();
    assert_eq!(reparsed, toml_doc(TOML_FIXTURE).unwrap().value);
    let toml_text = ConfigText {
        path: PathBuf::from("gates.toml"),
        format: ConfigFormat::Toml,
        text: TOML_FIXTURE.to_string(),
    };
    assert_eq!(toml_text.toml_text().unwrap(), TOML_FIXTURE);
    assert_eq!(
        toml_text.normalized_bytes().unwrap(),
        text.normalized_bytes().unwrap()
    );
}

#[test]
fn both_formats_locate_key_paths() {
    let toml_document = toml_doc(TOML_FIXTURE).unwrap();
    let yaml_document = yaml(YAML_FIXTURE).unwrap();
    assert_eq!(
        toml_document.location("review.enabled"),
        Some(Location { line: 8, column: 1 })
    );
    assert_eq!(
        yaml_document.location("review.enabled"),
        Some(Location { line: 7, column: 3 })
    );
    assert_eq!(
        yaml_document.location("gate[1].name"),
        Some(Location {
            line: 13,
            column: 5
        })
    );
    // serde prints paths without indices; they still resolve.
    assert_eq!(
        yaml_document.location("gate.command"),
        Some(Location {
            line: 11,
            column: 5
        })
    );
}

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
struct Review {
    enabled: bool,
}

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
struct WithReview {
    review: Review,
}

#[test]
fn yaml_no_is_the_string_no_and_a_boolean_field_fails_with_file_line_col() {
    let document = yaml("review:\n  enabled: no\n").unwrap();
    assert_eq!(
        document.value["review"]["enabled"],
        toml::Value::String("no".into())
    );
    let error = document.deserialize::<WithReview>().unwrap_err();
    assert_eq!(error.code(), "config_type");
    let rendered = error.to_string();
    assert!(
        rendered.starts_with("gates.yaml:2:3 (review.enabled): invalid type: string \"no\""),
        "{rendered}"
    );
}

#[test]
fn toml_type_errors_use_the_same_located_form() {
    let document = toml_doc("[review]\nenabled = \"no\"\n").unwrap();
    let rendered = document
        .deserialize::<WithReview>()
        .unwrap_err()
        .to_string();
    assert!(
        rendered.starts_with("gates.toml:2:1 (review.enabled): invalid type"),
        "{rendered}"
    );
}

#[test]
fn yaml_one_point_two_core_schema_scalars() {
    let document = yaml("a: on\nb: yes\nc: 010\nd: 0x10\ne: 0o17\nf: True\n").unwrap();
    assert_eq!(document.value["a"], toml::Value::String("on".into()));
    assert_eq!(document.value["b"], toml::Value::String("yes".into()));
    assert_eq!(document.value["c"], toml::Value::Integer(10));
    assert_eq!(document.value["d"], toml::Value::Integer(16));
    assert_eq!(document.value["e"], toml::Value::Integer(15));
    assert_eq!(document.value["f"], toml::Value::Boolean(true));
}

#[test]
fn yaml_null_is_refused_with_its_location() {
    for text in ["key: ~\n", "key: null\n", "key:\n", "list:\n  - ~\n"] {
        let error = yaml(text).unwrap_err();
        assert_eq!(error.code(), "yaml_null", "{text:?}: {error}");
        assert!(error.to_string().starts_with("gates.yaml:"), "{error}");
    }
    let rendered = yaml("ok: 1\nkey: ~\n").unwrap_err().to_string();
    assert!(
        rendered.starts_with("gates.yaml:2:1 (key): null"),
        "{rendered}"
    );
}

#[test]
fn yaml_duplicate_keys_are_refused() {
    let error = yaml("a: 1\nb: 2\na: 3\n").unwrap_err();
    assert_eq!(error.code(), "yaml_duplicate_key");
    assert!(error.to_string().contains("gates.yaml:3:1"), "{error}");
    let nested = yaml("x:\n  y: 1\n  y: 2\n").unwrap_err();
    assert_eq!(nested.code(), "yaml_duplicate_key");
}

#[test]
fn yaml_tags_are_refused() {
    for text in [
        "x: !foo bar\n",
        "x: !!str 1\n",
        "x: !!int '3'\n",
        "x:\n  - !custom {a: 1}\n",
    ] {
        let error = yaml(text).unwrap_err();
        assert_eq!(error.code(), "yaml_tag", "{text:?}: {error}");
    }
}

#[test]
fn yaml_merge_keys_are_refused() {
    let error = yaml("base: &base {a: 1}\nderived:\n  <<: *base\n  b: 2\n").unwrap_err();
    assert_eq!(error.code(), "yaml_merge_key", "{error}");
    assert!(error.to_string().contains("gates.yaml:3:3"), "{error}");
}

#[test]
fn yaml_anchors_and_aliases_are_allowed() {
    let document = yaml("shared: &cmd [cargo, test]\ngate:\n  - command: *cmd\n").unwrap();
    assert_eq!(
        document.value["gate"][0]["command"],
        document.value["shared"]
    );
}

#[test]
fn yaml_non_string_keys_are_refused() {
    for text in [
        "1: a\n",
        "true: a\n",
        "~: a\n",
        "1.5: a\n",
        "outer:\n  2: b\n",
        "outer: {3: c}\n",
        "outer: {ok: 1, false: 2}\n",
        "? [a, b]\n: c\n",
        "[a]: b\n",
    ] {
        let error = yaml(text).unwrap_err();
        assert_eq!(error.code(), "yaml_non_string_key", "{text:?}: {error}");
    }
    // Quoted keys are strings.
    let document = yaml("'1': a\n\"true\": b\nyes: c\n").unwrap();
    assert_eq!(document.value["1"], toml::Value::String("a".into()));
    assert_eq!(document.value["true"], toml::Value::String("b".into()));
    assert_eq!(document.value["yes"], toml::Value::String("c".into()));
}

#[test]
fn yaml_integers_outside_i64_are_refused() {
    for text in [
        "x: 9223372036854775808\n",
        "x: -9223372036854775809\n",
        "x: 18446744073709551615\n",
        "x: [1, 99999999999999999999]\n",
        "x: +0x8000000000000000\n",
        "x: -0x8000000000000001\n",
        "x: +0o1000000000000000000000\n",
        "x: -0o1000000000000000000001\n",
    ] {
        let error = yaml(text).unwrap_err();
        assert_eq!(error.code(), "yaml_integer_range", "{text:?}: {error}");
    }
    let document = yaml(
        "x: 9223372036854775807\ny: 1.0e30\nhex: +0x7fffffffffffffff\noctal: -0o1000000000000000000000\n",
    )
    .unwrap();
    assert_eq!(document.value["x"], toml::Value::Integer(i64::MAX));
    assert_eq!(document.value["hex"], toml::Value::Integer(i64::MAX));
    assert_eq!(document.value["octal"], toml::Value::Integer(i64::MIN));
    assert_eq!(document.value["y"], toml::Value::Float(1.0e30));
}

#[test]
fn yaml_multi_document_streams_are_refused() {
    let error = yaml("---\na: 1\n---\nb: 2\n").unwrap_err();
    assert_eq!(error.code(), "yaml_multiple_documents", "{error}");
    // A single explicit document is fine.
    assert!(yaml("---\na: 1\n").is_ok());
}

#[test]
fn yaml_top_level_must_be_a_mapping_and_empty_is_an_empty_table() {
    assert_eq!(yaml("- a\n").unwrap_err().code(), "config_not_a_table");
    for empty in ["", "# nothing configured\n", "---\n# no values\n"] {
        let document = yaml(empty).unwrap();
        assert_eq!(document.value, toml::Value::Table(toml::Table::new()));
    }
    for explicit_null in ["null\n", "~\n", "---\nnull\n"] {
        let error = yaml(explicit_null).unwrap_err();
        assert_eq!(error.code(), "yaml_null", "{explicit_null:?}: {error}");
    }
}

#[test]
fn alias_bomb_fails_within_the_expansion_cap_quickly() {
    let mut text = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..=9 {
        let previous = level - 1;
        let refs = vec![format!("*a{previous}"); 10].join(", ");
        text.push_str(&format!("a{level}: &a{level} [{refs}]\n"));
    }
    let started = Instant::now();
    let error = yaml(&text).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(error.code(), "yaml_alias_budget", "{error}");
    assert!(
        elapsed < Duration::from_secs(5),
        "alias bomb took {elapsed:?}"
    );
}

#[test]
fn toml_syntax_errors_are_located() {
    let error = toml_doc("a = 1\nb = [\n").unwrap_err();
    assert_eq!(error.code(), "toml_syntax");
    assert!(error.to_string().starts_with("gates.toml:"), "{error}");
}

#[test]
fn two_twins_in_a_directory_is_an_error_naming_both() {
    let dir = scratch();
    std::fs::write(dir.join("gates.toml"), "schema = 1\n").unwrap();
    assert_eq!(
        resolve_config_path(&dir, "gates").unwrap(),
        Some(dir.join("gates.toml"))
    );
    std::fs::write(dir.join("gates.yaml"), "schema: 1\n").unwrap();
    let error = load_config(&dir, "gates").unwrap_err();
    assert_eq!(error.code(), "config_ambiguous_format");
    let rendered = error.to_string();
    assert!(rendered.contains("gates.toml"), "{rendered}");
    assert!(rendered.contains("gates.yaml"), "{rendered}");
    assert!(config_exists(&dir, "gates"));
    std::fs::remove_file(dir.join("gates.toml")).unwrap();
    let document = load_config(&dir, "gates").unwrap().unwrap();
    assert_eq!(document.format, ConfigFormat::Yaml);
    assert_eq!(document.value["schema"], toml::Value::Integer(1));
    assert!(load_config(&dir, "prepare").unwrap().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn dangling_config_twin_is_not_treated_as_missing_or_fallback() {
    use std::os::unix::fs::symlink;

    let dir = scratch();
    let dangling = dir.join("gates.yaml");
    symlink("missing-target", &dangling).unwrap();
    std::fs::write(dir.join("gates.toml"), "schema = 1").unwrap();

    let error = load_config(&dir, "gates").unwrap_err();
    assert_eq!(error.code(), "config_read_failed");
    assert_eq!(error.path(), Some(dangling.as_path()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reader_based_loading_serves_commit_reads() {
    let files = [
        (".aethyme/config.yml", "review:\n  enabled: true\n"),
        (".aethyme/gates.toml", "schema = 1\n"),
        (".aethyme/gates.yml", "schema: 1\n"),
    ];
    let read = |relpath: &str| -> Result<Option<String>, ConfigSourceError> {
        Ok(files
            .iter()
            .find(|(path, _)| *path == relpath)
            .map(|(_, text)| text.to_string()))
    };
    let config = load_config_with(CONFIG_DIR, "config", read)
        .unwrap()
        .unwrap();
    assert_eq!(config.path, Path::new(".aethyme/config.yml"));
    assert_eq!(
        config.value["review"]["enabled"],
        toml::Value::Boolean(true)
    );
    let error = load_config_with(CONFIG_DIR, "gates", read).unwrap_err();
    assert_eq!(error.code(), "config_ambiguous_format");
    assert!(
        load_config_with(CONFIG_DIR, "prepare", read)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unsafe_stems_are_rejected_before_filesystem_or_git_callbacks() {
    use std::cell::Cell;
    use std::convert::Infallible;

    for stem in [
        "../outside",
        "..\\outside",
        ".",
        "",
        "nested/name",
        "CON",
        "nul",
        "COM1",
        "LPT9",
        "con.backup",
    ] {
        let called = Cell::new(false);
        let resolved = try_read_config_text_with::<Infallible, _>(".aethyme", stem, |_| {
            called.set(true);
            Ok(None)
        })
        .unwrap()
        .unwrap_err();
        assert_eq!(resolved.code(), "config_invalid_stem", "{stem:?}");
        assert!(!called.get(), "reader ran for unsafe stem {stem:?}");
        assert!(resolve_config_path(Path::new("/tmp"), stem).is_err());
        assert!(!config_exists(Path::new("/tmp"), stem));
        assert!(present_config_paths(Path::new("/tmp"), stem).is_empty());
    }
}

#[test]
fn stems_and_twins_are_recognised() {
    assert_eq!(config_stem_of(".aethyme/gates.yaml"), Some("gates"));
    assert_eq!(config_stem_of(".aethyme/config.yml"), Some("config"));
    assert_eq!(config_stem_of(".aethyme/config.toml"), Some("config"));
    assert_eq!(config_stem_of(".aethyme/config.json"), None);
    assert_eq!(config_stem_of(".aethyme/generated/config.toml"), None);
    assert_eq!(config_stem_of("gates.toml"), None);
    assert!(is_config_twin(".aethyme/prepare.yml", "prepare"));
    assert_eq!(
        candidate_relpaths("broker"),
        [
            ".aethyme/broker.toml".to_string(),
            ".aethyme/broker.yaml".to_string(),
            ".aethyme/broker.yml".to_string()
        ]
    );
}

#[test]
fn a_value_toml_cannot_serialize_is_refused_not_rendered_empty() {
    let document = ConfigDocument {
        format: ConfigFormat::Yaml,
        path: PathBuf::from("gates.yaml"),
        value: toml::Value::String("not a table".into()),
        spans: ConfigSpans::default(),
    };
    let error = document.to_toml_string().unwrap_err();
    assert_eq!(error.code(), "config_unrepresentable");
    assert!(error.to_string().starts_with("gates.yaml: "), "{error}");
    // A real document still renders and round-trips.
    let rendered = yaml("schema: 1\n").unwrap().to_toml_string().unwrap();
    assert_eq!(rendered.trim(), "schema = 1");
}
