//! Offline comparison of host-supplied evidence; never a publication gate.
use super::{optional, read};
use aethyme_broker::content_gate::{
    candidate_revision_v2::{Item, Snapshot},
    portable_compare_v2::{ExperimentalMatch, Inputs, compare},
    strict::parse_unique,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    previous: PathBuf,
    fresh: PathBuf,
    base: PathBuf,
    after: PathBuf,
    change: PathBuf,
    resolved_policy: PathBuf,
    host_options: PathBuf,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    collection: Option<PathBuf>,
    evaluator_identity: PathBuf,
    validator_name: String,
    validator_version: String,
    required_checks: Vec<String>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    fresh_exit_code: Option<i32>,
    fresh_timed_out: bool,
}

fn execute(
    path: &Path,
    timings: &mut BTreeMap<&'static str, u128>,
) -> Result<ExperimentalMatch, String> {
    let started = Instant::now();
    let manifest = read(path)?;
    let m: Manifest = serde_json::from_value(parse_unique(&manifest).map_err(str::to_owned)?)
        .map_err(|_| "invalid comparison manifest".to_owned())?;
    let base: Snapshot =
        serde_json::from_value(parse_unique(&read(&m.base)?).map_err(str::to_owned)?)
            .map_err(|_| "invalid base snapshot".to_owned())?;
    let after: Vec<Item> =
        serde_json::from_value(parse_unique(&read(&m.after)?).map_err(str::to_owned)?)
            .map_err(|_| "invalid after projections".to_owned())?;
    let previous = read(&m.previous)?;
    let fresh = read(&m.fresh)?;
    let change = read(&m.change)?;
    let policy = read(&m.resolved_policy)?;
    let options = read(&m.host_options)?;
    let collection = optional(&m.collection)?;
    let identity = read(&m.evaluator_identity)?;
    let required: Vec<_> = m.required_checks.iter().map(String::as_str).collect();
    timings.insert("input_read_and_parse_ms", started.elapsed().as_millis());
    let started = Instant::now();
    let result = compare(
        &previous,
        &fresh,
        &Inputs {
            base: &base,
            after: &after,
            change: &change,
            resolved_policy: &policy,
            host_options: &options,
            collection: collection.as_deref(),
            evaluator_identity: &identity,
            expected_validator_name: &m.validator_name,
            expected_validator_version: &m.validator_version,
            required_checks: &required,
            fresh_exit_code: m.fresh_exit_code,
            fresh_timed_out: m.fresh_timed_out,
        },
    )
    .map_err(str::to_owned);
    timings.insert("compare_ms", started.elapsed().as_millis());
    result
}

pub(super) fn run(args: &[String]) -> u8 {
    let mut timings = BTreeMap::new();
    let started = Instant::now();
    let (result, include_coverage) = match args {
        [path] => (execute(Path::new(path), &mut timings), false),
        [path, flag] if flag == "--telemetry" => (execute(Path::new(path), &mut timings), true),
        _ => (
            Err("expected one comparison manifest and optional --telemetry".to_owned()),
            false,
        ),
    };
    timings.insert("total_ms", started.elapsed().as_millis());
    let (status, code, reason, coverage) = match result {
        Ok(matched) => (
            "match",
            0,
            None,
            Some(serde_json::json!({
                "policy_gate_count": matched.policy_gate_count,
                "policy_check_count": matched.policy_check_count,
            })),
        ),
        Err(reason) => ("not_matched", 2, Some(reason), None),
    };
    let mut output = serde_json::json!({"schema":"aethyme.content-gate-offline-comparison/v1", "status":status,
        "publication_authorized":false, "reason":reason, "timings_ms":timings});
    if include_coverage {
        output["coverage"] = serde_json::json!(coverage);
    }
    println!("{output}");
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    #[test]
    fn compares_shared_vector_and_rejects_changed_inputs() {
        let v: Value = serde_json::from_str(include_str!(
            "../../../aethyme-broker/src/content_gate/portable-verdict-vector.json"
        ))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, data: &[u8]| {
            let p = dir.path().join(name);
            fs::write(&p, data).unwrap();
            p
        };
        let old = file("old", v["portable"].to_string().as_bytes());
        let base = file("base", v["base"].to_string().as_bytes());
        let after = file("after", v["after"].to_string().as_bytes());
        let artifact = |name: &str| file(name, v["artifacts"][name].as_str().unwrap().as_bytes());
        let change = artifact("change");
        let policy = artifact("resolved_policy");
        let options = artifact("host_options");
        let identity = artifact("evaluator");
        let manifest = json!({"previous":old,"fresh":old,"base":base,"after":after,"change":change,
            "resolved_policy":policy,"host_options":options,"collection":null,"evaluator_identity":identity,
            "validator_name":"aexeo","validator_version":"0.0.24","required_checks":["gate:7:article:words"],
            "fresh_exit_code":0,"fresh_timed_out":false});
        let path = file("manifest", manifest.to_string().as_bytes());
        let matched = execute(&path, &mut BTreeMap::new()).unwrap();
        assert_eq!(
            (matched.policy_gate_count, matched.policy_check_count),
            (1, 1)
        );
        fs::write(options, b"changed").unwrap();
        assert!(execute(&path, &mut BTreeMap::new()).is_err());
        fs::write(&path, b"{\"previous\":1,\"previous\":2}").unwrap();
        assert!(execute(&path, &mut BTreeMap::new()).is_err());
    }
}
