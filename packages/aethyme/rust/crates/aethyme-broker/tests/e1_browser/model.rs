//! User-approved export of synthetic Playground inputs only. Private oracles,
//! Aethyme source, credentials and real project files are never prompt payload.
use super::*;
use aethyme_broker::resolution::{
    self, FailedCheck, Proposal, ProposedFile, RequirementRef, ResolutionInputs, ResolutionOutcome,
    ResolutionRequest, Resolver, ResolverIdentity, Response, SynthesizedCandidate,
};
use aethyme_contracts::experimental_v0::EntryKind;

const CODEX_SHA: &str = "74c6a6d263f40d65f99c27a815275f5b4ed0bcc2baecc396b4cf3255498bdfc0";
const CODEX: &str = "/Users/christophehenner/.local/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex";
const RESPONSE_LIMIT: usize = 256 * 1024;
const CODEX_HELPER_SHA: &str = "0960435a104cf926e18bb938b8f6f8fcdf7e27be0c8d88e1911bc22a1de7d3e6";
const CODEX_RUNTIME: &[(&str, &str)] = &[
    ("bin/codex-code-mode-host", CODEX_HELPER_SHA),
    (
        "codex-package.json",
        "d932d8aed7d2cd6aee5dedccad6c30ddfa8ca10bce945f19c5780ab21291eb29",
    ),
    (
        "codex-path/rg",
        "cdd3c7bcc328730ac6bfbc04d19c0cfa1d23c1a3a838a8dc45ea7d84fda11b15",
    ),
    (
        "codex-resources/zsh/bin/zsh",
        "78baefdeac8715fe8a99b60d112f45daa2feaeb5068cd7b116c75300f1402d50",
    ),
];

struct CodexResolver {
    root: PathBuf,
    executable: PathBuf,
    home: PathBuf,
    invocations: Vec<Value>,
}

// These permissions apply to model tools. The CLI parent authenticates through
// its normal host credentials; those paths are never tool-readable roots.
fn permission_arguments(root: &Path) -> Vec<String> {
    permission_arguments_for_runtime(root, None)
}

fn permission_arguments_for_runtime(root: &Path, runtime: Option<&Path>) -> Vec<String> {
    let path = serde_json::to_string(root.to_str().unwrap()).unwrap();
    let runtime = runtime
        .map(|path| {
            format!(
                ", {}=\"read\"",
                serde_json::to_string(path.to_str().unwrap()).unwrap()
            )
        })
        .unwrap_or_default();
    vec![
        "-c".into(),
        "default_permissions=\"e1-fixtures\"".into(),
        "-c".into(),
        format!(
            "permissions.e1-fixtures.filesystem={{\":minimal\"=\"read\", \"/private/tmp\"=\"deny\", \"/private/var/folders\"=\"deny\", \":slash_tmp\"=\"deny\", \":tmpdir\"=\"deny\", {path}=\"read\"{runtime}}}"
        ),
        "-c".into(),
        "permissions.e1-fixtures.network.enabled=false".into(),
        "-c".into(),
        "approval_policy=\"never\"".into(),
        "-c".into(),
        "allow_login_shell=false".into(),
        "-c".into(),
        "web_search=\"disabled\"".into(),
        "-c".into(),
        "project_doc_max_bytes=0".into(),
        "-c".into(),
        "apps._default.enabled=false".into(),
    ]
}

fn isolated_home(root: &Path) -> PathBuf {
    let home = root.join("runner-home");
    std::fs::create_dir(&home).unwrap();
    let codex = home.join(".codex");
    std::fs::create_dir(&codex).unwrap();
    // A local pointer preserves normal CLI authentication without copying or
    // reading credential bytes. The model command profile denies both this
    // private home and the original credential path. Global instructions and
    // skill catalogs are absent from this newly created home.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "/Users/christophehenner/.codex/auth.json",
        codex.join("auth.json"),
    )
    .unwrap();
    #[cfg(not(unix))]
    panic!("appointed model trials require the reviewed macOS profile");
    home
}

fn model_command(executable: &Path, home: &Path, public: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", home)
        .env("CODEX_HOME", home.join(".codex"))
        .current_dir(public);
    command
}

struct PinnedCodex {
    _root: tempfile::TempDir,
    executable: PathBuf,
}

impl PinnedCodex {
    fn appointed() -> Result<Self, String> {
        let pinned = Self::acquire(Path::new(CODEX), CODEX_SHA)?;
        let source_root = Path::new(CODEX).parent().unwrap().parent().unwrap();
        for (relative, expected) in CODEX_RUNTIME {
            let bytes = acquired_bytes(&source_root.join(relative), expected)?;
            let copied = pinned._root.path().join(relative);
            std::fs::create_dir_all(copied.parent().unwrap()).map_err(|e| e.to_string())?;
            write_pinned(&copied, &bytes)?;
        }
        let mut helper = Command::new(pinned._root.path().join("bin/codex-code-mode-host"));
        helper
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .current_dir(pinned._root.path())
            .arg("--help");
        let observed = external(&mut helper, pinned._root.path(), "helper-admission", 3);
        let help = std::fs::read(pinned._root.path().join("helper-admission.stdout"))
            .map_err(|e| e.to_string())?;
        if observed["state"] != "completed"
            || observed["exit"] != 0
            || !String::from_utf8_lossy(&help).contains("codex-code-mode-host")
        {
            return Err(format!("Codex tool helper admission failed: {observed}"));
        }
        Ok(pinned)
    }
    fn acquire(source: &Path, expected: &str) -> Result<Self, String> {
        let bytes = acquired_bytes(source, expected)?;
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let executable = root.path().join("bin/codex");
        std::fs::create_dir(executable.parent().unwrap()).map_err(|e| e.to_string())?;
        write_pinned(&executable, &bytes)?;
        let pinned = Self {
            _root: root,
            executable,
        };
        let mut command = Command::new(&pinned.executable);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .current_dir(pinned._root.path())
            .arg("--version");
        let observed = external(&mut command, pinned._root.path(), "version", 3);
        if observed["state"] != "completed"
            || observed["exit"] != 0
            || std::fs::read(pinned._root.path().join("version.stdout"))
                .map_err(|e| e.to_string())?
                != b"codex-cli 0.162.1\n"
        {
            return Err(format!("Codex version admission failed: {observed}"));
        }
        Ok(pinned)
    }
}

fn acquired_bytes(source: &Path, expected: &str) -> Result<Vec<u8>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(source).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Codex must be a regular file".into());
    }
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = file
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        if sender.send(result).is_err() {
            eprintln!("bounded Codex acquisition completed after its deadline");
        }
    });
    let bytes = receiver
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "Codex acquisition deadline")?
        .map_err(|e| e.to_string())?;
    if bytes.len() > 256 * 1024 * 1024 || digest(&bytes) != expected {
        return Err("Codex size or digest mismatch".into());
    }
    Ok(bytes)
}

fn write_pinned(executable: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(executable, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o500))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Resolver for CodexResolver {
    fn identity(&self) -> ResolverIdentity {
        ResolverIdentity {
            name: "codex:gpt-6-luna".into(),
            version: format!("0.162.1;medium;sha256={CODEX_SHA};helper={CODEX_HELPER_SHA}"),
        }
    }
    fn propose(
        &mut self,
        request: &ResolutionRequest,
        inputs: &ResolutionInputs<'_>,
    ) -> Result<Response, String> {
        let root = self
            .root
            .join(format!("model-inputs-{}", self.invocations.len()));
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        let mut snapshots = vec![
            request.baseline().snapshot_id.clone(),
            request.accumulator().snapshot_id.clone(),
        ];
        let mut relationships = Vec::new();
        for member in request.contents().iter().chain(request.members()) {
            snapshots.extend([member.base.clone(), member.result.clone()]);
            relationships.push(
                json!({"id":member.id,"base":member.base.as_str(),"result":member.result.as_str()}),
            );
        }
        snapshots.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        snapshots.dedup();
        let mut files = Vec::new();
        for snapshot in snapshots {
            for path in request.scope() {
                if let Some(file) = inputs.read(&snapshot, path).map_err(|e| e.to_string())? {
                    files.push(json!({"snapshot":snapshot.as_str(),"path":path,"utf8":String::from_utf8(file.bytes).map_err(|_|"non-text model input")?}));
                }
            }
        }
        let public = json!({"baseline":request.baseline().snapshot_id.as_str(),"accumulator":request.accumulator().snapshot_id.as_str(),"members":relationships,"scope":request.scope(),"files":files});
        let bytes = serde_json::to_vec_pretty(&public).map_err(|e| e.to_string())?;
        if bytes.len() > 512 * 1024 {
            return Err("public input byte budget".into());
        }
        std::fs::write(root.join("inputs.json"), bytes).map_err(|e| e.to_string())?;
        std::fs::copy(
            fixtures().join("requirements.md"),
            root.join("requirements.md"),
        )
        .map_err(|e| e.to_string())?;
        let schema = json!({"type":"object","properties":{
            "writes":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"utf8":{"type":"string"}},"required":["path","utf8"],"additionalProperties":false}},
            "decision":{"type":"string"}},"required":["writes","decision"],"additionalProperties":false});
        std::fs::write(
            root.join("response-schema.json"),
            serde_json::to_vec(&schema).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        let prompt = "Read inputs.json and requirements.md with tools. Propose complete UTF-8 files only within the declared scope that jointly preserve the selected input changes and meet the public requirements. There is no authorized preference to discard one input. Return the response schema; do not edit files or inspect other directories. If no acceptable change exists, return an empty writes list and explain the unresolved choice. No answer key or observer is available.";
        let mut command = model_command(&self.executable, &self.home, &root);
        command
            .args([
                "exec",
                "--ephemeral",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--color",
                "never",
                "--json",
                "--output-schema",
            ])
            .arg(root.join("response-schema.json"))
            .args(permission_arguments_for_runtime(
                &root,
                self.executable.parent().and_then(Path::parent),
            ))
            .arg("-o")
            .arg(root.join("response.json"))
            .args([
                "-m",
                "gpt-6-luna",
                "-c",
                "model_reasoning_effort=\"medium\"",
                "-C",
            ])
            .arg(&root)
            .arg(prompt);
        verify_model_arguments(&command, &root)?;
        let process = external(&mut command, &root, "codex", 150);
        self.invocations.push(json!({"process":process,"requested_model":"gpt-6-luna","reasoning":"medium","executable_sha256":CODEX_SHA,"runtime_files":CODEX_RUNTIME,"public_input_sha256":digest(&serde_json::to_vec(&public).unwrap()),"root":root}));
        if process["state"] != "completed" || process["exit"] != 0 {
            return Err(format!("bounded model process failed: {process}"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(root.join("response.json"))
            .map_err(|e| e.to_string())?
            .take((RESPONSE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > RESPONSE_LIMIT {
            return Err("model response byte limit".into());
        }
        let response: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let mut proposal = Proposal::default();
        for write in response["writes"].as_array().ok_or("missing writes")? {
            let path = write["path"].as_str().ok_or("missing path")?.to_string();
            let bytes = write["utf8"]
                .as_str()
                .ok_or("missing utf8")?
                .as_bytes()
                .to_vec();
            if proposal
                .writes
                .insert(
                    path,
                    Some(ProposedFile {
                        kind: EntryKind::Regular,
                        bytes,
                    }),
                )
                .is_some()
            {
                return Err("duplicate proposal path".into());
            }
        }
        if proposal.writes.is_empty() {
            return Ok(Response::Unresolved(
                response["decision"]
                    .as_str()
                    .unwrap_or("empty proposal")
                    .into(),
            ));
        }
        Ok(Response::Proposal(proposal))
    }
}

fn verify_model_arguments(command: &Command, root: &Path) -> Result<(), String> {
    let args: Vec<_> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for (flag, expected) in [
        ("--output-schema", root.join("response-schema.json")),
        ("-o", root.join("response.json")),
        ("-C", root.to_path_buf()),
    ] {
        let index = args
            .iter()
            .position(|arg| arg == flag)
            .ok_or("required runner argument missing")?;
        if args.get(index + 1).map(String::as_str) != expected.to_str() {
            return Err(format!("runner option {flag} lost its value"));
        }
    }
    if args
        .iter()
        .any(|arg| arg == "--sandbox" || arg == "--dangerously-bypass-approvals-and-sandbox")
    {
        return Err("runner would override the appointed permission profile".into());
    }
    Ok(())
}

fn dynamic_subtraction(
    world: &mut World,
    synthesis: &SynthesizedCandidate,
    requirements: &Value,
    report: &mut Value,
    evidence: &Path,
) {
    report["phases"][2]["state"] = json!("admitted");
    persist(evidence, report);
    let baseline = world.request.baseline.as_str().to_string();
    let s = synthesis.candidate.commit.as_str();
    let lineage = captured(&mut world.store, &world.repo, &baseline, s);
    world.request.catalog.push(ContributionSpec {
        id: "S".into(),
        lineage: Some(lineage),
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: vec!["c1".into(), "c2".into()],
    });
    git(&world.repo, &["checkout", "-q", "--detach", s]);
    let actual = std::fs::read_to_string(world.repo.join(SOURCE_FILE)).unwrap();
    assert_eq!(
        actual.matches("</body>").count(),
        1,
        "frozen consumer anchor"
    );
    let (prefix, suffix) = actual.split_once("</body>").unwrap();
    assert_eq!(suffix.trim(), "</html>");
    let consumer = format!(
        "{prefix}<footer id=\"consumer-note\">Consumer observes the consent-aware search revision.</footer>\n</body>{suffix}"
    );
    let commit = commit_file(&world.repo, consumer.as_bytes());
    let consumer_lineage = captured(&mut world.store, &world.repo, s, &commit);
    world.request.catalog.push(ContributionSpec {
        id: "consumer-footer".into(),
        lineage: Some(consumer_lineage.clone()),
        requires: vec!["S".into()],
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    });
    let consumer_capture =
        collaboration_archive::retained_contribution(&world.store, &consumer_lineage)
            .unwrap()
            .unwrap();
    assert_eq!(
        consumer_capture.base.snapshot_id,
        synthesis.candidate.subject
    );
    report["consumer"] = json!({"base":s,"result":commit,"base_snapshot":synthesis.candidate.subject.as_str(),"source_sha256":digest(actual.as_bytes()),"result_sha256":digest(consumer.as_bytes())});
    let engine = MergirafEngine::pin(
        &PathBuf::from(std::env::var_os("AETHYME_E1_ENGINE").unwrap()),
        &std::env::var("AETHYME_E1_ENGINE_SHA256").unwrap(),
    )
    .unwrap();
    let c2 = collaboration_archive::retained_contribution(
        &world.store,
        world.request.catalog[1].lineage.as_ref().unwrap(),
    )
    .unwrap()
    .unwrap();
    let mut checks = requirements.clone();
    checks
        .as_array_mut()
        .unwrap()
        .push(json!("consumer_footer"));
    let mut subjects = Vec::new();
    for (index, (order, repeat)) in [
        vec!["S".to_string(), "consumer-footer".to_string()],
        vec!["consumer-footer".to_string(), "S".to_string()],
    ]
    .into_iter()
    .flat_map(|order| (0..2).map(move |repeat| (order.clone(), repeat)))
    .enumerate()
    {
        report["phases"][2]["runs"][index] =
            json!({"order":order,"repeat":repeat,"state":"admitted"});
        persist(evidence, report);
        world.request.deliveries = order.clone();
        let composition =
            composer::compose_structural(&mut world.store, &world.repo, &world.request, &engine)
                .unwrap()
                .composition;
        let mut row = json!({"order":order,"repeat":repeat,"state":"completed","outcome":composition.outcome.code()});
        if let Some(candidate) = composition.outcome.candidate() {
            let output = world.root.join(format!("actual-consumer-{index}"));
            collaboration_archive::reconstruct(&world.store, &candidate.subject, &output).unwrap();
            subjects.push(candidate.subject.clone());
            row["candidate"] = json!(candidate.subject.as_str());
            row["browser"] = observe(
                &output.join(SOURCE_FILE),
                &checks,
                &world.root,
                &format!("actual-consumer-{index}"),
            );
        }
        let passed = row["browser"]["observation"]["passed"] == true;
        report["phases"][2]["runs"][index] = row;
        persist(evidence, report);
        if !passed {
            report["phases"][2]["state"] = json!("failed");
            report["state"] = json!("consumer_failed");
            persist(evidence, report);
            return;
        }
    }
    assert!(
        subjects.iter().all(|id| *id == subjects[0]),
        "all consumer orders/repeats have identical source bytes"
    );
    report["phases"][2]["state"] = json!("completed");
    persist(evidence, report);
    for (phase, keep) in [(3, Vec::new()), (4, vec!["consumer-footer".to_string()])] {
        report["phases"][phase]["state"] = json!("admitted");
        persist(evidence, report);
        let mut subjects = Vec::new();
        for repeat in 0..2 {
            report["phases"][phase]["runs"][repeat] = json!({"repeat":repeat,"state":"admitted"});
            persist(evidence, report);
            let subtraction = composer::Subtraction {
                from: "S".into(),
                remove: vec!["c1".into()],
                keep: keep.clone(),
            };
            let composition = composer::recompose_structural_without(
                &mut world.store,
                &world.repo,
                &world.request,
                &subtraction,
                &engine,
            )
            .unwrap()
            .composition;
            let mut row = json!({"repeat":repeat,"state":"completed","outcome":composition.outcome.code(),"order":composition.order,"candidate_present":composition.outcome.candidate().is_some(),"recipe_present":composition.recipe.is_some()});
            let passed = if phase == 3 {
                if let Some(candidate) = composition.outcome.candidate() {
                    let output = world
                        .root
                        .join(format!("subtraction-without-consumer-{repeat}"));
                    collaboration_archive::reconstruct(&world.store, &candidate.subject, &output)
                        .unwrap();
                    subjects.push(candidate.subject.clone());
                    row["candidate"] = json!({"snapshot":candidate.subject.as_str(),"commit":candidate.commit.as_str(),"sha256":digest(&std::fs::read(output.join(SOURCE_FILE)).unwrap())});
                    row["recipe"] = serde_json::from_slice(
                        &collaboration_archive::read_object(
                            &world.store,
                            &composition.recipe.as_ref().unwrap().object,
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    row["browser"] = observe(
                        &output.join(SOURCE_FILE),
                        &json!(["search", "navigation", "no_analytics"]),
                        &world.root,
                        &format!("subtraction-without-consumer-{repeat}"),
                    );
                    let exact_inputs = candidate.inputs.len() == 1
                        && candidate.inputs[0].base_snapshot.as_ref() == Some(&c2.base.snapshot_id)
                        && candidate.inputs[0].result_snapshot.as_ref()
                            == Some(&c2.result.snapshot_id);
                    row["exact_retained_c2_inputs"] = json!(exact_inputs);
                    row["browser"]["observation"]["passed"] == true
                        && exact_inputs
                        && candidate.subject != synthesis.candidate.subject
                        && composition.order == vec!["c2"]
                } else {
                    false
                }
            } else {
                composition.outcome.code() == "inseparable_selection"
                    && composition.outcome.candidate().is_none()
                    && composition.recipe.is_none()
            };
            row["accepted"] = json!(passed);
            report["phases"][phase]["runs"][repeat] = row;
            persist(evidence, report);
            if !passed {
                report["phases"][phase]["state"] = json!("failed");
                report["state"] = json!("subtraction_failed");
                persist(evidence, report);
                return;
            }
        }
        assert!(subjects.is_empty() || subjects.iter().all(|id| *id == subjects[0]));
        report["phases"][phase]["state"] = json!("completed");
        persist(evidence, report);
    }
    let mut retained = Vec::new();
    for spec in &world.request.catalog {
        let lineage = collaboration_archive::retained_contribution(
            &world.store,
            spec.lineage.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap();
        for (role, snapshot) in [("base", lineage.base), ("result", lineage.result)] {
            let output = world.root.join(format!("retained-{}-{role}", spec.id));
            collaboration_archive::reconstruct(&world.store, &snapshot.snapshot_id, &output)
                .unwrap();
            let hash = digest(&std::fs::read(output.join(SOURCE_FILE)).unwrap());
            let expected = match (spec.id.as_str(), role) {
                ("c1", "base") | ("c2", "base") | ("S", "base") => {
                    std::fs::read(fixtures().join("base.html")).unwrap()
                }
                ("c1", "result") => std::fs::read(fixtures().join("analytics.html")).unwrap(),
                ("c2", "result") => std::fs::read(fixtures().join("privacy.html")).unwrap(),
                ("S", "result") | ("consumer-footer", "base") => actual.as_bytes().to_vec(),
                ("consumer-footer", "result") => consumer.as_bytes().to_vec(),
                _ => panic!("undeclared retention input"),
            };
            assert_eq!(
                hash,
                digest(&expected),
                "retained original source bytes changed"
            );
            retained.push(json!({"id":spec.id,"role":role,"snapshot":snapshot.snapshot_id.as_str(),"sha256":hash}));
        }
    }
    report["retained_originals_after_subtraction"] = json!(retained);
    report["state"] = json!("completed");
    persist(evidence, report);
}
#[test]
#[ignore = "explicit user-approved synthetic model synthesis and retained dynamic subtraction"]
fn synthesize_and_subtract_actual_browser_candidate() {
    let root = PathBuf::from(
        std::env::var_os("AETHYME_E1_SYNTHESIS_EVIDENCE").expect("fresh evidence path"),
    );
    std::fs::create_dir(&root).expect("never overwrite prior trials");
    let mut report = json!({"schema":"aethyme.e1.actual-synthesis/v1","state":"planned","phases":[{"id":"independent-failed-check","state":"unrun"},{"id":"genuine-synthesis","state":"unrun"},{"id":"actual-consumer","state":"unrun"},{"id":"subtract-no-consumer","state":"unrun"},{"id":"subtract-kept-consumer","state":"unrun"}],"attempts":[{"index":0,"state":"unrun"},{"index":1,"state":"unrun"}]});
    persist(&root, &report);
    report["phases"][2]["runs"] = json!(
        [vec!["S", "consumer-footer"], vec!["consumer-footer", "S"]]
            .into_iter()
            .flat_map(|order| (0..2)
                .map(move |repeat| json!({"order":order,"repeat":repeat,"state":"unrun"})))
            .collect::<Vec<_>>()
    );
    for index in [3, 4] {
        report["phases"][index]["runs"] = json!(
            (0..2)
                .map(|repeat| json!({"repeat":repeat,"state":"unrun"}))
                .collect::<Vec<_>>()
        );
    }
    persist(&root, &report);
    let result = recover_report(&root, &mut report, |report| run_synthesis(&root, report));
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn recover_report(
    root: &Path,
    report: &mut Value,
    body: impl FnOnce(&mut Value),
) -> std::thread::Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(report)));
    if result.is_err() {
        report["state"] = json!("unknown_after_interruption");
        for field in ["phases", "attempts"] {
            for row in report[field].as_array_mut().unwrap() {
                if row["state"] == "admitted" {
                    row["state"] = json!("unknown_after_interruption");
                }
                if let Some(runs) = row["runs"].as_array_mut() {
                    for run in runs {
                        if run["state"] == "admitted" {
                            run["state"] = json!("unknown_after_interruption");
                        }
                    }
                }
            }
        }
        persist(root, report);
    }
    result
}

fn failed_semantic_witness(observation: &Value, requirements: &Value) -> bool {
    observation["state"] == "completed"
        && observation["exit"] == 1
        && observation["observation"]["passed"] == false
        && observation["observation"]["schema"] == "aethyme.e1.browser-observation/v1"
        && observation["observation"]["requirements"] == *requirements
        && observation["observation"]["checks"]
            .as_array()
            .is_some_and(|checks| {
                checks.iter().any(|check| check["passed"] == false)
                    && required_check_names(requirements)
                        .iter()
                        .all(|name| checks.iter().any(|check| check["name"] == *name))
            })
}

fn block_missing_candidate(report: &mut Value, candidate_present: bool) -> bool {
    if candidate_present {
        return false;
    }
    report["phases"][0]["state"] = json!("blocked_no_candidate");
    report["state"] = json!("blocked_no_candidate");
    true
}

fn run_synthesis(root: &Path, report: &mut Value) {
    let runtime = runtime_profile();
    verify_runtime(&runtime);
    let codex = PinnedCodex::appointed().unwrap();
    report["runtime_profile"] = runtime;
    persist(root, report);
    let cases: Value =
        serde_json::from_slice(&std::fs::read(fixtures().join("cases.json")).unwrap()).unwrap();
    let case = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "FX04-browser")
        .unwrap();
    report["phases"][0]["state"] = json!("admitted");
    persist(root, report);
    let mut world = world(case, root);
    report["retained_playground"] = json!(world.root);
    world.request.deliveries = vec!["c1".into(), "c2".into()];
    let engine = MergirafEngine::pin(
        &PathBuf::from(std::env::var_os("AETHYME_E1_ENGINE").unwrap()),
        &std::env::var("AETHYME_E1_ENGINE_SHA256").unwrap(),
    )
    .unwrap();
    let composition =
        composer::compose_structural(&mut world.store, &world.repo, &world.request, &engine)
            .unwrap()
            .composition;
    if block_missing_candidate(report, composition.outcome.candidate().is_some()) {
        persist(root, report);
        return;
    }
    let candidate = composition.outcome.candidate().unwrap();
    {
        let output = world.root.join("failed-check-input");
        collaboration_archive::reconstruct(&world.store, &candidate.subject, &output).unwrap();
        report["initial_observation"] = observe(
            &output.join(SOURCE_FILE),
            &case["requirements"],
            &world.root,
            "before-synthesis",
        );
        if report["initial_observation"]["observation"]["passed"] == true {
            report["state"] = json!("unexpected_no_failed_check");
            persist(root, report);
            return;
        }
        if !failed_semantic_witness(&report["initial_observation"], &case["requirements"]) {
            report["phases"][0]["state"] = json!("infrastructure_error");
            report["state"] = json!("initial_check_infrastructure_error");
            persist(root, report);
            return;
        }
    }
    report["phases"][0]["state"] = json!("completed");
    let checks = vec![FailedCheck {
        check: "public-browser-contract".into(),
    }];
    let request = resolution::request_resolution(
        &mut world.store,
        &world.repo,
        &world.request,
        &composition,
        &checks,
        &[RequirementRef {
            id: "public-browser-contract".into(),
            path: None,
        }],
        None,
    )
    .unwrap();
    report["request"] =
        json!({"id":request.record().id.as_str(),"decision":request.decision_key()});
    persist(root, report);
    let mut resolver = CodexResolver {
        root: world.root.clone(),
        executable: codex.executable.clone(),
        home: isolated_home(&world.root),
        invocations: Vec::new(),
    };
    let mut qualified = None;
    for attempt in 0..2 {
        report["phases"][1]["state"] = json!("admitted");
        report["attempts"][attempt] = json!({"index":attempt,"state":"admitted"});
        persist(root, report);
        let resolution = resolution::resolve(
            &mut world.store,
            &world.repo,
            request.record(),
            &mut resolver,
        )
        .unwrap();
        let mut row = json!({"index":attempt,"state":"completed","outcome":resolution.outcome.code(),"attempt":resolution.attempt,"allowance_used":resolution.allowance.used,"model":resolver.invocations.last()});
        if let ResolutionOutcome::Synthesized(synthesized) = resolution.outcome {
            let output = world.root.join(format!("synthesized-{attempt}"));
            collaboration_archive::reconstruct(
                &world.store,
                &synthesized.candidate.subject,
                &output,
            )
            .unwrap();
            row["candidate"] = json!({"snapshot":synthesized.candidate.subject.as_str(),"commit":synthesized.candidate.commit.as_str(),"tree":synthesized.candidate.tree,"group":synthesized.group_record.id.as_str(),"directory":output});
            row["browser"] = observe(
                &output.join(SOURCE_FILE),
                &case["requirements"],
                &world.root,
                &format!("after-synthesis-{attempt}"),
            );
            if row["browser"]["observation"]["passed"] == true {
                qualified = Some(synthesized);
            }
        }
        report["attempts"][attempt] = row;
        persist(root, report);
        if qualified.is_some() {
            break;
        }
    }
    report["phases"][1]["state"] = json!(if qualified.is_some() {
        "completed"
    } else {
        "failed"
    });
    report["state"] = json!(if qualified.is_some() {
        "synthesis_qualified_subtraction_pending"
    } else {
        "synthesis_failed"
    });
    persist(root, report);
    if let Some(synthesis) = qualified {
        dynamic_subtraction(&mut world, &synthesis, &case["requirements"], report, root);
    }
}

#[test]
fn interrupted_model_journal_preserves_completed_unknown_and_unrun_slots() {
    let root = tempfile::tempdir().unwrap();
    let mut report = json!({"phases":[{"state":"completed"},{"state":"admitted"},{"state":"unrun"}],"attempts":[{"index":0,"state":"admitted"},{"index":1,"state":"unrun"}]});
    persist(root.path(), &report);
    assert!(
        recover_report(root.path(), &mut report, |_| panic!(
            "controlled synthetic journal interruption"
        ))
        .is_err()
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(saved["phases"][0]["state"], "completed");
    assert_eq!(saved["phases"][1]["state"], "unknown_after_interruption");
    assert_eq!(saved["phases"][2]["state"], "unrun");
    assert_eq!(saved["attempts"][0]["state"], "unknown_after_interruption");
    assert_eq!(saved["attempts"][1]["state"], "unrun");
}

#[test]
fn noncandidate_and_invalid_witness_do_not_admit_model_attempts() {
    let mut report = json!({"phases":[{"state":"admitted"},{"state":"unrun"}],"attempts":[{"state":"unrun"},{"state":"unrun"}]});
    let mut calls = 0;
    if !block_missing_candidate(&mut report, false) {
        calls += 1;
    }
    assert_eq!(calls, 0);
    assert_eq!(report["phases"][0]["state"], "blocked_no_candidate");
    assert!(
        report["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "unrun")
    );
    let requirements = json!(["search"]);
    let checks: Vec<Value> = required_check_names(&requirements)
        .into_iter()
        .map(|name| json!({"name":name,"passed":false}))
        .collect();
    let valid = json!({"state":"completed","exit":1,"observation":{"passed":false,"schema":"aethyme.e1.browser-observation/v1","requirements":requirements,"checks":checks}});
    assert!(failed_semantic_witness(&valid, &requirements));
    for change in ["exit", "passed", "schema", "requirements", "checks"] {
        let mut invalid = valid.clone();
        match change {
            "exit" => invalid["exit"] = json!(2),
            "passed" => invalid["observation"]["passed"] = json!(true),
            "schema" => invalid["observation"]["schema"] = json!("wrong"),
            "requirements" => invalid["observation"]["requirements"] = json!([]),
            _ => invalid["observation"]["checks"] = json!([]),
        }
        if failed_semantic_witness(&invalid, &requirements) {
            calls += 1;
        }
    }
    assert_eq!(calls, 0);
}

#[test]
fn runner_options_keep_adjacent_values_when_permissions_are_added() {
    let root = Path::new("/private/tmp/synthetic-public");
    let mut command = Command::new("codex");
    command
        .arg("--output-schema")
        .arg(root.join("response-schema.json"))
        .args(permission_arguments(root))
        .arg("-o")
        .arg(root.join("response.json"))
        .arg("-C")
        .arg(root);
    assert!(verify_model_arguments(&command, root).is_ok());
    let mut broken = Command::new("codex");
    broken
        .arg("--output-schema")
        .args(permission_arguments(root))
        .arg(root.join("response-schema.json"))
        .arg("-o")
        .arg(root.join("response.json"))
        .arg("-C")
        .arg(root);
    assert!(verify_model_arguments(&broken, root).is_err());
}

#[cfg(unix)]
#[test]
fn pinned_codex_uses_admitted_bytes_after_source_replacement() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let bytes = b"#!/bin/sh\necho 'codex-cli 0.162.1'\n";
    std::fs::write(&source, bytes).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = PinnedCodex::acquire(&source, &digest(bytes)).unwrap();
    std::fs::write(&source, b"#!/bin/sh\necho substituted\n").unwrap();
    assert_eq!(std::fs::read(&pinned.executable).unwrap(), bytes);
    assert_eq!(
        Command::new(&pinned.executable).output().unwrap().stdout,
        b"codex-cli 0.162.1\n"
    );
    assert!(PinnedCodex::acquire(&source, &digest(bytes)).is_err());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(PinnedCodex::acquire(&link, &digest(bytes)).is_err());
}

#[test]
#[ignore = "appointed native Codex sandbox probe; opens protected files without reading data"]
fn appointed_model_permissions_deny_source_oracle_and_credentials() {
    let pinned = PinnedCodex::appointed().unwrap();
    let root = tempfile::tempdir().unwrap();
    let public = root.path().join("public");
    std::fs::create_dir(&public).unwrap();
    std::fs::write(public.join("inputs.json"), b"synthetic public input").unwrap();
    let mut paths = vec![public.join("inputs.json")];
    for name in [
        "AETHYME_E1_DENY_MAIN",
        "AETHYME_E1_DENY_SIBLING",
        "AETHYME_E1_DENY_ORACLE",
        "AETHYME_E1_DENY_EVIDENCE",
        "AETHYME_E1_DENY_AUTH",
    ] {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit protected probe path"));
        assert!(
            path.is_file(),
            "probe must not mistake missing paths for denied reads"
        );
        paths.push(path);
    }
    let literal = serde_json::to_string(&paths).unwrap();
    let script = format!(
        "paths={literal}; paths.each_with_index {{|path,i| begin; File.open(path,\"r\") {{}}; puts \"#{{i}}:allowed\"; rescue Errno::EPERM, Errno::EACCES; puts \"#{{i}}:denied\"; end }}"
    );
    let home = isolated_home(root.path());
    assert!(!home.join(".codex/AGENTS.md").exists());
    assert!(!home.join(".codex/config.toml").exists());
    assert!(!home.join(".codex/skills").exists());
    assert!(!home.join(".agents").exists());
    assert_eq!(std::fs::read_dir(home.join(".codex")).unwrap().count(), 1);
    let mut command = model_command(&pinned.executable, &home, &public);
    command
        .args(["sandbox", "-P", "e1-fixtures"])
        .args(permission_arguments_for_runtime(
            &public,
            Some(pinned._root.path()),
        ))
        .args(["--", "/usr/bin/ruby", "--disable-gems", "-e", &script]);
    let process = external(&mut command, root.path(), "permissions", 15);
    assert_eq!(process["state"], "completed");
    assert_eq!(
        process["exit"],
        0,
        "{}",
        String::from_utf8_lossy(&std::fs::read(root.path().join("permissions.stderr")).unwrap())
    );
    assert_eq!(
        std::fs::read(root.path().join("permissions.stdout")).unwrap(),
        b"0:allowed\n1:denied\n2:denied\n3:denied\n4:denied\n5:denied\n"
    );
    // Actual pinned shell and search executable must run under the SAME tool
    // profile. A successful system Ruby probe alone misses runtime denials.
    let shell = pinned._root.path().join("codex-resources/zsh/bin/zsh");
    let rg = pinned._root.path().join("codex-path/rg");
    let mut command = model_command(&pinned.executable, &home, &public);
    command
        .args(["sandbox", "-P", "e1-fixtures"])
        .args(permission_arguments_for_runtime(
            &public,
            Some(pinned._root.path()),
        ))
        .arg("--")
        .arg(&shell)
        .args([
            "-f",
            "-c",
            "exec \"$1\" --fixed-strings synthetic inputs.json",
            "probe",
        ])
        .arg(&rg);
    let process = external(&mut command, root.path(), "runtime-permissions", 15);
    assert_eq!(process["state"], "completed");
    assert_eq!(
        process["exit"],
        0,
        "{}",
        String::from_utf8_lossy(
            &std::fs::read(root.path().join("runtime-permissions.stderr")).unwrap()
        )
    );
    assert_eq!(
        std::fs::read(root.path().join("runtime-permissions.stdout")).unwrap(),
        b"synthetic public input\n"
    );
}
