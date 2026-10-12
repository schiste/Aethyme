//! Explicit E1 Playground experiments. No trials run in ordinary CI.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aethyme_broker::collaboration_archive::{self, CommitOid};
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary, capture,
};
use aethyme_broker::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, forbidden_roots,
};
use aethyme_broker::composer::{
    self, CompositionBudget, CompositionRequest, ContributionSpec, structural::MergirafEngine,
};
use aethyme_contracts::experimental_v0::RecordId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SOURCE_FILE: &str = "page.html";

#[path = "e1_browser/model.rs"]
mod model;

fn fixtures() -> PathBuf {
    aethyme_testkit::rust_workspace_root()
        .join("crates/aethyme-testkit/fixtures/composition-e1/browser")
}

fn git(repo: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_string()
}

fn commit_file(repo: &Path, bytes: &[u8]) -> String {
    std::fs::write(repo.join(SOURCE_FILE), bytes).unwrap();
    git(repo, &["add", SOURCE_FILE]);
    git(repo, &["commit", "-qm", "fixture input"]);
    git(repo, &["rev-parse", "HEAD"])
}

fn captured(store: &mut CollaborationStore, repo: &Path, base: &str, result: &str) -> RecordId {
    let CaptureOutcome::Acknowledged(receipt) = capture(
        store,
        &CaptureRequest {
            operation_id: OperationId::mint().unwrap(),
            repository: repo.to_path_buf(),
            base: CommitOid::parse(base).unwrap(),
            result: CommitOid::parse(result).unwrap(),
            policy: CapturePolicy::Required,
            retention: RetentionBoundary::UntilReleased,
        },
    )
    .unwrap() else {
        panic!("actual Playground capture required");
    };
    receipt.contribution
}

struct World {
    root: PathBuf,
    repo: PathBuf,
    store: CollaborationStore,
    request: CompositionRequest,
}

fn world(case: &Value, evidence: &Path) -> World {
    let root = tempfile::Builder::new()
        .prefix("Playground-E1-browser-")
        .tempdir_in(evidence)
        .unwrap()
        .keep();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Playground"]);
    git(
        &repo,
        &["config", "user.email", "playground@example.invalid"],
    );
    let baseline = commit_file(&repo, &std::fs::read(fixtures().join("base.html")).unwrap());
    let mut commits = BTreeMap::from([("base.html".to_string(), baseline.clone())]);
    let mut store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(&root.join("state")),
        &ProjectKey::parse("e1-browser").unwrap(),
        &forbidden_roots(&repo),
    )
    .unwrap();
    let mut catalog = Vec::new();
    for contribution in case["contributions"].as_array().unwrap() {
        let base_name = contribution["base"].as_str().unwrap();
        let base = if let Some(commit) = commits.get(base_name) {
            commit.clone()
        } else {
            git(&repo, &["checkout", "-q", "--detach", &baseline]);
            let commit = commit_file(&repo, &std::fs::read(fixtures().join(base_name)).unwrap());
            commits.insert(base_name.into(), commit.clone());
            commit
        };
        git(&repo, &["checkout", "-q", "--detach", &base]);
        let result_name = contribution["result"].as_str().unwrap();
        let result = commit_file(&repo, &std::fs::read(fixtures().join(result_name)).unwrap());
        commits.insert(result_name.into(), result.clone());
        catalog.push(ContributionSpec {
            id: contribution["id"].as_str().unwrap().into(),
            lineage: Some(captured(&mut store, &repo, &base, &result)),
            requires: serde_json::from_value(contribution["requires"].clone()).unwrap(),
            atomic_group: contribution["atomic_group"].as_str().map(str::to_string),
            revision_of: None,
            derived_from: serde_json::from_value(contribution["derived_from"].clone()).unwrap(),
        });
    }
    git(&repo, &["checkout", "-q", "--detach", &baseline]);
    World {
        root,
        repo,
        store,
        request: CompositionRequest {
            baseline: CommitOid::parse(&baseline).unwrap(),
            accepted: Vec::new(),
            catalog,
            deliveries: Vec::new(),
            budget: CompositionBudget::default(),
        },
    }
}

fn persist(root: &Path, report: &Value) {
    let mut file = tempfile::NamedTempFile::new_in(root).unwrap();
    file.write_all(&serde_json::to_vec_pretty(report).unwrap())
        .unwrap();
    file.as_file().sync_all().unwrap();
    file.persist(root.join("report.json")).unwrap();
    #[cfg(unix)]
    std::fs::File::open(root).unwrap().sync_all().unwrap();
}

struct OwnedChild {
    child: Child,
    cleaned: bool,
}
impl OwnedChild {
    fn stop(&mut self) -> std::process::ExitStatus {
        assert!(!self.cleaned);
        #[cfg(unix)]
        // SAFETY: this unreaped child owns the process group created at spawn.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let status = self.child.wait().expect("reap owned experiment child");
        self.cleaned = true;
        status
    }
    fn exited(&mut self) -> bool {
        #[cfg(unix)]
        {
            // SAFETY: all-zero siginfo_t is a valid output buffer for waitid.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: the live child ID and writable output buffer remain valid;
            // WNOWAIT observes status without releasing the owned group identity.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            assert_eq!(
                result, 0,
                "observe own child without reaping its group identity"
            );
            info.si_signo != 0
        }
        #[cfg(not(unix))]
        {
            self.child.try_wait().unwrap().is_some()
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // The group was created only for this trial. Stop its descendants too,
        // including when setup/polling unwinds. Never match processes by name.
        if !self.cleaned {
            self.stop();
        }
    }
}

const LOG_LIMIT: usize = 1024 * 1024;

/// Bounded pipe readers retain at most limit+1 bytes. Descendants are confined
/// to an owned process group under this cooperative experiment profile.
fn external(command: &mut Command, root: &Path, label: &str, seconds: u64) -> Value {
    external_observed(command, root, label, seconds, None)
}

fn external_observed(
    command: &mut Command,
    root: &Path,
    label: &str,
    seconds: u64,
    model_tools: Option<u64>,
) -> Value {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let observed_tools = Arc::new(AtomicU64::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = root.join(format!("{label}.stdout"));
    let stderr = root.join(format!("{label}.stderr"));
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => OwnedChild {
            child,
            cleaned: false,
        },
        Err(error) => return json!({"state":"infrastructure_error","error":error.to_string()}),
    };
    let streams: Vec<Box<dyn Read + Send>> = vec![
        Box::new(child.child.stdout.take().unwrap()),
        Box::new(child.child.stderr.take().unwrap()),
    ];
    let (sender, receiver) = std::sync::mpsc::channel();
    for (index, stream) in streams.into_iter().enumerate() {
        let sender = sender.clone();
        let observed_tools = Arc::clone(&observed_tools);
        let exceeded = Arc::clone(&exceeded);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = (|| -> std::io::Result<Vec<u8>> {
                let mut stream = stream.take((LOG_LIMIT + 1) as u64);
                let mut buffer = [0u8; 8192];
                let mut line = Vec::new();
                loop {
                    let count = stream.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(tool_limit) = model_tools.filter(|_| index == 0) {
                        for byte in &buffer[..count] {
                            if *byte == b'\n' {
                                if let Ok(event) = serde_json::from_slice::<Value>(&line)
                                    && event["type"] == "item.started"
                                    && event["item"]["type"] == "command_execution"
                                {
                                    let count = observed_tools.fetch_add(1, Ordering::SeqCst) + 1;
                                    if count > tool_limit {
                                        exceeded.store(true, Ordering::SeqCst);
                                    }
                                }
                                line.clear();
                            } else {
                                line.push(*byte);
                            }
                        }
                    }
                }
                Ok(bytes)
            })();
            let _ = sender.send((index, result));
        });
    }
    drop(sender);
    let mut logs: [Option<Vec<u8>>; 2] = [None, None];
    let mut state = "completed";
    loop {
        for (index, bytes) in receiver.try_iter() {
            match bytes {
                Ok(bytes) => logs[index] = Some(bytes),
                Err(_) => state = "infrastructure_error",
            }
        }
        if state != "completed"
            || exceeded.load(Ordering::SeqCst)
            || logs.iter().flatten().any(|bytes| bytes.len() > LOG_LIMIT)
            || started.elapsed() > Duration::from_secs(seconds)
        {
            if state == "completed" {
                state = "limit_exhausted";
            }
            break;
        }
        if child.exited() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = child.stop();
    let exit = status.code();
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    drop(child);
    for _ in 0..2 {
        if logs.iter().all(Option::is_some) {
            break;
        }
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok((index, Ok(bytes))) => logs[index] = Some(bytes),
            _ => {
                state = "infrastructure_error";
                break;
            }
        }
    }
    if logs.iter().flatten().any(|bytes| bytes.len() > LOG_LIMIT) {
        state = "limit_exhausted";
    }
    if exceeded.load(Ordering::SeqCst) {
        state = "observed_tool_allowance_exhausted";
    }
    for (path, bytes) in [&stdout, &stderr].into_iter().zip(logs) {
        let mut bytes = bytes.unwrap_or_default();
        bytes.truncate(LOG_LIMIT);
        std::fs::write(path, bytes).unwrap();
    }
    json!({"state":state,"exit":exit,"signal":signal,"wall_ms":started.elapsed().as_millis(),"stdout":stdout,"stderr":stderr,"observed_command_dispatches":observed_tools.load(Ordering::SeqCst),"observed_command_allowance":model_tools})
}

fn requirement_argument(requirements: &Value) -> String {
    requirements
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let value = value.as_str().unwrap();
            assert!(!value.contains(','));
            value
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn runtime_profile() -> Value {
    let workspace = aethyme_testkit::rust_workspace_root();
    let path = workspace
        .ancestors()
        .nth(3)
        .unwrap()
        .join("packages/aethyme-eval/e1/browser-runtime-profile.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn inventory_directory(path: &Path, paths: &mut Vec<String>) {
    assert!(paths.len() < 50_000, "runtime file-count budget");
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(
        !metadata.file_type().is_symlink(),
        "runtime inventory rejects symlinks"
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            inventory_directory(&entry.unwrap().path(), paths);
        }
    } else {
        assert!(metadata.is_file(), "regular runtime file required");
        paths.push(path.to_str().unwrap().to_string());
    }
}

fn verify_runtime(profile: &Value) {
    assert_eq!(profile["schema"], "aethyme.e1.appointed-browser-runtime/v1");
    let mut actual = Vec::new();
    for root in profile["roots"].as_array().unwrap() {
        inventory_directory(Path::new(root.as_str().unwrap()), &mut actual);
    }
    let files = profile["files"].as_array().unwrap();
    for field in ["node", "chromium", "playwright"] {
        assert!(
            files.iter().any(|file| file["path"] == profile[field]),
            "launched runtime file must occur in appointed inventory"
        );
    }
    actual.push(profile["node"].as_str().unwrap().to_string());
    let observer = files
        .iter()
        .find(|file| {
            file["path"]
                .as_str()
                .unwrap()
                .ends_with("/e1/browser-oracle.mjs")
        })
        .unwrap();
    actual.push(observer["path"].as_str().unwrap().to_string());
    actual.sort();
    assert_eq!(
        actual,
        files
            .iter()
            .map(|file| file["path"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        "complete runtime inventory differs"
    );
    for file in files {
        let expected_size = file["bytes"].as_u64().unwrap();
        assert!(
            expected_size <= 512 * 1024 * 1024,
            "runtime per-file budget"
        );
        let mut source = std::fs::File::open(file["path"].as_str().unwrap())
            .unwrap()
            .take(expected_size + 1);
        let mut hasher = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let count = source.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            bytes += count as u64;
        }
        assert_eq!(bytes, expected_size, "runtime size changed");
        assert_eq!(
            format!("{:x}", hasher.finalize()),
            file["sha256"].as_str().unwrap(),
            "runtime bytes changed"
        );
    }
}

fn required_check_names(requirements: &Value) -> Vec<&'static str> {
    let selected: Vec<&str> = requirements
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let mut checks = vec![
        "unique DOM identities",
        "no WebSocket egress",
        "no runtime page errors",
    ];
    for (requirement, check) in [
        ("label", "selected heading appears exactly once"),
        ("accessible", "query has selected accessible name"),
        ("moved", "unique panel moved to sidebar"),
        ("mobile", "mobile control focuses the same query"),
        ("keyboard", "Escape clears the query"),
        ("navigation", "existing navigation survives"),
        (
            "no_analytics",
            "subtraction has no analytics even with consent",
        ),
        ("consumer_footer", "actual retained consumer survives"),
    ] {
        if selected.contains(&requirement) {
            checks.push(check);
        }
    }
    if selected.contains(&"search") {
        checks.extend([
            "Enter preserves directory search",
            "pointer preserves directory search",
        ]);
    }
    if selected
        .iter()
        .any(|value| ["privacy", "consent", "no_analytics"].contains(value))
    {
        checks.push("consent is initially false");
    }
    if selected.contains(&"privacy") || selected.contains(&"consent") {
        checks.extend([
            "no telemetry without affirmative consent",
            "one anonymous event with consent",
            "consent transport is independent of query",
            "revoked consent stops later events",
        ]);
    } else {
        checks.push("ordinary compositions emit no telemetry");
    }
    checks
}

fn appointed_environment(command: &mut Command, root: &Path) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .current_dir(root);
}

fn observe(file: &Path, requirements: &Value, root: &Path, label: &str) -> Value {
    let workspace = aethyme_testkit::rust_workspace_root();
    let monorepo = workspace.ancestors().nth(3).unwrap();
    let observer = monorepo.join("packages/aethyme-eval/e1/browser-oracle.mjs");
    let profile = runtime_profile();
    assert!(
        profile["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"].as_str() == observer.to_str()),
        "actual observer must match the verified appointed path"
    );
    let mut command = Command::new("/usr/bin/sandbox-exec");
    appointed_environment(&mut command, root);
    command
        .args(["-p", "(version 1)(allow default)(deny network*)"])
        .arg(profile["node"].as_str().unwrap())
        .arg(observer)
        .arg(file)
        .arg(requirement_argument(requirements))
        .arg(profile["playwright"].as_str().unwrap())
        .arg(profile["chromium"].as_str().unwrap());
    let mut result = external(&mut command, root, label, 45);
    if result["state"] == "completed" {
        let mut bytes = Vec::new();
        std::fs::File::open(result["stdout"].as_str().unwrap())
            .unwrap()
            .take((LOG_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .unwrap();
        result["observation"] = serde_json::from_slice(&bytes)
            .unwrap_or(json!({"passed":false,"error":"invalid observer output"}));
        if result["exit"] != 0
            || result["observation"]["schema"] != "aethyme.e1.browser-observation/v1"
            || result["observation"]["requirements"] != *requirements
            || result["observation"]["browser_version"] != profile["versions"]["chromium"]
            || !required_check_names(requirements).iter().all(|name| {
                result["observation"]["checks"]
                    .as_array()
                    .is_some_and(|checks| checks.iter().any(|check| check["name"] == *name))
            })
        {
            result["observation"]["passed"] = json!(false);
            result["evidence_error"] =
                json!("observer completion/schema/selected requirements differ");
        }
    }
    result
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The unmanaged Git comparison applies exact public input triples. It grants
/// no lineage/dependency/synthesis authority and creates no archived recipe.
fn plain_git(case: &Value, order: &Value, root: &Path, index: usize) -> Value {
    let current = root.join(format!("plain-git-{index}.html"));
    std::fs::copy(fixtures().join("base.html"), &current).unwrap();
    let mut traces = Vec::new();
    for (step, id) in order.as_array().unwrap().iter().enumerate() {
        let input = case["contributions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|input| input["id"] == *id)
            .unwrap();
        let mut command = Command::new("git");
        command
            .args(["merge-file", "-p"])
            .arg(&current)
            .arg(fixtures().join(input["base"].as_str().unwrap()))
            .arg(fixtures().join(input["result"].as_str().unwrap()));
        let result = external(&mut command, root, &format!("plain-{index}-{step}"), 5);
        let success = result["state"] == "completed" && result["exit"] == 0;
        if success {
            std::fs::copy(result["stdout"].as_str().unwrap(), &current).unwrap();
        }
        traces.push(result);
        if !success {
            return json!({"outcome":"conflict_or_infrastructure","traces":traces});
        }
    }
    json!({"outcome":"candidate","candidate":{"file":current,"sha256":digest(&std::fs::read(&current).unwrap())},
        "browser":observe(&current,&case["requirements"],root,&format!("plain-observer-{index}")),"traces":traces})
}

#[test]
fn browser_schedule_preserves_every_declared_repeat() {
    let cases: Value =
        serde_json::from_slice(&std::fs::read(fixtures().join("cases.json")).unwrap()).unwrap();
    assert_eq!(cases["cases"].as_array().unwrap().len(), 10);
    assert_eq!(cases["cases"][0]["orders"].as_array().unwrap().len(), 6);
    assert_eq!(cases["cases"][2]["orders"].as_array().unwrap().len(), 7);
}

#[test]
#[ignore = "explicit browser Playground campaign with pinned engine and browser runtimes"]
fn compare_browser_inputs() {
    let root = PathBuf::from(
        std::env::var_os("AETHYME_E1_BROWSER_EVIDENCE").expect("fresh evidence path"),
    );
    std::fs::create_dir(&root).expect("never overwrite a prior campaign");
    let cases: Value =
        serde_json::from_slice(&std::fs::read(fixtures().join("cases.json")).unwrap()).unwrap();
    let mut rows = Vec::new();
    for profile in [
        "plain-git-unmanaged",
        "git-text-baseline",
        "mergiraf-html-candidate",
    ] {
        for case in cases["cases"].as_array().unwrap() {
            // Dynamic FX07 follows a genuinely qualified synthesized result.
            if !case["synthesis"].is_null() {
                continue;
            }
            for order in case["orders"].as_array().unwrap() {
                for repeat in 0..2 {
                    rows.push(json!({"profile":profile,"case":case["id"],"order":order,"repeat":repeat,"state":"unrun"}));
                }
            }
        }
    }
    let mut report =
        json!({"schema":"aethyme.e1.browser-campaign/v1","state":"planned","rows":rows});
    persist(&root, &report);
    let profile = runtime_profile();
    verify_runtime(&profile);
    report["runtime_profile"] = profile;
    persist(&root, &report);
    let engine = MergirafEngine::pin(
        &PathBuf::from(std::env::var_os("AETHYME_E1_ENGINE").unwrap()),
        &std::env::var("AETHYME_E1_ENGINE_SHA256").unwrap(),
    )
    .expect("pinned engine admission");
    let mut worlds = BTreeMap::new();
    for index in 0..report["rows"].as_array().unwrap().len() {
        let planned = report["rows"][index].clone();
        report["rows"][index]["state"] = json!("admitted");
        persist(&root, &report);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let case = cases["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["id"] == planned["case"])
                .unwrap();
            let key = format!("{}:{}", planned["profile"], planned["case"]);
            let world = worlds.entry(key).or_insert_with(|| world(case, &root));
            if planned["profile"] == "plain-git-unmanaged" {
                let mut row = planned.clone();
                let result = plain_git(case, &planned["order"], &world.root, index);
                for (key, value) in result.as_object().unwrap() {
                    row[key] = value.clone();
                }
                row["state"] = json!("completed");
                row["retained_playground"] = json!(world.root);
                return row;
            }
            world.request.deliveries = serde_json::from_value(planned["order"].clone()).unwrap();
            let started = Instant::now();
            let (composition, traces) = if planned["profile"] == "git-text-baseline" {
                (
                    composer::compose(&mut world.store, &world.repo, &world.request).unwrap(),
                    json!([]),
                )
            } else {
                let result = composer::compose_structural(
                    &mut world.store,
                    &world.repo,
                    &world.request,
                    &engine,
                )
                .unwrap();
                (result.composition, json!(result.observations))
            };
            let mut row = planned.clone();
            row["state"] = json!("completed");
            row["outcome"] = json!(composition.outcome.code());
            row["engine_observations"] = traces;
            row["compose_wall_ms"] = json!(started.elapsed().as_millis());
            row["retained_playground"] = json!(world.root);
            if let Some(candidate) = composition.outcome.candidate() {
                let output = world.root.join(format!("candidate-{index}"));
                collaboration_archive::reconstruct(&world.store, &candidate.subject, &output)
                    .unwrap();
                let file = output.join(SOURCE_FILE);
                row["candidate"] = json!({"file":file,"sha256":digest(&std::fs::read(&file).unwrap()),"snapshot":candidate.subject.as_str(),"commit":candidate.commit.as_str(),"tree":candidate.tree.as_str(),"mode":format!("{:?}",candidate.mode)});
                row["recipe"] = json!(composition.recipe.as_ref().unwrap().id.as_str());
                row["browser"] = observe(
                    &file,
                    &case["requirements"],
                    &world.root,
                    &format!("observer-{index}"),
                );
            }
            row
        }));
        report["rows"][index] = match outcome {
            Ok(row) => row,
            Err(_) => {
                let mut row = planned;
                row["state"] = json!("unknown_after_interruption");
                row["infrastructure_error"] = json!("setup or execution panic; no automatic retry");
                row
            }
        };
        persist(&root, &report);
    }
    report["state"] = json!("completed");
    // Private answer selection happens only after all composer observations.
    let expected: Value =
        serde_json::from_slice(&std::fs::read(fixtures().join("expectations.json")).unwrap())
            .unwrap();
    let mut verdicts = Vec::new();
    for profile in [
        "plain-git-unmanaged",
        "git-text-baseline",
        "mergiraf-html-candidate",
    ] {
        for expectation in expected["cases"].as_array().unwrap() {
            if expectation["outcome"] == "recompose_or_inseparable" {
                continue;
            }
            let runs: Vec<&Value> = report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["profile"] == profile && row["case"] == expectation["id"])
                .collect();
            let all_completed =
                !runs.is_empty() && runs.iter().all(|row| row["state"] == "completed");
            let hashes: std::collections::BTreeSet<&str> = runs
                .iter()
                .filter_map(|row| row["candidate"]["sha256"].as_str())
                .collect();
            let identical_bytes = hashes.len() == 1
                && runs
                    .iter()
                    .all(|row| row["candidate"]["sha256"].is_string());
            let accepted = all_completed
                && match expectation["outcome"].as_str().unwrap() {
                    "candidate" => {
                        identical_bytes
                            && runs
                                .iter()
                                .all(|row| row["browser"]["observation"]["passed"] == true)
                    }
                    "synthesis_required" => false, // Genuine bounded synthesis is a separate required phase.
                    outcome => runs
                        .iter()
                        .all(|row| row["outcome"] == outcome && row["candidate"].is_null()),
                };
            verdicts.push(json!({"profile":profile,"case":expectation["id"],"expected":expectation["outcome"],"all_completed":all_completed,"byte_identical":identical_bytes,"accepted":accepted,"runs":runs.len()}));
        }
    }
    report["verdicts"] = json!(verdicts);
    persist(&root, &report);
}

#[test]
fn requirement_selection_round_trips_cli_and_names_required_checks() {
    let requirements = json!(["search", "consent", "privacy", "navigation"]);
    let argument = requirement_argument(&requirements);
    assert_eq!(
        argument.split(',').collect::<Vec<_>>(),
        vec!["search", "consent", "privacy", "navigation"]
    );
    assert!(required_check_names(&requirements).contains(&"one anonymous event with consent"));
    assert!(required_check_names(&requirements).contains(&"Enter preserves directory search"));
}

#[test]
#[ignore = "explicit appointed Node runtime parser check; no browser trial"]
fn appointed_observer_cli_parser_round_trips_selected_requirements() {
    let requirements = json!(["search", "consent", "privacy", "navigation"]);
    let argument = requirement_argument(&requirements);
    let workspace = aethyme_testkit::rust_workspace_root();
    let observer = workspace
        .ancestors()
        .nth(3)
        .unwrap()
        .join("packages/aethyme-eval/e1/browser-oracle.mjs");
    let module_url = format!("file://{}", observer.display());
    let script = "const {parseRequirements}=await import(process.argv[1]); console.log(JSON.stringify(parseRequirements(process.argv[2]))); try { parseRequirements('[\"search\"]'); process.exitCode=3; } catch {}";
    let result = Command::new(runtime_profile()["node"].as_str().unwrap())
        .args(["--input-type=module", "-e", script, &module_url, &argument])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        requirements
    );
}

#[test]
#[cfg(unix)]
fn owned_process_group_stops_descendant_after_leader_exit_and_deadline() {
    for (label, script, seconds, expected) in [
        (
            "leader-exit",
            "sleep 60 & echo $! > descendant.pid",
            5,
            "completed",
        ),
        (
            "deadline",
            "sleep 60 & echo $! > descendant.pid; wait",
            1,
            "limit_exhausted",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.current_dir(root.path()).args(["-c", script]);
        let result = external(&mut command, root.path(), label, seconds);
        assert_eq!(result["state"], expected);
        assert!(result["exit"].is_number() || result["signal"].is_number());
        let pid = std::fs::read_to_string(root.path().join("descendant.pid")).unwrap();
        let pid: i32 = pid.trim().parse().unwrap();
        // A killed orphan may remain as a zombie until init reaps it. It must
        // not execute or retain an open pipe after the guardian returns.
        let status = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output()
            .unwrap();
        let state = String::from_utf8(status.stdout).unwrap();
        assert!(
            state.trim().is_empty() || state.trim().starts_with('Z'),
            "owned descendant still executes: {pid} {state}"
        );
    }
}

#[test]
#[cfg(unix)]
fn retained_child_logs_have_a_hard_byte_cap() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "yes bounded-output"]);
    let result = external(&mut command, root.path(), "flood", 5);
    assert_eq!(result["state"], "limit_exhausted");
    assert!(
        std::fs::metadata(result["stdout"].as_str().unwrap())
            .unwrap()
            .len()
            <= LOG_LIMIT as u64
    );
}

#[test]
#[ignore = "explicit appointed Node runtime environment check; no browser trial"]
fn appointed_node_cannot_run_an_ambient_preload() {
    let root = tempfile::tempdir().unwrap();
    let hook = root.path().join("preload.cjs");
    let marker = root.path().join("injected-marker");
    std::fs::write(
        &hook,
        format!(
            "require('node:fs').writeFileSync({},'injected');",
            serde_json::to_string(marker.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let mut command = Command::new(runtime_profile()["node"].as_str().unwrap());
    command
        .env("NODE_OPTIONS", format!("--require {}", hook.display()))
        .env("NODE_PATH", root.path())
        .env("DYLD_INSERT_LIBRARIES", &hook)
        .args(["-e", "console.log('appointed')"]);
    appointed_environment(&mut command, root.path());
    let outcome = external(&mut command, root.path(), "preload", 5);
    assert_eq!(outcome["state"], "completed");
    assert_eq!(outcome["exit"], 0);
    assert!(!marker.exists(), "ambient preload must not execute");
}
