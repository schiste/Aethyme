//! Host-facing execution of an Aexeo v1 check. Publication remains the host's responsibility.
mod portable_compare;

use aethyme_broker::content_gate::strict::{DecisionInputs, strict_precheck_v1};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    change: PathBuf,
    policy: PathBuf,
    options: PathBuf,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    collection: Option<PathBuf>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    external: Option<PathBuf>,
    additional: BTreeMap<String, PathBuf>,
    content: BTreeMap<String, PathBuf>,
    validator: Vec<String>,
    required_checks: Vec<String>,
    env: BTreeMap<String, String>,
    timeout_seconds: u64,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    if !fs::metadata(path)
        .map_err(|_| "cannot stat input")?
        .is_file()
    {
        return Err("input is not a regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "cannot open input")?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read input")?;
    if bytes.len() as u64 > LIMIT {
        return Err("input exceeds size limit".into());
    }
    Ok(bytes)
}
fn optional(path: &Option<PathBuf>) -> Result<Option<Vec<u8>>, String> {
    path.as_ref().map(|p| read(p)).transpose()
}
fn snapshot(manifest_path: &Path, m: &Manifest) -> Result<DecisionInputs, String> {
    let mut additional = BTreeMap::new();
    for (name, path) in &m.additional {
        if name.is_empty() || name.contains('\0') {
            return Err("empty additional input name".into());
        }
        additional.insert(name.clone(), read(path)?);
    }
    let executable = Path::new(&m.validator[0]);
    if !executable.is_absolute() {
        return Err("validator executable must be an absolute path".into());
    }
    if !fs::metadata(executable)
        .map_err(|_| "cannot stat validator executable")?
        .is_file()
    {
        return Err("validator executable is not a regular file".into());
    }
    let file = fs::File::open(executable).map_err(|_| "cannot open validator executable")?;
    let mut hasher = Sha256::new();
    let length = io::copy(&mut file.take(512 * 1024 * 1024 + 1), &mut hasher)
        .map_err(|_| "cannot hash validator executable")?;
    if length > 512 * 1024 * 1024 {
        return Err("validator executable exceeds size limit".into());
    }
    additional.insert(
        "\0aethyme.runner.executable".into(),
        hasher.finalize().to_vec(),
    );
    // The manifest itself binds executable, argument vector, file inventory and deadline.
    additional.insert("\0aethyme.runner.manifest".into(), read(manifest_path)?);
    Ok(DecisionInputs {
        change: read(&m.change)?,
        policy: read(&m.policy)?,
        options: read(&m.options)?,
        collection: optional(&m.collection)?,
        external: optional(&m.external)?,
        additional,
    })
}
fn content(m: &Manifest) -> Result<BTreeMap<String, Option<Vec<u8>>>, String> {
    m.content
        .iter()
        .map(|(id, path)| {
            let value = match fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_file() => Some(read(path)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                _ => return Err("candidate is not a regular file or could not be observed".into()),
            };
            Ok((id.clone(), value))
        })
        .collect()
}

fn execute(path: &Path) -> Result<(), String> {
    let manifest_bytes = read(path)?;
    let manifest_value = aethyme_broker::content_gate::strict::parse_unique(&manifest_bytes)
        .map_err(|_| "invalid runner manifest")?;
    let m: Manifest =
        serde_json::from_value(manifest_value).map_err(|_| "invalid runner manifest")?;
    if m.validator.is_empty()
        || m.validator[0].is_empty()
        || m.timeout_seconds == 0
        || m.timeout_seconds > 3600
    {
        return Err("invalid validator or timeout".into());
    }
    let required: std::collections::BTreeSet<_> = m.required_checks.iter().collect();
    if required.is_empty()
        || required.len() != m.required_checks.len()
        || required.iter().any(|id| id.trim().is_empty())
    {
        return Err("required_checks must contain distinct nonempty check IDs".into());
    }
    let before = snapshot(path, &m)?;
    // The manifest must not have changed between parsing and capture.
    if before.additional.get("\0aethyme.runner.manifest") != Some(&manifest_bytes) {
        return Err("runner manifest changed".into());
    }
    let temp = tempfile::tempdir().map_err(|_| "cannot create verdict directory")?;
    let verdict_path = temp.path().join("verdict.json");
    let change_path = fs::canonicalize(&m.change).map_err(|_| "cannot resolve change")?;
    // In a broker gate, inherit its supervised process group so the broker's
    // own deadline can terminate every validator descendant. Standalone runs
    // create an isolated group for their own timeout and liveness checks.
    let broker_mode = std::env::var("AETHYME_CONTENT_GATE_MODE").as_deref() == Ok("v1-uncached")
        && std::env::var_os("AETHYME_GATE_WORKER_ID").is_some();
    let mut command = Command::new(&m.validator[0]);
    command
        .env_clear()
        .envs(&m.env)
        .args(&m.validator[1..])
        .env("CONTENT_GATE_CHANGE_FILE", change_path)
        .env("CONTENT_GATE_VERDICT_FILE", &verdict_path)
        .stdin(Stdio::null());
    if !broker_mode {
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| "validator could not start")?;
    let deadline = Instant::now() + Duration::from_secs(m.timeout_seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) if Instant::now() <= deadline => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(Some(_)) => return Err("validator exceeded deadline".into()),
            _ => {
                if broker_mode {
                    let _ = child.kill();
                } else {
                    // SAFETY: kill(2) takes plain integers and touches no Rust memory;
                    // the negated pid targets the validator's own process group.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                let _ = child.wait();
                return Err("validator timed out or could not be observed".into());
            }
        }
    };
    // A surviving process group could still mutate the verdict. Never trust it.
    if !broker_mode
        // SAFETY: signal 0 only probes the validator's own process group and
        // touches no Rust memory.
        && (unsafe { libc::kill(-(child.id() as i32), 0) } == 0
            || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
    {
        return Err("validator descendants may still be active".into());
    }
    // Nonzero exits are always blocked; distinguish the v1 protocol's outcomes
    // without claiming that an unvalidated verdict is authentic or complete.
    match status.code() {
        Some(0) => {}
        Some(1) => {
            return Err("validator exited with code 1 (failed verdict or execution error)".into());
        }
        Some(2) => {
            return Err("validator exited with code 2 (not checked or invalid input)".into());
        }
        Some(code) => return Err(format!("validator exited with code {code}")),
        None => return Err("validator terminated without an exit code".into()),
    }
    let meta =
        fs::symlink_metadata(&verdict_path).map_err(|_| "validator did not produce verdict")?;
    if !meta.file_type().is_file() {
        return Err("verdict is not a regular file".into());
    }
    let verdict = read(&verdict_path)?;
    let after = snapshot(path, &m)?;
    let current = content(&m)?;
    strict_precheck_v1(&before, &after, &verdict, status.code(), false, &current)
        .map_err(str::to_owned)?;
    let parsed =
        aethyme_broker::content_gate::strict::parse_unique(&verdict).map_err(str::to_owned)?;
    let checks = parsed["checks"].as_array().ok_or("invalid checks")?;
    for id in required {
        let matching: Vec<_> = checks
            .iter()
            .filter(|c| c["id"].as_str() == Some(id))
            .collect();
        if matching.is_empty()
            || matching
                .iter()
                .any(|c| c["blocking"] != true || c["status"] != "passed")
        {
            return Err(format!("required blocking check not passed: {id}"));
        }
    }
    Ok(())
}
pub fn run(args: &[String]) -> u8 {
    if args.first().is_some_and(|arg| arg == "compare-v2") {
        if std::env::var_os("AETHYME_GATE_WORKER_ID").is_some() {
            eprintln!("offline comparison is forbidden in broker gates");
            return 2;
        }
        return portable_compare::run(&args[1..]);
    }
    if std::env::var_os("AETHYME_GATE_WORKER_ID").is_some()
        && std::env::var("AETHYME_CONTENT_GATE_MODE").as_deref() != Ok("v1-uncached")
    {
        eprintln!("content gate requires the native uncached gate type");
        return 2;
    }
    if args.len() != 2 || args[0] != "run" {
        eprintln!("usage: aethyme content-gate <run|compare-v2> MANIFEST.json");
        return 2;
    }
    match execute(Path::new(&args[1])) {
        Ok(()) => {
            eprintln!(
                "content gate precheck passed; host must revalidate atomically at publication"
            );
            0
        }
        Err(e) => {
            eprintln!("content gate blocked: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    fn fixture(command: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let digest = format!("{:x}", Sha256::digest(b"hello"));
        let change = json!({"schema":"content-gate.change/v1","change_id":"c","created_at":"2026-01-01T00:00:00Z","origin":{"kind":"manual"},"base":{"kind":"opaque","revision":"r"},"entries":[{"entity_id":"a","operation":"update","media_type":"text/plain","after":{"sha256":digest,"inline":"hello"}}]});
        let verdict = json!({"schema":"content-gate.verdict/v1","status":"pass","subject":{"change_id":"c","base":{"kind":"opaque","revision":"r"},"entries":[{"entity_id":"a","sha256":digest}]},"validator":{"name":"aexeo","version":"1"},"evaluated_at":"2026-01-01T00:00:00Z","summary":{"checks":1,"passed":1,"failed":0,"not_checked":0,"blocking_failed":0,"blocking_not_checked":0},"checks":[{"id":"x","status":"passed","blocking":true}],"findings":[]});
        fs::write(dir.join("change.json"), change.to_string()).unwrap();
        fs::write(dir.join("verdict.json"), verdict.to_string()).unwrap();
        fs::write(dir.join("policy"), "policy").unwrap();
        fs::write(dir.join("options"), "options").unwrap();
        fs::write(dir.join("article"), "hello").unwrap();
        let command = command
            .replace("{verdict}", dir.join("verdict.json").to_str().unwrap())
            .replace("{policy}", dir.join("policy").to_str().unwrap())
            .replace("{article}", dir.join("article").to_str().unwrap());
        let manifest = json!({"change":dir.join("change.json"),"policy":dir.join("policy"),"options":dir.join("options"),"collection":null,"external":null,"additional":{},"content":{"a":dir.join("article")},"validator":["/bin/sh","-c",command],"required_checks":["x"],"env":{"PATH":"/usr/bin:/bin"},"timeout_seconds":1});
        let path = dir.join("manifest.json");
        fs::write(&path, manifest.to_string()).unwrap();
        (root, path)
    }
    #[test]
    fn runs_validator_and_rejects_stale_missing_and_inconsistent_verdicts() {
        let (_d, p) = fixture("cp '{verdict}' \"$CONTENT_GATE_VERDICT_FILE\"");
        assert!(execute(&p).is_ok());
        let (_d, p) = fixture("exit 0");
        assert!(execute(&p).is_err());
        let (_d, p) = fixture("cp '{verdict}' \"$CONTENT_GATE_VERDICT_FILE\"; exit 1");
        assert!(execute(&p).is_err());
        let (_d, p) = fixture("printf '{' > \"$CONTENT_GATE_VERDICT_FILE\"");
        assert!(execute(&p).is_err());
        let (_d, p) =
            fixture("cp '{verdict}' \"$CONTENT_GATE_VERDICT_FILE\"; printf x >> '{policy}'");
        assert!(execute(&p).is_err());
    }
    #[test]
    fn candidate_changes_and_deletions_are_checked() {
        let (_d, p) =
            fixture("cp '{verdict}' \"$CONTENT_GATE_VERDICT_FILE\"; printf x >> '{article}'");
        assert!(execute(&p).is_err());
        let (d, p) = fixture("cp '{verdict}' \"$CONTENT_GATE_VERDICT_FILE\"");
        let change_path = d.path().join("change.json");
        let verdict_path = d.path().join("verdict.json");
        let mut change: serde_json::Value =
            serde_json::from_slice(&fs::read(&change_path).unwrap()).unwrap();
        change["entries"][0] =
            json!({"entity_id":"a", "operation":"delete", "media_type":"text/plain"});
        fs::write(change_path, change.to_string()).unwrap();
        let mut verdict: serde_json::Value =
            serde_json::from_slice(&fs::read(&verdict_path).unwrap()).unwrap();
        verdict["subject"]["entries"][0]["sha256"] = serde_json::Value::Null;
        fs::write(verdict_path, verdict.to_string()).unwrap();
        assert!(execute(&p).is_err());
        fs::remove_file(d.path().join("article")).unwrap();
        assert!(execute(&p).is_ok());
    }
    #[test]
    fn required_checks_cannot_be_omitted_or_downgraded() {
        let (d, p) = fixture("cp '{verdict}' \"$CONTENT_GATE_VERDICT_FILE\"");
        assert!(execute(&p).is_ok());
        let verdict_path = d.path().join("verdict.json");
        let original = fs::read(&verdict_path).unwrap();
        for (field, value) in [("id", json!("other")), ("blocking", json!(false))] {
            let mut verdict: serde_json::Value = serde_json::from_slice(&original).unwrap();
            verdict["checks"][0][field] = value;
            fs::write(&verdict_path, verdict.to_string()).unwrap();
            assert!(execute(&p).is_err(), "{field}");
        }
        fs::write(&verdict_path, original).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        for value in [json!([]), json!(["x", "x"]), json!([" "])] {
            manifest["required_checks"] = value;
            fs::write(&p, manifest.to_string()).unwrap();
            assert!(execute(&p).is_err());
        }
        manifest.as_object_mut().unwrap().remove("required_checks");
        fs::write(&p, manifest.to_string()).unwrap();
        assert!(execute(&p).is_err());
    }
    #[test]
    fn timeout_blocks() {
        let (_d, p) = fixture("sleep 3");
        assert!(execute(&p).is_err());
    }
}
