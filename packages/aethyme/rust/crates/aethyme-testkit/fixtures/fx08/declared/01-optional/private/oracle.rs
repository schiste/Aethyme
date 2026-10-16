#[path = "source.rs"]
mod subject;

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn requested_entry_validation_and_exact_diagnostics() {
    for (input, index, expected) in [
        (" a_9-z ", 7, "a_9-z"),
        ("0", 0, "0"),
        ("_", 1, "_"),
        ("-", 2, "-"),
        ("\talpha\n", 19, "alpha"),
    ] {
        assert_eq!(subject::parse_entry(input, index), Ok(expected.to_string()));
    }
    for (input, index, error) in [
        (" Bad ", 3, "feature entry 3 invalid: Bad"),
        (" \t\n ", 4, "feature entry 4 invalid: "),
        (" café ", 8, "feature entry 8 invalid: café"),
        (" a.b ", 0, "feature entry 0 invalid: a.b"),
        (" a/b ", 23, "feature entry 23 invalid: a/b"),
        ("a b", 2, "feature entry 2 invalid: a b"),
    ] {
        assert_eq!(subject::parse_entry(input, index), Err(error.to_string()));
    }
}

#[test]
fn requested_feature_diagnostics_use_trimmed_tokens_and_real_indexes() {
    for (input, error) in [
        ("search,,labels", "feature entry 1 invalid: "),
        ("search,", "feature entry 1 invalid: "),
        (",search", "feature entry 0 invalid: "),
        ("search, labels, Bad ", "feature entry 2 invalid: Bad"),
        ("search, café ", "feature entry 1 invalid: café"),
        ("search, bad.name ", "feature entry 1 invalid: bad.name"),
    ] {
        assert_eq!(subject::parse_feature(Some(input)), Err(error.to_string()));
    }
}

#[test]
fn survives_absent_empty_whitespace_and_duplicate_distinctions() {
    assert_eq!(subject::parse_feature(None), Ok(names(&["search", "labels"])));
    assert_eq!(subject::parse_feature(Some("")), Ok(Vec::new()));
    for input in ["   ", "\t\n"] {
        assert_eq!(
            subject::parse_feature(Some(input)),
            Err("feature entry 0 invalid: ".to_string())
        );
    }
    assert_eq!(
        subject::parse_feature(Some(" search , labels,search, x_2-y ")),
        Ok(names(&["search", "labels", "search", "x_2-y"]))
    );
}
