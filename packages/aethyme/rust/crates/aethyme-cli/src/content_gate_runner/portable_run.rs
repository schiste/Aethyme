//! Experimental supervised comparison. This is not a publication gate.
use super::{optional, read, supervisor};
use aethyme_broker::content_gate::{
    candidate_revision_v2::{Item, Snapshot},
    portable_compare_v2::{Inputs, compare},
    strict::parse_unique,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    previous: PathBuf,
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
    validator: Vec<String>,
    validator_sha256: String,
    env: BTreeMap<String, String>,
    timeout_seconds: u64,
    additional: BTreeMap<String, PathBuf>,
}

#[derive(PartialEq, Eq)]
struct SnapshotInputs {
    manifest: Vec<u8>,
    previous: Vec<u8>,
    base: Vec<u8>,
    after: Vec<u8>,
    change: Vec<u8>,
    policy: Vec<u8>,
    options: Vec<u8>,
    collection: Option<Vec<u8>>,
    evaluator: Vec<u8>,
    executable: [u8; 32],
    additional: BTreeMap<String, Vec<u8>>,
}

fn executable_digest(path: &Path) -> Result<[u8; 32], String> {
    if !path.is_absolute()
        || !fs::metadata(path)
            .map_err(|_| "cannot stat validator")?
            .is_file()
    {
        return Err("validator must be an absolute regular file".into());
    }
    let file = fs::File::open(path).map_err(|_| "cannot open validator")?;
    let mut hasher = Sha256::new();
    let length = io::copy(&mut file.take(512 * 1024 * 1024 + 1), &mut hasher)
        .map_err(|_| "cannot hash validator")?;
    if length > 512 * 1024 * 1024 {
        return Err("validator exceeds size limit".into());
    }
    Ok(hasher.finalize().into())
}

fn snapshot(path: &Path, m: &Manifest) -> Result<SnapshotInputs, String> {
    if m.additional.len() > 1024 {
        return Err("too many additional inputs".into());
    }
    let mut total = 0usize;
    let mut additional = BTreeMap::new();
    for (name, path) in &m.additional {
        if name.trim().is_empty() || name.contains('\0') {
            return Err("invalid additional input name".into());
        }
        let bytes = read(path)?;
        total = total
            .checked_add(bytes.len())
            .filter(|n| *n <= 64 * 1024 * 1024)
            .ok_or("additional inputs exceed size limit")?;
        additional.insert(name.clone(), bytes);
    }
    let captured = SnapshotInputs {
        manifest: read(path)?,
        previous: read(&m.previous)?,
        base: read(&m.base)?,
        after: read(&m.after)?,
        change: read(&m.change)?,
        policy: read(&m.resolved_policy)?,
        options: read(&m.host_options)?,
        collection: optional(&m.collection)?,
        evaluator: read(&m.evaluator_identity)?,
        executable: executable_digest(Path::new(m.validator.first().ok_or("missing validator")?))?,
        additional,
    };
    let lengths = [
        captured.manifest.len(),
        captured.previous.len(),
        captured.base.len(),
        captured.after.len(),
        captured.change.len(),
        captured.policy.len(),
        captured.options.len(),
        captured.collection.as_ref().map_or(0, Vec::len),
        captured.evaluator.len(),
        total,
    ];
    if lengths
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .is_none_or(|n| n > 64 * 1024 * 1024)
    {
        return Err("comparison inputs exceed size limit".into());
    }
    Ok(captured)
}

fn execute(path: &Path, timings: &mut BTreeMap<&'static str, u128>) -> Result<(), String> {
    let started = Instant::now();
    let manifest = read(path)?;
    let m: Manifest = serde_json::from_value(parse_unique(&manifest).map_err(str::to_owned)?)
        .map_err(|_| "invalid supervised comparison manifest".to_owned())?;
    if m.validator_sha256.len() != 64
        || !m
            .validator_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid validator digest".into());
    }
    let before = snapshot(path, &m)?;
    if before.manifest != manifest {
        return Err("manifest changed".into());
    }
    let actual: String = before
        .executable
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != m.validator_sha256 {
        return Err("validator digest differs".into());
    }
    let base: Snapshot = serde_json::from_value(parse_unique(&before.base).map_err(str::to_owned)?)
        .map_err(|_| "invalid base snapshot".to_owned())?;
    let after: Vec<Item> =
        serde_json::from_value(parse_unique(&before.after).map_err(str::to_owned)?)
            .map_err(|_| "invalid after projections".to_owned())?;
    timings.insert("input_read_and_parse_ms", started.elapsed().as_millis());
    let temp = tempfile::tempdir().map_err(|_| "cannot create verdict directory")?;
    let verdict = temp.path().join("verdict.json");
    let change = fs::canonicalize(&m.change).map_err(|_| "cannot resolve change")?;
    let started = Instant::now();
    let status = supervisor::run(
        &m.validator,
        &m.env,
        &change,
        &verdict,
        m.timeout_seconds,
        false,
        true,
    )?;
    timings.insert("validator_ms", started.elapsed().as_millis());
    if !fs::symlink_metadata(&verdict)
        .map_err(|_| "validator did not produce verdict")?
        .file_type()
        .is_file()
    {
        return Err("verdict is not a regular file".into());
    }
    let fresh = read(&verdict)?;
    let started = Instant::now();
    let current = snapshot(path, &m)?;
    if before != current {
        return Err("inputs changed during validation".into());
    }
    timings.insert("input_recheck_ms", started.elapsed().as_millis());
    let required: Vec<_> = m.required_checks.iter().map(String::as_str).collect();
    let started = Instant::now();
    let result = compare(
        &before.previous,
        &fresh,
        &Inputs {
            base: &base,
            after: &after,
            change: &before.change,
            resolved_policy: &before.policy,
            host_options: &before.options,
            collection: before.collection.as_deref(),
            evaluator_identity: &before.evaluator,
            expected_validator_name: &m.validator_name,
            expected_validator_version: &m.validator_version,
            required_checks: &required,
            fresh_exit_code: status.code(),
            fresh_timed_out: false,
        },
    )
    .map_err(str::to_owned);
    timings.insert("compare_ms", started.elapsed().as_millis());
    result?;
    Ok(())
}

pub(super) fn run(args: &[String]) -> u8 {
    let started = Instant::now();
    let mut timings = BTreeMap::new();
    let result = if args.len() == 1 {
        execute(Path::new(&args[0]), &mut timings)
    } else {
        Err("expected one supervised comparison manifest".to_owned())
    };
    timings.insert("total_ms", started.elapsed().as_millis());
    let (status, code, reason) = match result {
        Ok(()) => ("match", 0, None),
        Err(reason) => ("not_matched", 2, Some(reason)),
    };
    println!(
        "{}",
        serde_json::json!({"schema":"aethyme.content-gate-supervised-comparison/v1", "status":status,
        "publication_authorized":false, "reason":reason, "timings_ms":timings})
    );
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn supervised_comparison_runs_process_and_rejects_changed_inputs_and_wrong_binary() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../aethyme-broker/src/content_gate/portable-verdict-vector.json"
        ))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let previous = file("previous", fixture["portable"].to_string().as_bytes());
        let base = file("base", fixture["base"].to_string().as_bytes());
        let after = file("after", fixture["after"].to_string().as_bytes());
        let artifact = |name: &str| {
            file(
                name,
                fixture["artifacts"][name].as_str().unwrap().as_bytes(),
            )
        };
        let change = artifact("change");
        let policy = artifact("resolved_policy");
        let options = artifact("host_options");
        let identity = artifact("evaluator");
        let fresh = file("fresh-source", fixture["portable"].to_string().as_bytes());
        let shell = fs::canonicalize("/bin/sh").unwrap();
        let shell_hash: String = executable_digest(&shell)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut manifest = json!({"previous":previous,"base":base,"after":after,"change":change,
            "resolved_policy":policy,"host_options":options,"collection":null,"evaluator_identity":identity,
            "validator_name":"aexeo","validator_version":"0.0.24","required_checks":["gate:7:article:words"],
            "validator":[shell,"-c","/bin/cp \"$SOURCE\" \"$CONTENT_GATE_VERDICT_FILE\""],
            "validator_sha256":shell_hash,"env":{"SOURCE":fresh,"OPTIONS":options},
            "timeout_seconds":5,"additional":{"source":fresh}});
        let path = file("manifest", manifest.to_string().as_bytes());
        assert!(execute(&path, &mut BTreeMap::new()).is_ok());
        manifest["validator_sha256"] = json!("0".repeat(64));
        fs::write(&path, manifest.to_string()).unwrap();
        assert_eq!(
            execute(&path, &mut BTreeMap::new()).unwrap_err(),
            "validator digest differs"
        );
        manifest["validator_sha256"] = json!(shell_hash);
        manifest["validator"][2] = json!(
            "/bin/echo changed > \"$OPTIONS\"; /bin/cp \"$SOURCE\" \"$CONTENT_GATE_VERDICT_FILE\""
        );
        fs::write(&path, manifest.to_string()).unwrap();
        assert_eq!(
            execute(&path, &mut BTreeMap::new()).unwrap_err(),
            "inputs changed during validation"
        );
        fs::write(
            &options,
            fixture["artifacts"]["host_options"].as_str().unwrap(),
        )
        .unwrap();
        manifest["validator"][2] = json!("exit 1");
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(
            execute(&path, &mut BTreeMap::new())
                .unwrap_err()
                .contains("code 1")
        );
        manifest["validator"][2] = json!("/bin/sleep 2");
        manifest["timeout_seconds"] = json!(1);
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(
            execute(&path, &mut BTreeMap::new())
                .unwrap_err()
                .contains("timed out")
        );
    }
}
