//! E1 uses retained Playground inputs, public composer metadata and an
//! independent oracle. Original held-outs are deliberately not invoked here.
use std::path::{Path, PathBuf};

use aethyme_broker::collaboration_archive::{self, CommitOid};
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary, capture,
};
use aethyme_broker::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, forbidden_roots,
};
use aethyme_broker::composer::structural::MergirafEngine;
use aethyme_broker::composer::{
    self, Composition, CompositionBudget, CompositionRequest, ContributionSpec, Subtraction,
};
use aethyme_broker::composition::CompositionOutcome;
use aethyme_contracts::experimental_v0::RecordId;
use aethyme_testkit::composition_fixtures::{
    self as fx, CaseInput, Observed, Outcome, ScenarioInput,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn capture_lineage(
    store: &mut CollaborationStore,
    repo: &Path,
    base: &str,
    result: &str,
) -> RecordId {
    let CaptureOutcome::Acknowledged(receipt) = capture(
        store,
        &CaptureRequest {
            operation_id: OperationId::mint().unwrap(),
            repository: repo.to_path_buf(),
            base: CommitOid::parse(base).unwrap(),
            result: CommitOid::parse(result).unwrap(),
            policy: CapturePolicy::Advisory,
            retention: RetentionBoundary::UntilReleased,
        },
    )
    .unwrap() else {
        panic!("incomplete Playground capture");
    };
    receipt.contribution
}

struct World {
    root: tempfile::TempDir,
    repo: PathBuf,
    store: CollaborationStore,
    request: CompositionRequest,
}

fn world(case: &CaseInput, scenario: &ScenarioInput, evidence: &Path) -> World {
    let root = tempfile::Builder::new()
        .prefix("Playground-E1-")
        .tempdir_in(evidence)
        .unwrap();
    let materialized = case.materialize(&scenario.id, &root.path().join("repos"));
    let repo = materialized.repo;
    let mut store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(&root.path().join("state")),
        &ProjectKey::parse("e1").unwrap(),
        &forbidden_roots(&repo),
    )
    .unwrap();
    let catalog =
        case.contributions
            .iter()
            .map(|input| ContributionSpec {
                id: input.id.clone(),
                lineage: materialized.contributions.get(&input.id).map(|commits| {
                    capture_lineage(&mut store, &repo, &commits.base, &commits.result)
                }),
                requires: input.requires.clone(),
                atomic_group: input.atomic_group.clone(),
                revision_of: input.revision_of.clone(),
                derived_from: input.derived_from.clone(),
            })
            .collect();
    let request = CompositionRequest {
        baseline: CommitOid::parse(&materialized.baseline).unwrap(),
        accepted: materialized
            .accepted
            .iter()
            .map(|commit| CommitOid::parse(commit).unwrap())
            .collect(),
        catalog,
        deliveries: Vec::new(),
        budget: CompositionBudget::default(),
    };
    World {
        root,
        repo,
        store,
        request,
    }
}

fn run(
    world: &mut World,
    scenario: &ScenarioInput,
    order: &[String],
    engine: Option<&MergirafEngine>,
) -> Result<(Composition, Value), composer::ComposeError> {
    world.request.deliveries = order.to_vec();
    let subtraction = scenario.request.subtract.as_ref().map(|input| Subtraction {
        from: input.from.clone(),
        remove: input.remove.clone(),
        keep: input.keep.clone(),
    });
    match (engine, subtraction) {
        (Some(engine), Some(subtraction)) => {
            let result = composer::recompose_structural_without(
                &mut world.store,
                &world.repo,
                &world.request,
                &subtraction,
                engine,
            )?;
            Ok((result.composition, json!(result.observations)))
        }
        (Some(engine), None) => {
            let result = composer::compose_structural(
                &mut world.store,
                &world.repo,
                &world.request,
                engine,
            )?;
            Ok((result.composition, json!(result.observations)))
        }
        (None, Some(subtraction)) => Ok((
            composer::recompose_without(
                &mut world.store,
                &world.repo,
                &world.request,
                &subtraction,
            )?,
            json!([]),
        )),
        (None, None) => Ok((
            composer::compose(&mut world.store, &world.repo, &world.request)?,
            json!([]),
        )),
    }
}

fn oracle_outcome(code: &str) -> Outcome {
    match code {
        "conflict" => Outcome::Conflict,
        "unknown_base" => Outcome::UnknownBase,
        "dependency_cycle" => Outcome::DependencyCycle,
        "missing_input" => Outcome::MissingInput,
        "competing_revisions" => Outcome::CompetingRevisions,
        "budget_exhausted" => Outcome::BudgetExhausted,
        "inseparable_selection" => Outcome::InseparableSelection,
        _ => Outcome::Unsupported,
    }
}

fn judge(
    case: &CaseInput,
    scenario: &ScenarioInput,
    evidence: &Path,
    engine: Option<&MergirafEngine>,
) -> Value {
    let mut world = world(case, scenario, evidence);
    let orders = if scenario.orders.is_empty() {
        vec![Vec::new()]
    } else {
        scenario.orders.clone()
    };
    let mut seen: Vec<(Vec<String>, Result<PathBuf, Outcome>)> = Vec::new();
    let mut rows = Vec::new();
    let mut errors = 0;
    for order in orders {
        for repeat in 0..2 {
            let started = std::time::Instant::now();
            match run(&mut world, scenario, &order, engine) {
                Ok((composition, observations)) => {
                    assert_eq!(
                        composition.outcome.candidate().is_some(),
                        composition.recipe.is_some()
                    );
                    let mut row = json!({"order":order,"repeat":repeat,"outcome":composition.outcome.code(),"observations":observations,"wall_ms":started.elapsed().as_millis()});
                    let observed = if let CompositionOutcome::Candidate(candidate) =
                        &composition.outcome
                    {
                        let output = world.root.path().join(format!("candidate-{}", seen.len()));
                        collaboration_archive::reconstruct(
                            &world.store,
                            &candidate.subject,
                            &output,
                        )
                        .unwrap();
                        row["candidate"] = json!({"snapshot":candidate.subject.as_str(),"commit":candidate.commit.as_str(),"tree":candidate.tree.as_str(),"mode":format!("{:?}",candidate.mode),"directory":output});
                        row["recipe"] = json!(composition.recipe.as_ref().unwrap().id.as_str());
                        Ok(output)
                    } else {
                        Err(oracle_outcome(composition.outcome.code()))
                    };
                    seen.push((order.clone(), observed));
                    rows.push(row);
                }
                Err(error) => {
                    errors += 1;
                    rows.push(json!({"order":order,"repeat":repeat,"infrastructure_error":error.to_string(),"code":error.code(),"wall_ms":started.elapsed().as_millis()}));
                    seen.push((order.clone(), Err(Outcome::Unsupported)));
                }
            }
        }
    }
    let runs = seen
        .iter()
        .map(|(order, output)| {
            (
                order.clone(),
                match output {
                    Ok(path) => Observed::Candidate(path),
                    Err(outcome) => Observed::Refused(*outcome),
                },
            )
        })
        .collect::<Vec<_>>();
    let verdict = fx::judge_scenario(case, &scenario.id, &runs);
    let archive = world.root.keep();
    json!({"case":case.case,"scenario":scenario.id,"accepted":verdict.accepted&&errors==0,"oracle_failures":verdict.failures,"infrastructure_errors":errors,"runs":rows,"retained_playground":archive})
}

fn fixture_root() -> PathBuf {
    aethyme_testkit::rust_workspace_root().join("crates/aethyme-testkit/fixtures/composition-e1")
}

#[test]
fn original_e1_inputs_match_the_pretrial_inventory() {
    let workspace = aethyme_testkit::rust_workspace_root();
    let root = workspace.ancestors().nth(3).expect("monorepo root");
    let inventory: Value = serde_json::from_slice(
        &std::fs::read(fixture_root().join("frozen-input-inventory.json")).unwrap(),
    )
    .unwrap();
    for input in inventory["inputs"]
        .as_array()
        .expect("frozen file inventory")
    {
        let path = root.join(input["path"].as_str().unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes.len() as u64,
            input["bytes"].as_u64().unwrap(),
            "{}",
            path.display()
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            input["sha256"].as_str().unwrap(),
            "{}",
            path.display()
        );
    }
}

#[test]
#[ignore = "explicit E1 Playground campaign: pinned external engine and fresh evidence directory required"]
fn compare_original_inputs_with_pinned_structural_engine() {
    let executable = PathBuf::from(std::env::var_os("AETHYME_E1_ENGINE").expect("explicit engine"));
    let digest = std::env::var("AETHYME_E1_ENGINE_SHA256").expect("explicit engine digest");
    let evidence = PathBuf::from(
        std::env::var_os("AETHYME_E1_EVIDENCE_DIR").expect("fresh evidence directory"),
    );
    std::fs::create_dir(&evidence).expect("evidence directory must be new");
    let engine = MergirafEngine::pin(&executable, &digest).unwrap();
    let mut rows = Vec::new();
    for (name, profile) in [
        ("git-text-baseline", None),
        ("mergiraf-html-candidate", Some(&engine)),
    ] {
        for case in fx::cases() {
            for scenario in &case.scenarios {
                let mut row = judge(&case, scenario, &evidence, profile);
                row["profile"] = json!(name);
                rows.push(row);
            }
        }
    }
    let report = json!({"schema":"aethyme.e1.original-fixture-campaign/v1","engine_sha256":engine.sha256(),"rows":rows});
    std::fs::write(
        evidence.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    for row in report["rows"].as_array().unwrap() {
        eprintln!(
            "{} {} {} accepted={} {:?}",
            row["profile"], row["case"], row["scenario"], row["accepted"], row["oracle_failures"]
        );
    }
    assert_eq!(
        report["rows"].as_array().unwrap().len(),
        fx::cases()
            .iter()
            .map(|case| case.scenarios.len())
            .sum::<usize>()
            * 2
    );
    // A completed campaign is not a passed Q1. The external requirements,
    // browser positives, real synthesis and final sealed variant decide that.
}
