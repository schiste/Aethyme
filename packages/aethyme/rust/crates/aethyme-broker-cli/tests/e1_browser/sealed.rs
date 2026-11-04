//! One final campaign against a previously frozen independent variant.
use super::*;

const FREEZE: &str = "49f0debe22731accc39c61598ceb71e09bc91ba8f1f5f8b17b4903db1694050e";

fn sealed_root() -> PathBuf {
    fixtures().parent().unwrap().join("sealed")
}

fn verify_sealed() -> Value {
    let root = sealed_root();
    let bytes = std::fs::read(root.join("freeze.json")).unwrap();
    assert_eq!(digest(&bytes), FREEZE, "independent manifest changed");
    let freeze: Value = serde_json::from_slice(&bytes).unwrap();
    let mut expected = vec![root.join("freeze.json").to_str().unwrap().to_string()];
    for file in freeze["files"].as_array().unwrap() {
        let path = root.join(file["path"].as_str().unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, file["bytes"].as_u64().unwrap());
        assert_eq!(digest(&bytes), file["sha256"].as_str().unwrap());
        expected.push(path.to_str().unwrap().to_string());
    }
    let mut actual = Vec::new();
    inventory_directory(&root, &mut actual);
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected, "frozen file set changed");
    freeze
}

fn observe_sealed(file: &Path, root: &Path, label: &str, profile: &Value, runner: &Path) -> Value {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    appointed_environment(&mut command, root);
    command
        .args(["-p", "(version 1)(allow default)(deny network*)"])
        .arg(profile["node"].as_str().unwrap())
        .arg(runner)
        .arg(file)
        .arg(sealed_root().join("private/observer.mjs"))
        .arg(profile["playwright"].as_str().unwrap())
        .arg(profile["chromium"].as_str().unwrap());
    let mut result = external(&mut command, root, label, 45);
    if result["state"] == "completed" {
        let bytes = std::fs::read(result["stdout"].as_str().unwrap()).unwrap();
        result["observation"] = serde_json::from_slice(&bytes).unwrap_or(json!({"passed":false}));
        let observation = &result["observation"];
        let checks_valid = observation["checks"].as_array().is_some_and(|checks| {
            !checks.is_empty()
                && checks
                    .iter()
                    .all(|check| check["name"].is_string() && check["passed"] == true)
                && [
                    "caller has no runtime page errors",
                    "caller has no external requests",
                ]
                .iter()
                .all(|name| checks.iter().any(|check| check["name"] == *name))
        });
        if result["exit"] != 0
            || observation["schema"] != "aethyme.e1.sealed-observation/v1"
            || observation["browser_version"] != profile["versions"]["chromium"]
            || observation["passed"] != true
            || !checks_valid
        {
            result["observation"]["passed"] = json!(false);
        }
    }
    result
}

#[test]
fn sealed_manifest_is_still_independently_frozen() {
    verify_sealed();
}

#[test]
#[ignore = "appointed external structural engine and browser; one final campaign only"]
fn final_sealed_structural_campaign() {
    let root =
        PathBuf::from(std::env::var_os("AETHYME_E1_SEALED_EVIDENCE").expect("fresh evidence path"));
    std::fs::create_dir(&root).expect("never overwrite a campaign");
    let public = sealed_root().join("public");
    let case: Value =
        serde_json::from_slice(&std::fs::read(public.join("case.json")).unwrap()).unwrap();
    assert_eq!(case["schema"], "aethyme.e1.sealed-input/v1");
    assert_eq!(case["runs_per_order"], 2);
    let mut rows = Vec::new();
    for order in case["orders"].as_array().unwrap() {
        for repeat in 0..2 {
            rows.push(json!({"order":order,"repeat":repeat,"state":"unrun"}));
        }
    }
    let mut report = json!({"schema":"aethyme.e1.sealed-campaign/v1","state":"planned","case":case,"rows":rows,
        "engine_sha256":std::env::var("AETHYME_E1_ENGINE_SHA256").unwrap(),"freeze_sha256":FREEZE});
    persist(&root, &report);
    let admitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        report["freeze"] = verify_sealed();
        let profile = runtime_profile();
        verify_runtime(&profile);
        report["runtime_profile"] = profile.clone();
        let runner = aethyme_testkit::rust_workspace_root()
            .ancestors()
            .nth(3)
            .unwrap()
            .join("packages/aethyme-eval/e1/sealed-runner.mjs");
        report["runner_sha256"] = json!(digest(&std::fs::read(&runner).unwrap()));
        let engine = MergirafEngine::pin(
            &PathBuf::from(std::env::var_os("AETHYME_E1_ENGINE").unwrap()),
            &std::env::var("AETHYME_E1_ENGINE_SHA256").unwrap(),
        )
        .unwrap();
        let mut world = world_at(&case, &root, &public);
        report["retained_playground"] = json!(world.root);
        persist(&root, &report);
        for index in 0..report["rows"].as_array().unwrap().len() {
            let planned = report["rows"][index].clone();
            report["rows"][index]["state"] = json!("admitted");
            persist(&root, &report);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                world.request.deliveries =
                    serde_json::from_value(planned["order"].clone()).unwrap();
                let started = Instant::now();
                let result = composer::compose_structural(
                    &mut world.store,
                    &world.repo,
                    &world.request,
                    &engine,
                )
                .unwrap();
                let mut row = planned.clone();
                row["state"] = json!("completed");
                row["outcome"] = json!(result.composition.outcome.code());
                row["engine_observations"] = json!(result.observations);
                row["compose_wall_ms"] = json!(started.elapsed().as_millis());
                if let Some(candidate) = result.composition.outcome.candidate() {
                    let output = world.root.join(format!("sealed-candidate-{index}"));
                    collaboration_archive::reconstruct(&world.store, &candidate.subject, &output)
                        .unwrap();
                    let file = output.join(SOURCE_FILE);
                    row["candidate"] = json!({"snapshot":candidate.subject.as_str(),"tree":candidate.tree.as_str(),"commit":candidate.commit.as_str(),"sha256":digest(&std::fs::read(&file).unwrap())});
                    row["recipe"] = json!(result.composition.recipe.unwrap().id.as_str());
                    row["browser"] = observe_sealed(
                        &file,
                        &world.root,
                        &format!("sealed-observer-{index}"),
                        &profile,
                        &runner,
                    );
                }
                row
            }));
            report["rows"][index] = outcome.unwrap_or_else(|_| {
                let mut row = planned;
                row["state"] = json!("unknown_after_interruption");
                row
            });
            persist(&root, &report);
        }
        let mut retained = Vec::new();
        for member in case["contributions"].as_array().unwrap() {
            let alias = member["id"].as_str().unwrap();
            let lineage_id = world
                .request
                .catalog
                .iter()
                .find(|row| row.id == alias)
                .unwrap()
                .lineage
                .as_ref()
                .unwrap();
            let lineage = collaboration_archive::retained_contribution(&world.store, lineage_id)
                .unwrap()
                .unwrap();
            for (field, source) in [("base", &lineage.base), ("result", &lineage.result)] {
                let output = world.root.join(format!("retained-{alias}-{field}"));
                collaboration_archive::reconstruct(&world.store, &source.snapshot_id, &output)
                    .unwrap();
                let actual = std::fs::read(output.join(SOURCE_FILE)).unwrap();
                let expected = std::fs::read(public.join(member[field].as_str().unwrap())).unwrap();
                assert_eq!(actual, expected, "sealed original archive changed");
                retained.push(json!({"alias":alias,"role":field,"lineage":lineage_id.as_str(),"snapshot":source.snapshot_id.as_str(),"sha256":digest(&actual),"reconstructable":true}));
            }
        }
        report["retained_originals"] = json!(retained);
        persist(&root, &report);
    }));
    if admitted.is_err() {
        report["state"] = json!("admission_failed");
        persist(&root, &report);
        panic!("sealed admission failed; no retry is authorized by this campaign");
    }
    let rows = report["rows"].as_array().unwrap();
    let candidate_hashes: Vec<_> = rows
        .iter()
        .filter_map(|row| row["candidate"]["sha256"].as_str())
        .collect();
    let passed = rows.iter().all(|row| {
        row["state"] == "completed"
            && row["outcome"] == "candidate"
            && row["browser"]["observation"]["passed"] == true
    }) && candidate_hashes.len() == rows.len()
        && candidate_hashes.windows(2).all(|pair| pair[0] == pair[1]);
    report["state"] = json!("completed");
    report["passed"] = json!(passed);
    persist(&root, &report);
    // A failure remains valid evidence; this test requires a complete denominator.
    assert!(
        report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "completed")
    );
}
