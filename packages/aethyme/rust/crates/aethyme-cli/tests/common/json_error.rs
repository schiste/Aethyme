use std::process::Output;

/// Validate and return a broker CLI error message from its JSON stdout value.
/// Parsing the complete byte slice also rejects a second trailing document.
pub fn message(output: &Output) -> String {
    let status = output
        .status
        .code()
        .expect("broker error command exits with a process status");
    assert_ne!(status, 0, "expected a nonzero exit: {output:?}");

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("expected one JSON error document: {error}; {output:?}"));
    assert_eq!(value["schema"], "aethyme.broker-error/v1", "{value}");
    let error = &value["error"];
    assert_eq!(error["code"].as_i64(), Some(i64::from(status)), "{value}");
    error["message"]
        .as_str()
        .unwrap_or_else(|| panic!("error.message must be a string: {value}"))
        .to_owned()
}
