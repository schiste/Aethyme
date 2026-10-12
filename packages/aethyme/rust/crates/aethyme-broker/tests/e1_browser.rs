//! Explicit E1 Playground experiments. No trials run in ordinary CI.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    std::fs::write(repo.join("index.html"), bytes).unwrap();
    git(repo, &["add", "index.html"]);
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

/// Files avoid pipe deadlocks; own child, time and file growth are bounded.
fn external(command: &mut Command, root: &Path, label: &str, seconds: u64) -> Value {
    let stdout = root.join(format!("{label}.stdout"));
    let stderr = root.join(format!("{label}.stderr"));
    command
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
        .stdin(Stdio::null());
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return json!({"state":"infrastructure_error","error":error.to_string()}),
    };
    loop {
        if [&stdout, &stderr]
            .iter()
            .any(|path| std::fs::metadata(path).unwrap().len() > 1024 * 1024)
            || started.elapsed() > Duration::from_secs(seconds)
        {
            child.kill().expect("kill only owned child");
            child.wait().expect("reap owned child");
            return json!({"state":"limit_exhausted","wall_ms":started.elapsed().as_millis(),"stdout":stdout,"stderr":stderr});
        }
        if let Some(status) = child.try_wait().unwrap() {
            return json!({"state":"completed","exit":status.code(),"wall_ms":started.elapsed().as_millis(),"stdout":stdout,"stderr":stderr});
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn observe(file: &Path, requirements: &Value, root: &Path, label: &str) -> Value {
    let workspace = aethyme_testkit::rust_workspace_root();
    let monorepo = workspace.ancestors().nth(3).unwrap();
    let observer = monorepo.join("packages/aethyme-eval/e1/browser-oracle.mjs");
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .args([
            "-p",
            "(version 1)(allow default)(deny network*)",
            "/opt/homebrew/bin/node",
        ])
        .arg(observer)
        .arg(file)
        .arg(serde_json::to_string(requirements).unwrap())
        .arg(std::env::var_os("AETHYME_E1_PLAYWRIGHT").expect("pinned Playwright module"))
        .arg(std::env::var_os("AETHYME_E1_CHROMIUM").expect("pinned Chromium executable"));
    let mut result = external(&mut command, root, label, 45);
    if result["state"] == "completed" {
        let bytes = std::fs::read(result["stdout"].as_str().unwrap()).unwrap();
        result["observation"] = serde_json::from_slice(&bytes)
            .unwrap_or(json!({"passed":false,"error":"invalid observer output"}));
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
                let file = output.join("index.html");
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
    persist(&root, &report);
}
