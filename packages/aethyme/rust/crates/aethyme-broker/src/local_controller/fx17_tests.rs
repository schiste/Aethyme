use crate::Broker;
use crate::broker::gate_trust::{self, GatePolicy};
use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_state::{CollaborationRoot, CollaborationStore, ProjectKey};
use crate::execution_snapshot::{
    CommandSpec, ExecutionSpec, LinuxProfile, MATERIALIZATION_PROFILE, ResourceLimits,
};
use crate::execution_supervisor::DockerExecutor;
use crate::execution_verifier::{AssertionCase, AssertionInventory, VerifierPolicy};
use crate::local_budget::{self as budget, ProjectPolicy, Units};
use crate::local_controller::DrivePolicy;
use crate::local_source_transport::GitCarrier;
use crate::local_target::{self as target, Phase, RequestedContribution, TargetSpec};
use crate::local_wave::{self as wave, WavePolicy};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const FX17_IMAGE: &str = "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251";
const FX17_MANIFEST_SHA: &str = "b3846f2fcca9bc8fb3bbcad26c0adedbc96fba723eddbbb174a9f3545ba11abc";
const FX17_CONTRACT_SHA: &str = "81d22e805fe6fa48424b1c83fe3bf54d1c9d0e76e5bb14d9cc8c20e84f3be13a";
const FX17_ORACLE_SHA: &str = "bfbec841810ac9269b34c628969a4c8207809b70716753d979750ecedb605a06";
const FX17_PROXY_SHA: &str = "eae502e3245a6cb750d64828063e793732cfa3a8481f8ff6753dcacad63ee2ff";
const FX17_TRANSCRIPT_SHA: &str =
    "db545a7b7e8f18a9dbeb6931dc327f99a6e8d074d039d08d60dfc57ff208d8bb";
const FX17_SOURCE_REVIEW_SHA: &str =
    "de7d56d9cc37f68787540e298bd49577c3fdf83b51544e3113aed4928eebb74e";
const FX17_PROVENANCE_SHA: &str =
    "6c6205982af3a7fc20d3e62134e4b30d9ad5ee7b3bb8f424a005a3296083829f";
const FIXTURE_GIT_PATH: &str = "/usr/bin:/bin";
const FIXTURE_GIT_NULL_DEVICE: &str = "/dev/null";

const CONTROLLED_STATES: [&str; 21] = [
    include_str!("../../../../../../aethyme-eval/fx17/controlled/00-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/01-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/02-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/03-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/04-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/05-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/06-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/07-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/08-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/09-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/10-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/11-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/12-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/13-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/14-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/15-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/16-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/17-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/18-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/19-app.sh"),
    include_str!("../../../../../../aethyme-eval/fx17/controlled/20-app.sh"),
];
const FX17_PROXY: &str = include_str!("../../../../../../aethyme-eval/fx17/proxy.sh");
const FX17_EXPECTED_TRANSCRIPT: &[u8] =
    include_bytes!("../../../../../../aethyme-eval/fx17/oracle/baseline65-proxy-transcript.txt");
const FX17_ORACLE: &[u8] =
    include_bytes!("../../../../../../aethyme-eval/fx17/oracle/baseline-cases.json");
const FX17_MANIFEST: &[u8] =
    include_bytes!("../../../../../../aethyme-eval/fx17/proposal-manifest.json");
const FX17_SOURCE_REVIEW: &[u8] =
    include_bytes!("../../../../../../aethyme-eval/fx17/oracle/source-review.json");
const FX17_PROVENANCE: &[u8] =
    include_bytes!("../../../../../../aethyme-eval/fx17/oracle/provenance.json");
const FX17_PUBLIC_CONTRACT: &[u8] =
    include_bytes!("../../../../../../aethyme-eval/fx17/public-contract.md");

const GATE_CONFIG: &str = "[[gate]]\nname = \"fx17-source-shape\"\ncommand = \"test -f app.sh && git diff --check HEAD^ HEAD\"\ntimeout_seconds = 10\ncache = false\n";

struct RemoveGateTrust(PathBuf);

impl Drop for RemoveGateTrust {
    fn drop(&mut self) {
        gate_trust::forget_record(&self.0);
    }
}

fn sha256(bytes: &[u8]) -> String {
    ObjectDigest::of(bytes).hex()
}

fn source_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn output_error(program: &str, args: &[String], output: &Output) -> String {
    format!(
        "{program} {} failed with {:?}: {}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn fixture_git_command(repo: &Path, args: &[String], index: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    command
        .env_clear()
        .current_dir(repo)
        .env("PATH", FIXTURE_GIT_PATH)
        .env("HOME", "")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", FIXTURE_GIT_NULL_DEVICE)
        .env("GIT_CONFIG_GLOBAL", FIXTURE_GIT_NULL_DEVICE)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "FX17 fixture")
        .env("GIT_AUTHOR_EMAIL", "fx17@example.invalid")
        .env("GIT_COMMITTER_NAME", "FX17 fixture")
        .env("GIT_COMMITTER_EMAIL", "fx17@example.invalid")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "commit.template=",
        ])
        .args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
}

fn git_output(
    repo: &Path,
    args: &[String],
    index: Option<&Path>,
    stdin: Option<&[u8]>,
) -> Result<Output, String> {
    let mut command = fixture_git_command(repo, args, index);
    let output = if let Some(bytes) = stdin {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("start git {}: {error}", args.join(" ")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "git stdin was not piped".to_string())?
            .write_all(bytes)
            .map_err(|error| format!("write git stdin: {error}"))?;
        child
            .wait_with_output()
            .map_err(|error| format!("wait for git {}: {error}", args.join(" ")))?
    } else {
        command
            .output()
            .map_err(|error| format!("run git {}: {error}", args.join(" ")))?
    };
    if !output.status.success() {
        return Err(output_error("git", args, &output));
    }
    Ok(output)
}

fn git_text(
    repo: &Path,
    args: &[String],
    index: Option<&Path>,
    stdin: Option<&[u8]>,
) -> Result<String, String> {
    let output = git_output(repo, args, index, stdin)?;
    String::from_utf8(output.stdout)
        .map(|text| text.trim_end_matches(['\r', '\n']).to_string())
        .map_err(|error| format!("git output is not UTF-8: {error}"))
}

fn git_static(repo: &Path, args: &[&str]) -> Result<String, String> {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    git_text(repo, &args, None, None)
}

fn update_index(
    repo: &Path,
    index: &Path,
    parent: &str,
    changes: &[(&str, &[u8])],
    message: &str,
) -> Result<String, String> {
    git_text(
        repo,
        &["read-tree".to_string(), parent.to_string()],
        Some(index),
        None,
    )?;
    for (path, bytes) in changes {
        let object = git_text(
            repo,
            &["hash-object".into(), "-w".into(), "--stdin".into()],
            None,
            Some(bytes),
        )?;
        let entry = format!("100644,{object},{path}");
        git_text(
            repo,
            &[
                "update-index".into(),
                "--add".into(),
                "--cacheinfo".into(),
                entry,
            ],
            Some(index),
            None,
        )?;
    }
    let tree = git_text(repo, &["write-tree".into()], Some(index), None)?;
    git_text(
        repo,
        &[
            "commit-tree".into(),
            tree,
            "-p".into(),
            parent.to_string(),
            "-m".into(),
            message.to_string(),
        ],
        None,
        None,
    )
}

fn fsync_report(path: &Path, report: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "report path has no parent".to_string())?;
    let temporary = path.with_extension("json.pending");
    let bytes = serde_json::to_vec_pretty(report).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("open report journal {}: {error}", temporary.display()))?;
    file.write_all(&bytes)
        .map_err(|error| format!("write report journal: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync report journal: {error}"))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("publish report journal {}: {error}", path.display()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync report directory {}: {error}", parent.display()))
}

fn save_event(path: &Path, report: &mut Value, event: Value) -> Result<(), String> {
    report["events"]
        .as_array_mut()
        .ok_or_else(|| "report events are not an array".to_string())?
        .push(event);
    fsync_report(path, report)
}

fn save_stage(path: &Path, report: &mut Value, index: usize, stage: Value) -> Result<(), String> {
    report["stages"]
        .as_array_mut()
        .ok_or_else(|| "report stages are not an array".to_string())?
        .get_mut(index)
        .ok_or_else(|| "report stage index disappeared".to_string())?
        .clone_from(&stage);
    fsync_report(path, report)
}

fn check_frozen_inputs() -> Result<(Vec<Value>, String), String> {
    if sha256(FX17_MANIFEST) != FX17_MANIFEST_SHA
        || sha256(FX17_PUBLIC_CONTRACT) != FX17_CONTRACT_SHA
        || sha256(FX17_ORACLE) != FX17_ORACLE_SHA
        || sha256(FX17_SOURCE_REVIEW) != FX17_SOURCE_REVIEW_SHA
        || sha256(FX17_PROVENANCE) != FX17_PROVENANCE_SHA
        || sha256(FX17_PROXY.as_bytes()) != FX17_PROXY_SHA
        || sha256(FX17_EXPECTED_TRANSCRIPT) != FX17_TRANSCRIPT_SHA
    {
        return Err("frozen FX17 file digest mismatch".into());
    }
    if FX17_PROXY.len() > 4096 {
        return Err("fixed trusted proxy exceeds the verifier command argument bound".into());
    }
    let manifest: Value =
        serde_json::from_slice(FX17_MANIFEST).map_err(|error| error.to_string())?;
    let source_review: Value =
        serde_json::from_slice(FX17_SOURCE_REVIEW).map_err(|error| error.to_string())?;
    if manifest["profile"] != "aethyme.fx17/declared-v0"
        || source_review["manifest_sha256"] != FX17_MANIFEST_SHA
        || source_review["findings"]
            .as_array()
            .is_none_or(|rows| !rows.is_empty())
        || source_review["distinct_controlled_source_states"] != 21
        || source_review["consecutive_actual_source_changes"] != 20
    {
        return Err("frozen FX17 manifest or independent review contract mismatch".into());
    }
    for (number, source) in CONTROLLED_STATES.iter().enumerate() {
        let name = format!("controlled/{number:02}-app.sh");
        if manifest["files"][&name] != sha256(source.as_bytes()) {
            return Err(format!("frozen FX17 source digest mismatch for {name}"));
        }
    }
    let oracle: Vec<Value> =
        serde_json::from_slice(FX17_ORACLE).map_err(|error| error.to_string())?;
    if oracle.len() != 65 {
        return Err(format!(
            "expected 65 independent vectors, found {}",
            oracle.len()
        ));
    }
    let mut expected = String::from("FX17V1\n");
    for case in &oracle {
        if case["stdin_hex"] != "" {
            return Err("proxy contract expects every frozen stdin vector to be empty".into());
        }
        let id = case["id"]
            .as_str()
            .ok_or_else(|| "oracle case has no id".to_string())?;
        let exit = case["exit_code"]
            .as_u64()
            .ok_or_else(|| format!("oracle case {id} has no exit code"))?;
        let stdout = case["stdout"]
            .as_str()
            .ok_or_else(|| format!("oracle case {id} has no stdout"))?;
        let stderr = case["stderr"]
            .as_str()
            .ok_or_else(|| format!("oracle case {id} has no stderr"))?;
        expected.push_str(&format!(
            "{id}\t{exit}\t{}\t{}\n",
            hex(stdout.as_bytes()),
            hex(stderr.as_bytes())
        ));
    }
    if expected.as_bytes() != FX17_EXPECTED_TRANSCRIPT {
        return Err(
            "independent proxy transcript is not the oracle-derived 65-row encoding".into(),
        );
    }
    Ok((oracle, expected))
}

fn reservation_rows(
    store: &CollaborationStore,
    wave: &str,
) -> Result<(Vec<Value>, Units, Units), String> {
    let ids = {
        let mut statement = store
            .read_connection()
            .prepare(
                "SELECT reservation FROM local_budget_reservations WHERE wave=?1 ORDER BY reservation",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map([wave], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    let mut rows = Vec::with_capacity(ids.len());
    let mut allocated = Units::default();
    let mut spent = Units::default();
    for key in ids {
        let id = aethyme_contracts::experimental_v0::RecordId::parse(&key)
            .map_err(|error| error.to_string())?;
        let reservation = budget::load(store, &id).map_err(|error| error.to_string())?;
        allocated = add_units(allocated, reservation.allocated)?;
        spent = add_units(spent, reservation.spent)?;
        rows.push(json!({
            "id": reservation.id.as_str(),
            "root": reservation.root.as_str(),
            "state": reservation.state,
            "allocated": reservation.allocated,
            "spent": reservation.spent,
            "attempts": reservation.attempts,
            "receipt": reservation.receipt.map(|receipt| receipt.to_string()),
            "observed_overrun": reservation.observed_overrun,
        }));
    }
    Ok((rows, allocated, spent))
}

fn add_units(left: Units, right: Units) -> Result<Units, String> {
    let add = |a: i64, b: i64| {
        a.checked_add(b)
            .ok_or_else(|| "budget usage aggregation overflow".to_string())
    };
    Ok(Units {
        time_ms: add(left.time_ms, right.time_ms)?,
        memory_bytes: add(left.memory_bytes, right.memory_bytes)?,
        disk_bytes: add(left.disk_bytes, right.disk_bytes)?,
        model_requests: add(left.model_requests, right.model_requests)?,
        input_tokens: add(left.input_tokens, right.input_tokens)?,
        output_tokens: add(left.output_tokens, right.output_tokens)?,
        spend_microunits: add(left.spend_microunits, right.spend_microunits)?,
        slots: add(left.slots, right.slots)?,
        previews: add(left.previews, right.previews)?,
    })
}

fn query_strings(
    store: &CollaborationStore,
    sql: &str,
    parameter: &str,
) -> Result<Vec<String>, String> {
    let mut statement = store
        .read_connection()
        .prepare(sql)
        .map_err(|error| error.to_string())?;
    statement
        .query_map([parameter], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn completion_roots(store: &CollaborationStore, completion: &str) -> Result<Vec<Value>, String> {
    let mut statement = store
        .read_connection()
        .prepare(
            "SELECT kind,reference FROM local_controller_completion_roots WHERE completion=?1 ORDER BY kind,reference",
        )
        .map_err(|error| error.to_string())?;
    statement
        .query_map([completion], |row| {
            Ok(json!({"kind":row.get::<_,String>(0)?,"reference":row.get::<_,String>(1)?}))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn read_commit_file(repo: &Path, commit: &str, path: &str) -> Result<Vec<u8>, String> {
    let spec = format!("{commit}:{path}");
    let output = git_output(repo, &["show".into(), spec], None, None)?;
    Ok(output.stdout)
}

fn create_state_commit(
    repo: &Path,
    index: &Path,
    parent: &str,
    ordinal: usize,
) -> Result<CommitOid, String> {
    let message = format!("FX17 controlled C{ordinal:02}");
    let commit = update_index(
        repo,
        index,
        parent,
        &[("app.sh", CONTROLLED_STATES[ordinal].as_bytes())],
        &message,
    )?;
    CommitOid::parse(&commit).map_err(|error| error.to_string())
}

fn record_unwind(report: &mut Value, payload: Box<dyn std::any::Any + Send>) -> String {
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_string())
        })
        .unwrap_or_else(|| "non-string panic payload".into());
    report["failures"]
        .as_array_mut()
        .expect("report failures are initialized")
        .push(json!({"error":message}));
    message
}

fn run_fx17(
    report: &mut Value,
    report_path: &Path,
    started: Instant,
    host: &Path,
    repo: &Path,
    harness_repo: &Path,
    remote: &Path,
) -> Result<(), String> {
    let (oracle, expected_transcript) = check_frozen_inputs()?;
    let report_digest = json!({
        "manifest_sha256": FX17_MANIFEST_SHA,
        "public_contract_sha256": FX17_CONTRACT_SHA,
        "oracle_sha256": FX17_ORACLE_SHA,
        "source_review_sha256": FX17_SOURCE_REVIEW_SHA,
        "provenance_sha256": FX17_PROVENANCE_SHA,
        "proxy_sha256": FX17_PROXY_SHA,
        "expected_transcript_sha256": FX17_TRANSCRIPT_SHA,
        "baseline_vector_count": oracle.len(),
        "controlled_state_count": CONTROLLED_STATES.len(),
        "actual_change_count": CONTROLLED_STATES.len() - 1,
    });
    report["frozen_inputs"] = report_digest;
    save_event(
        report_path,
        report,
        json!({"at":"preflight","result":"frozen hashes and independent transcript verified"}),
    )?;

    fs::create_dir_all(repo.join(".aethyme")).map_err(|error| error.to_string())?;
    fs::write(repo.join("app.sh"), CONTROLLED_STATES[0].as_bytes())
        .map_err(|error| error.to_string())?;
    fs::write(repo.join(".aethyme/gates.toml"), GATE_CONFIG).map_err(|error| error.to_string())?;
    git_static(repo, &["init", "-q", "-b", "main"])?;
    git_static(repo, &["add", "app.sh", ".aethyme/gates.toml"])?;
    git_static(repo, &["commit", "-qm", "FX17 frozen baseline state 00"])?;
    let baseline_commit = git_static(repo, &["rev-parse", "HEAD"])?;
    let initial_head = baseline_commit.clone();
    let initial_symbolic_head = git_static(repo, &["symbolic-ref", "-q", "HEAD"])?;
    let initial_refs = git_static(repo, &["show-ref", "--head"])?;
    let baseline_oid = CommitOid::parse(&baseline_commit).map_err(|error| error.to_string())?;

    let mut store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("fx17-controlled").map_err(|error| error.to_string())?,
        &[],
    )
    .map_err(|error| error.to_string())?;
    let baseline_snapshot = archive::retain_snapshot(&mut store, repo, &baseline_oid)
        .map_err(|error| error.to_string())?;

    fs::write(harness_repo.join("proxy.sh"), FX17_PROXY.as_bytes())
        .map_err(|error| error.to_string())?;
    let harness_inventory = AssertionInventory::new(vec![AssertionCase {
        id: "fx17-baseline65-proxy-transcript".into(),
        arguments: vec![],
        stdin_hex: String::new(),
        exit_code: 0,
        stdout: source_digest(expected_transcript.as_bytes()),
        stderr: source_digest(b""),
    }])
    .map_err(|error| error.to_string())?;
    fs::write(
        harness_repo.join("inventory.json"),
        harness_inventory.canonical_bytes(),
    )
    .map_err(|error| error.to_string())?;
    git_static(harness_repo, &["init", "-q", "-b", "main"])?;
    git_static(harness_repo, &["add", "proxy.sh", "inventory.json"])?;
    git_static(
        harness_repo,
        &["commit", "-qm", "FX17 trusted proxy and oracle inventory"],
    )?;
    let harness_commit = CommitOid::parse(&git_static(harness_repo, &["rev-parse", "HEAD"])?)
        .map_err(|error| error.to_string())?;
    let harness_snapshot = archive::retain_snapshot(&mut store, harness_repo, &harness_commit)
        .map_err(|error| error.to_string())?;
    report["retained_roots"] = json!({
        "initial_baseline_commit": baseline_commit,
        "initial_baseline_source": baseline_snapshot.snapshot_id.as_str(),
        "harness_snapshot": harness_snapshot.snapshot_id.as_str(),
        "harness_commit": harness_commit.as_str(),
        "collaboration_project": store.project_dir().display().to_string(),
        "source_repository": repo.display().to_string(),
        "bare_remote": remote.display().to_string(),
        "harness_repository": harness_repo.display().to_string(),
    });
    save_event(
        report_path,
        report,
        json!({"at":"fixture","result":"baseline, harness and independent oracle retained"}),
    )?;

    let cli = std::env::var("AETHYME_DOCKER_CLI")
        .map_err(|_| "AETHYME_DOCKER_CLI must name the appointed Docker client".to_string())?;
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").map_err(|_| {
        "AETHYME_DOCKER_TEST_IMAGE must name the appointed cached image".to_string()
    })?;
    if image != FX17_IMAGE {
        return Err(format!(
            "Docker image must equal the pinned FX17 image {FX17_IMAGE}"
        ));
    }
    let native = DockerExecutor::probe(PathBuf::from(&cli)).map_err(|error| error.to_string())?;
    let resources = crate::execution_resources::ResourcePolicy {
        host_state: host.join("execution-resources.db"),
        temporary_root: host.to_path_buf(),
        runtime: native.runtime().id(),
        daemon: native.runtime().daemon_id.clone(),
        meter_image: image.clone(),
        memory_mib: 2048,
        disk_mib: 2048,
        process_slots: 1024,
        job_slots: 2,
        host_floor_bytes: 1,
        daemon_floor_bytes: 1,
    };
    let executor = native
        .with_resources(resources)
        .map_err(|error| error.to_string())?;

    let proxy = FX17_PROXY.as_bytes().to_vec();
    let command = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), FX17_PROXY.into()],
        environment: Default::default(),
    };
    let limits = ResourceLimits {
        timeout_ms: 60_000,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        pids: 32,
    };
    let execution_template = ExecutionSpec {
        candidate_source: baseline_snapshot.snapshot_id.to_string(),
        preparation: None,
        dependencies: None,
        configuration: None,
        harness: harness_snapshot.snapshot_id.to_string(),
        dataset: None,
        assertion_inventory: harness_inventory.id().to_string(),
        materialization_profile: MATERIALIZATION_PROFILE.into(),
        profile: LinuxProfile {
            build_image: image.clone(),
            verifier_image: image.clone(),
            architecture: executor.runtime().architecture.clone(),
            runtime_profile: executor.runtime().id(),
            build_limits: limits,
            verifier_limits: limits,
        },
        build: CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "set -eu; cp /source/app.sh /outputs/app.sh".into(),
            ],
            environment: Default::default(),
        },
        verify: command.clone(),
    };
    let verifier_policy = VerifierPolicy {
        profile: crate::execution_verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness_snapshot.snapshot_id.to_string(),
        inventory: harness_inventory.id().to_string(),
        command: command.clone(),
        max_age_ms: 900_000,
    };
    let acceptance_policy = verifier_policy
        .record()
        .map_err(|error| error.to_string())?
        .id()
        .to_string();

    let gates = crate::gates::parse_gates(GATE_CONFIG).map_err(|error| error.to_string())?;
    if gates.len() != 1 {
        return Err("FX17 fixture must retain exactly one safe native gate".into());
    }
    let trusted_policy = GatePolicy::from_parts(&gates, None);
    let trust_path = gate_trust::record_trust(
        repo,
        &[trusted_policy.policy_sha256.as_str()],
        "FX17 fixed source-shape gate in disposable Playground",
    )
    .map_err(|error| error.to_string())?;
    let _remove_trust = RemoveGateTrust(trust_path.clone());

    let mut broker = Broker::open(repo)
        .map_err(|error| error.to_string())?
        .with_host_operation_database(host.join("operations.db"))
        .with_worktree_root(host.join("worktrees"));
    let session = broker
        .adopt(repo, Some("FX17 disposable native evolution Playground"))
        .map_err(|error| error.to_string())?;
    git_static(
        repo,
        &[
            "remote",
            "add",
            "fx17-accepted",
            remote
                .to_str()
                .ok_or_else(|| "bare remote path is not UTF-8".to_string())?,
        ],
    )?;
    git_static(
        repo,
        &[
            "init",
            "--bare",
            "-q",
            "--initial-branch=accepted",
            remote
                .to_str()
                .ok_or_else(|| "bare remote path is not UTF-8".to_string())?,
        ],
    )?;
    git_static(
        repo,
        &[
            "push",
            "-q",
            "fx17-accepted",
            &format!("{baseline_commit}:refs/heads/accepted"),
        ],
    )?;
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "fx17-accepted",
        "refs/heads/accepted",
        30,
        "Operator authorizes the exact disposable FX17 local bare Git target",
        true,
    )
    .map_err(|error| error.to_string())?;

    let owner = target::acquire_controller(
        &mut store,
        "fx17-controlled-evolution",
        "appointed-fx17-operator",
    )
    .map_err(|error| error.to_string())?;
    let objective_authorization = source_digest(b"FX17 finite 20-change local evolution objective");
    let requirements = vec![source_digest(FX17_PUBLIC_CONTRACT)];
    let cap = Units {
        time_ms: 1_000_000_000,
        memory_bytes: 64 * 1024 * 1024 * 1024,
        disk_bytes: 128 * 1024 * 1024 * 1024,
        slots: 200,
        ..Default::default()
    };
    let mut verifier_generation = None;
    let mut previous_accepted_commit = baseline_commit.clone();
    let mut previous_source = baseline_snapshot.snapshot_id.to_string();
    let mut accepted_parent_chain = Vec::with_capacity(20);
    let index = host.join("source.index");
    let cancelled = AtomicBool::new(false);

    report["execution_profile"] = json!({
        "runtime": executor.runtime(),
        "image": image,
        "docker_cli": cli,
        "build": execution_template.build,
        "verifier_command": command,
        "proxy_is_inline_byte_identical_to_archived_source": sha256(&proxy) == FX17_PROXY_SHA,
        "network": "none",
        "artifact_mount": "read-only and contains only app.sh",
        "private_tmp": "writable Docker tmpfs for joined stream capture",
        "candidate_source_host_execution": false,
        "native_gate": {
            "policy_sha256": trusted_policy.policy_sha256,
            "command": "test -f app.sh && git diff --check HEAD^ HEAD",
            "trusted_once": true
        },
        "service_allowance": {
            "appointments": 20,
            "max_ticks_per_appointment": 2,
            "expected_ticks_per_appointment": 1,
            "completions_per_appointment": 1,
            "max_elapsed_ms_per_appointment": 900000,
            "max_drive_wall_ms": 900000
        },
        "budget_cap": cap
    });
    save_event(
        report_path,
        report,
        json!({"at":"runtime-preflight","result":"pinned Docker runtime, private resources, verifier and safe native gate appointed"}),
    )?;

    for (ordinal, controlled_state) in CONTROLLED_STATES.iter().enumerate().skip(1) {
        let id = format!("C{ordinal:02}");
        let stage_started = Instant::now();
        let stage_index = report["stages"]
            .as_array()
            .ok_or_else(|| "report stages are not an array".to_string())?
            .len();
        let mut stage = json!({
            "ordinal": ordinal,
            "contribution": id,
            "status": "preparing",
            "baseline_commit": previous_accepted_commit,
            "baseline_source": previous_source,
            "declared_source_sha256": sha256(controlled_state.as_bytes()),
            "expected_baseline_vectors": 65,
        });
        report["stages"]
            .as_array_mut()
            .expect("report stages initialized")
            .push(stage.clone());
        fsync_report(report_path, report)?;

        let source_commit = create_state_commit(repo, &index, &previous_accepted_commit, ordinal)?;
        let changed = git_text(
            repo,
            &[
                "diff-tree".into(),
                "--no-commit-id".into(),
                "--name-only".into(),
                "-r".into(),
                previous_accepted_commit.clone(),
                source_commit.as_str().to_owned(),
            ],
            None,
            None,
        )?;
        let changed_paths = changed.lines().collect::<Vec<_>>();
        if changed_paths != ["app.sh"] {
            return Err(format!(
                "{id} must retain only app.sh from the previous actual accepted commit; got {changed_paths:?}"
            ));
        }
        let retained = archive::retain_contribution(
            &mut store,
            repo,
            &CommitOid::parse(&previous_accepted_commit).map_err(|error| error.to_string())?,
            &source_commit,
        )
        .map_err(|error| error.to_string())?;
        if retained.base.snapshot_id.as_str() != previous_source {
            return Err(format!(
                "{id} retained delta base {} does not match prior accepted source {}",
                retained.base.snapshot_id, previous_source
            ));
        }
        stage["source_commit"] = json!(source_commit.as_str());
        stage["contribution_lineage"] = json!(retained.lineage_record_id.as_str());
        stage["retained_base_snapshot"] = json!(retained.base.snapshot_id.as_str());
        stage["retained_result_snapshot"] = json!(retained.result.snapshot_id.as_str());
        stage["changed_paths"] = json!(changed_paths);
        stage["status"] = json!("delta-retained");
        save_stage(report_path, report, stage_index, stage.clone())?;

        let selected = TargetSpec {
            objective_authorization: objective_authorization.clone(),
            objective: "Apply exactly C01 through C20 as successive accepted source changes".into(),
            baseline: previous_source.clone(),
            contributions: vec![RequestedContribution {
                id: id.clone(),
                lineage: retained.lineage_record_id.to_string(),
                requires: vec![],
                atomic_group: None,
                revision_of: None,
                derived_from: vec![],
            }],
            selected: vec![id.clone()],
            requirements: requirements.clone(),
            acceptance_policy: acceptance_policy.clone(),
            composer_profile: crate::composer::PROVISIONAL_TEXT_PROFILE.into(),
        };
        let revision = target::propose_revision(&mut store, &owner, selected)
            .map_err(|error| error.to_string())?;
        target::select_revision(&mut store, &owner, &revision.id)
            .map_err(|error| error.to_string())?;
        let pinned =
            target::load_revision(&store, &revision.id).map_err(|error| error.to_string())?;
        if pinned.spec.baseline != previous_source
            || pinned.spec.selected.len() != 1
            || pinned.spec.selected[0] != id
            || pinned.spec.contributions.len() != 1
            || pinned.spec.contributions[0].lineage != retained.lineage_record_id.as_str()
        {
            return Err(format!(
                "{id} target revision widened its baseline or selected scope"
            ));
        }
        if ordinal == 1 {
            budget::configure_project(
                &mut store,
                &owner,
                ProjectPolicy {
                    authorization: source_digest(b"FX17 finite local budget authorization"),
                    price_profile: source_digest(b"FX17 declared unpriced local Docker profile"),
                    cap,
                    recovery_headroom: Units::default(),
                    mandatory_headroom: Units::default(),
                    max_root_reservations: 128,
                },
            )
            .map_err(|error| error.to_string())?;
            budget::authorize_objective(&mut store, &owner, cap)
                .map_err(|error| error.to_string())?;
        }
        stage["target_revision"] = json!(revision.id.as_str());
        stage["selected"] = json!(pinned.spec.selected);
        stage["revision_baseline"] = json!(pinned.spec.baseline);
        stage["status"] = json!("revision-selected");
        save_stage(report_path, report, stage_index, stage.clone())?;

        let permit = match verifier_generation {
            None => {
                let permit = crate::execution_verifier::configure(
                    &mut store,
                    &owner,
                    "fx17-native",
                    None,
                    verifier_policy.clone(),
                )
                .map_err(|error| error.to_string())?;
                verifier_generation = Some(permit.generation());
                permit
            }
            Some(generation) => crate::execution_verifier::existing_permit(
                &store,
                &owner,
                "fx17-native",
                generation,
            )
            .map_err(|error| error.to_string())?,
        };
        if Some(permit.generation()) != verifier_generation {
            return Err(format!(
                "{id} verifier generation changed across immutable revisions"
            ));
        }
        stage["verifier_generation"] = json!(permit.generation());
        stage["status"] = json!("verifier-authority-bound");
        save_stage(report_path, report, stage_index, stage.clone())?;

        wave::available(&mut store, &owner, &id, ordinal as i64)
            .map_err(|error| error.to_string())?;
        let drive_policy = DrivePolicy {
            execution: execution_template.clone(),
            wave_policy: WavePolicy {
                batch_width: 1,
                max_group: 1,
                debounce_ms: 0,
                age_promotion_ms: 0,
                drift_rechecks: 1,
            },
            max_wall_ms: 900_000,
            allow_empty_native_gates: false,
        };
        let appointment = crate::local_controller::ServicePolicy {
            nonce: format!("fx17-stage-{ordinal:02}"),
            drive: drive_policy,
            max_ticks: 2,
            max_completions: 1,
            poll_ms: 20,
            max_elapsed_ms: 900_000,
        };
        stage["status"] = json!("appointment-dispatched");
        stage["service_policy"] =
            serde_json::to_value(&appointment).map_err(|error| error.to_string())?;
        save_stage(report_path, report, stage_index, stage.clone())?;

        let service = crate::local_controller::serve(
            &mut store,
            &owner,
            &mut broker,
            &carrier,
            &executor,
            &permit,
            &appointment,
            &cancelled,
        )
        .map_err(|error| error.to_string())?;
        stage["service_report"] =
            serde_json::to_value(&service).map_err(|error| error.to_string())?;
        stage["status"] = json!("service-returned");
        save_stage(report_path, report, stage_index, stage.clone())?;
        if service.outcome != crate::local_controller::ServiceOutcome::CompletionAllowance
            || service.completions.len() != 1
            || service.ticks != 1
        {
            return Err(format!(
                "{id} did not produce exactly one actual service completion: {:?}",
                service
            ));
        }
        let completion = service.completions[0].clone();
        let wave_id = aethyme_contracts::experimental_v0::RecordId::parse(&completion.wave)
            .map_err(|error| error.to_string())?;
        let active_wave = wave::load(&store, &wave_id).map_err(|error| error.to_string())?;
        if active_wave.phase != crate::local_wave::WavePhase::Accepted
            || active_wave.spec.baseline != previous_source
            || active_wave.spec.selected.len() != 1
            || active_wave.spec.selected[0] != id
            || active_wave.spec.prior_completion.is_some()
            || active_wave.candidate_commit.as_ref().map(CommitOid::as_str)
                != Some(completion.candidate_commit.as_str())
        {
            return Err(format!(
                "{id} completion does not bind the exact single-change wave"
            ));
        }
        let expected_parent = previous_accepted_commit.clone();
        let actual_parent = git_text(
            repo,
            &[
                "rev-parse".into(),
                format!("{}^", completion.accepted_commit),
            ],
            None,
            None,
        )?;
        if actual_parent != expected_parent {
            return Err(format!(
                "{id} accepted commit parent {actual_parent} is not previous accepted commit {expected_parent}"
            ));
        }
        let accepted_diff = git_text(
            repo,
            &[
                "diff-tree".into(),
                "--no-commit-id".into(),
                "--name-only".into(),
                "-r".into(),
                expected_parent.clone(),
                completion.accepted_commit.clone(),
            ],
            None,
            None,
        )?;
        let accepted_paths = accepted_diff.lines().collect::<Vec<_>>();
        if accepted_paths.len() != 1 || accepted_paths[0] != "app.sh" {
            return Err(format!(
                "{id} accepted commit must retain only the selected app.sh delta; got {accepted_paths:?}"
            ));
        }
        stage["accepted_changed_paths"] = json!(accepted_paths);
        if completion.source != retained.result.snapshot_id.as_str() {
            return Err(format!(
                "{id} accepted source snapshot differs from the sole retained contribution result"
            ));
        }
        let remote_tip = git_text(
            remote,
            &["rev-parse".into(), "refs/heads/accepted".into()],
            None,
            None,
        )?;
        if remote_tip != completion.accepted_commit {
            return Err(format!(
                "{id} accepted completion does not match actual disposable bare remote tip"
            ));
        }
        if read_commit_file(repo, &completion.accepted_commit, "app.sh")?
            != controlled_state.as_bytes()
        {
            return Err(format!(
                "{id} accepted source is not the frozen controlled state"
            ));
        }
        let accepted_oid =
            CommitOid::parse(&completion.accepted_commit).map_err(|error| error.to_string())?;
        let accepted_snapshot =
            archive::snapshot_of_commit(repo, &accepted_oid).map_err(|error| error.to_string())?;
        if accepted_snapshot.id().as_str() != completion.source {
            return Err(format!(
                "{id} completion source does not identify accepted commit content"
            ));
        }
        let disposition =
            target::dispositions(&store, &revision.id).map_err(|error| error.to_string())?;
        if disposition.len() != 1
            || disposition[0].contribution != id
            || disposition[0].status != target::Disposition::Delivered
        {
            return Err(format!("{id} was not the sole delivered target obligation"));
        }

        let native_ids = query_strings(
            &store,
            "SELECT id FROM local_native_gate_jobs WHERE wave=?1",
            &completion.wave,
        )?;
        if native_ids.len() != 1 {
            return Err(format!("{id} expected exactly one fresh native gate job"));
        }
        let native_id = aethyme_contracts::experimental_v0::RecordId::parse(&native_ids[0])
            .map_err(|error| error.to_string())?;
        let native_job =
            crate::native_gates::load(&store, &native_id).map_err(|error| error.to_string())?;
        let native_observation = native_job
            .observation
            .clone()
            .ok_or_else(|| format!("{id} native gate observation is missing"))?;
        if native_job.state != "completed"
            || native_observation["status"] != "passed"
            || native_observation["current_authority"] != true
            || native_observation["configured_gates"] != 1
            || native_observation["selected_gates"] != 1
        {
            return Err(format!(
                "{id} native gate job did not pass the frozen safe gate"
            ));
        }

        let evidence_ids = query_strings(
            &store,
            "SELECT id FROM local_verification_evidence WHERE revision=?1 AND producer='fx17-native' ORDER BY issued_ms",
            revision.id.as_str(),
        )?;
        if evidence_ids.len() != 1 {
            return Err(format!(
                "{id} expected one actual verifier evidence receipt"
            ));
        }
        let acceptance_receipts = query_strings(
            &store,
            "SELECT id FROM local_acceptance_receipts WHERE id=?1",
            &completion.acceptance_receipt,
        )?;
        if acceptance_receipts.len() != 1 || acceptance_receipts[0] != completion.acceptance_receipt
        {
            return Err(format!(
                "{id} private accepted-completion receipt is not retained"
            ));
        }
        let roots = completion_roots(
            &store,
            &completion
                .record()
                .map_err(|error| error.to_string())?
                .id()
                .to_string(),
        )?;
        let (reservations, allocated, spent) = reservation_rows(&store, &completion.wave)?;
        if reservations.len() != 3 {
            return Err(format!(
                "{id} expected control, build and verifier reservation records"
            ));
        }
        stage["completion"] =
            serde_json::to_value(&completion).map_err(|error| error.to_string())?;
        stage["wave"] = json!(completion.wave);
        stage["native_gate_job"] = json!({
            "id": native_job.id.as_str(),
            "state": native_job.state,
            "observation": native_observation,
        });
        stage["verifier_evidence_ids"] = json!(evidence_ids);
        stage["acceptance_receipt_id"] = json!(completion.acceptance_receipt);
        stage["retention_roots"] = json!(roots);
        stage["budget"] = json!({
            "reservations": reservations,
            "allocated": allocated,
            "spent": spent,
            "accounting_limit": "declared conservative local reservation/receipt units; not invoice, provider charge or external quota"
        });
        stage["actual_elapsed_ms"] = json!(stage_started.elapsed().as_millis());
        stage["status"] = json!("accepted-and-verified");
        save_stage(report_path, report, stage_index, stage.clone())?;
        accepted_parent_chain.push(json!({
            "ordinal": ordinal,
            "contribution": id,
            "baseline_commit": expected_parent,
            "accepted_commit": completion.accepted_commit,
            "accepted_tree": completion.accepted_tree,
            "source_snapshot": completion.source,
            "completion_receipt": completion.acceptance_receipt,
            "revision": revision.id.as_str(),
            "wave": completion.wave,
            "verifier_generation": permit.generation(),
        }));
        report["accepted_parent_chain"] = json!(accepted_parent_chain);
        let stage_elapsed = stage_started.elapsed().as_millis();
        report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
        report["last_stage_elapsed_ms"] = json!(stage_elapsed);
        save_event(
            report_path,
            report,
            json!({"at":"completion","ordinal":ordinal,"completion":completion,"actual_elapsed_ms":stage_elapsed}),
        )?;

        previous_accepted_commit = completion.accepted_commit.clone();
        previous_source = completion.source.clone();
        if ordinal < 20 {
            let status =
                target::status(&store, owner.target()).map_err(|error| error.to_string())?;
            if status.phase != Phase::PublicationConfirmed {
                return Err(format!(
                    "{id} target did not reach private publication confirmation"
                ));
            }
            target::transition(&mut store, &owner, status.version, Phase::Idle)
                .map_err(|error| error.to_string())?;
            stage["status"] = json!("accepted-target-returned-to-idle");
            save_stage(report_path, report, stage_index, stage.clone())?;
        }
    }

    let final_head = git_static(repo, &["rev-parse", "HEAD"])?;
    let final_symbolic_head = git_static(repo, &["symbolic-ref", "-q", "HEAD"])?;
    let final_refs = git_static(repo, &["show-ref", "--head"])?;
    if final_head != initial_head || final_symbolic_head != initial_symbolic_head {
        return Err("the source repository checkout HEAD changed during the trial".into());
    }
    report["source_checkout_observation"] = json!({
        "initial_head": initial_head,
        "final_head": final_head,
        "initial_symbolic_head": initial_symbolic_head,
        "final_symbolic_head": final_symbolic_head,
        "initial_refs_sha256": sha256(initial_refs.as_bytes()),
        "final_refs_sha256": sha256(final_refs.as_bytes()),
        "refs_changed": initial_refs != final_refs,
        "head_unchanged": true
    });
    let count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE target='fx17-controlled-evolution'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if count != 20 {
        return Err(format!(
            "expected 20 durable private completions, found {count}"
        ));
    }
    let reservation_ids = {
        let mut statement = store
            .read_connection()
            .prepare("SELECT reservation FROM local_budget_reservations ORDER BY reservation")
            .map_err(|error| error.to_string())?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    let mut total_allocated = Units::default();
    let mut total_spent = Units::default();
    let mut retained_reservations = Vec::with_capacity(reservation_ids.len());
    for id in reservation_ids {
        let key = aethyme_contracts::experimental_v0::RecordId::parse(&id)
            .map_err(|error| error.to_string())?;
        let row = budget::load(&store, &key).map_err(|error| error.to_string())?;
        total_allocated = add_units(total_allocated, row.allocated)?;
        total_spent = add_units(total_spent, row.spent)?;
        retained_reservations.push(json!({
            "id": row.id.as_str(),
            "root": row.root.as_str(),
            "state": row.state,
            "allocated": row.allocated,
            "spent": row.spent,
            "attempts": row.attempts,
            "receipt": row.receipt.map(|receipt| receipt.to_string()),
            "observed_overrun": row.observed_overrun,
        }));
    }
    report["status"] = json!("completed");
    report["completion_count"] = json!(count);
    report["verifier_generation"] = json!(verifier_generation);
    report["accepted_parent_chain"] = json!(accepted_parent_chain);
    report["budget_accounting"] = json!({
        "reservation_count": retained_reservations.len(),
        "reservations": retained_reservations,
        "allocated": total_allocated,
        "spent": total_spent,
        "meaning": "declared conservative local reservation/receipt units; not invoice, provider charge or external quota"
    });
    report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
    report["effects"] = json!({
        "candidate_subject": "/bin/sh /artifact/app.sh <frozen argv>",
        "candidate_built_on_host": false,
        "docker_network": "none",
        "artifact_mount": "read-only, app.sh only",
        "writable_container_path": "private /tmp tmpfs for proxy stream capture",
        "effect_claim_limit": "This does not prove complete filesystem-effect absence. The 21 frozen candidate sources are covered by the independent static review, while this runtime checks the full 65-case behavior transcript."
    });
    save_event(
        report_path,
        report,
        json!({"at":"finished","result":"all 20 immutable single-completion revisions and 65-vector verifier transcripts passed"}),
    )?;
    Ok(())
}

#[test]
#[ignore = "actual 20-appointment FX17 Docker, native gate, verifier and disposable local bare-Git evolution"]
fn actual_fx17_twenty_controlled_changes_preserve_all_65_vectors() {
    let started = Instant::now();
    let host = tempfile::tempdir().expect("FX17 report root").keep();
    let repo = tempfile::tempdir()
        .expect("FX17 source repository root")
        .keep();
    let harness_repo = tempfile::tempdir()
        .expect("FX17 harness repository root")
        .keep();
    let remote = tempfile::tempdir()
        .expect("FX17 local bare remote root")
        .keep();
    let report_path = host.join("fx17-report.json");
    let mut report = json!({
        "profile": "aethyme.fx17-controlled-evolution-harness/v1",
        "status": "running",
        "started_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_millis(),
        "retained_roots": {
            "host_state": host.display().to_string(),
            "source_repository": repo.display().to_string(),
            "harness_repository": harness_repo.display().to_string(),
            "bare_remote": remote.display().to_string(),
            "report": report_path.display().to_string()
        },
        "events": [],
        "stages": [],
        "failures": [],
        "sealed_task_executions": 0,
        "model_exports": 0,
    });
    fsync_report(&report_path, &report).expect("write initial FX17 failure-retaining report");
    eprintln!(
        "FX17 actual trial report {}, source {}, host {}, remote {}, harness {}",
        report_path.display(),
        repo.display(),
        host.display(),
        remote.display(),
        harness_repo.display()
    );
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_fx17(
            &mut report,
            &report_path,
            started,
            &host,
            &repo,
            &harness_repo,
            &remote,
        )
    }));
    match outcome {
        Ok(Ok(())) => {
            fsync_report(&report_path, &report).expect("sync completed FX17 report");
        }
        Ok(Err(error)) => {
            report["status"] = json!("failed");
            report["failures"]
                .as_array_mut()
                .expect("failures array")
                .push(json!({"error":error}));
            report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
            fsync_report(&report_path, &report).expect("sync failed FX17 report");
            panic!(
                "FX17 actual evolution failed; retained report: {}",
                report_path.display()
            );
        }
        Err(payload) => {
            let message = record_unwind(&mut report, payload);
            report["status"] = json!("panicked");
            report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
            fsync_report(&report_path, &report).expect("sync panicked FX17 report");
            panic!(
                "FX17 actual evolution panicked ({message}); retained report: {}",
                report_path.display()
            );
        }
    }
}
