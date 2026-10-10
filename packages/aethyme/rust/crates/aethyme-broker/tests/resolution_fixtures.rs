//! #665: candidate manifests, bounded resolution requests and the checks on
//! a resolver's proposal, against the provisional fixtures (#733).
//!
//! Fixture runs use the fixtures' composer-facing API, as `composer_fixtures`
//! does, and never branch on a case or scenario id. One thing here does read
//! the answer key: the stand-in for the gates. `check` asks the fixtures'
//! oracle (`judge`) whether a candidate passes, because there are no real
//! gates in a unit fixture. So the scenarios this adds to the composer's
//! count measure the plumbing (a failing candidate becomes a request), not
//! the system. Only an opaque check id and pass/fail leave `check`; the
//! oracle's messages never reach a request or a resolver, and a test holds
//! that. The fake resolvers are test doubles; none ships.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use aethyme_broker::collaboration_archive::{self, CommitOid};
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary, capture,
};
use aethyme_broker::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, forbidden_roots,
};
use aethyme_broker::composer::{
    self, Composition, CompositionBudget, CompositionRequest, ContributionSpec, Subtraction,
};
use aethyme_broker::composition::{CompositionMode, CompositionOutcome, Producer};
use aethyme_broker::resolution::{
    self, FailedCheck, MAX_SYNTHESIS_ATTEMPTS, Preference, Proposal, ProposedFile, RequirementRef,
    Resolution, ResolutionFailure, ResolutionInputs, ResolutionOutcome, ResolutionRequest,
    Resolver, ResolverIdentity, Response, Trigger,
};
use aethyme_contracts::experimental_v0::canonical_json::Value;
use aethyme_contracts::experimental_v0::{EntryKind, RecordId};
use aethyme_testkit::composition_fixtures::{
    self as fx, CaseInput, Observed, Outcome, ScenarioInput,
};

fn open_store(root: &Path, repo: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(root),
        &ProjectKey::parse("fixtures").unwrap(),
        &forbidden_roots(repo),
    )
    .unwrap()
}

fn capture_lineage(
    store: &mut CollaborationStore,
    repo: &Path,
    base: &str,
    result: &str,
) -> RecordId {
    let request = CaptureRequest {
        operation_id: OperationId::mint().unwrap(),
        repository: repo.to_path_buf(),
        base: CommitOid::parse(base).unwrap(),
        result: CommitOid::parse(result).unwrap(),
        policy: CapturePolicy::Advisory,
        retention: RetentionBoundary::UntilReleased,
    };
    let CaptureOutcome::Acknowledged(receipt) = capture(store, &request).unwrap() else {
        panic!("capture of {base}..{result} was incomplete");
    };
    receipt.contribution
}

struct World {
    root: tempfile::TempDir,
    store: CollaborationStore,
    repo: PathBuf,
    catalog: Vec<ContributionSpec>,
    baseline: CommitOid,
    accepted: Vec<CommitOid>,
    /// Each retained contribution's base and result commits.
    commits: Vec<(String, String, String)>,
}

fn world(case: &CaseInput, input: &ScenarioInput) -> World {
    let root = tempfile::tempdir().unwrap();
    let materialized = case.materialize(&input.id, &root.path().join("repos"));
    let repo = materialized.repo.clone();
    let mut store = open_store(&root.path().join("state"), &repo);
    let mut commits = Vec::new();
    let catalog = case
        .contributions
        .iter()
        .map(|contribution| ContributionSpec {
            id: contribution.id.clone(),
            lineage: materialized.contributions.get(&contribution.id).map(|c| {
                commits.push((contribution.id.clone(), c.base.clone(), c.result.clone()));
                capture_lineage(&mut store, &repo, &c.base, &c.result)
            }),
            requires: contribution.requires.clone(),
            atomic_group: contribution.atomic_group.clone(),
            revision_of: contribution.revision_of.clone(),
            derived_from: contribution.derived_from.clone(),
        })
        .collect();
    World {
        baseline: CommitOid::parse(&materialized.baseline).unwrap(),
        accepted: materialized
            .accepted
            .iter()
            .map(|commit| CommitOid::parse(commit).unwrap())
            .collect(),
        root,
        store,
        repo,
        catalog,
        commits,
    }
}

fn request_for(world: &World, order: &[String]) -> CompositionRequest {
    CompositionRequest {
        baseline: world.baseline.clone(),
        accepted: world.accepted.clone(),
        catalog: world.catalog.clone(),
        deliveries: order.to_vec(),
        budget: CompositionBudget::default(),
    }
}

fn compose(world: &mut World, input: &ScenarioInput, request: &CompositionRequest) -> Composition {
    match &input.request.subtract {
        Some(subtract) => composer::recompose_without(
            &mut world.store,
            &world.repo,
            request,
            &Subtraction {
                from: subtract.from.clone(),
                remove: subtract.remove.clone(),
                keep: subtract.keep.clone(),
            },
        )
        .unwrap(),
        None => composer::compose(&mut world.store, &world.repo, request).unwrap(),
    }
}

fn outcome_of(code: &str) -> Outcome {
    match code {
        "conflict" => Outcome::Conflict,
        "merge_commit"
        | "snapshot_entry"
        | "commit_shape"
        | "transforming_attribute"
        | "partial_clone" => Outcome::Unsupported,
        "unknown_base" => Outcome::UnknownBase,
        "dependency_cycle" => Outcome::DependencyCycle,
        "missing_input" => Outcome::MissingInput,
        "competing_revisions" => Outcome::CompetingRevisions,
        "budget_exhausted" => Outcome::BudgetExhausted,
        "inseparable_selection" => Outcome::InseparableSelection,
        other => panic!("no oracle outcome for {other}"),
    }
}

fn reconstruct(
    world: &World,
    subject: &aethyme_contracts::experimental_v0::SourceSnapshotId,
) -> PathBuf {
    let dir = world
        .root
        .path()
        .join(format!("out-{}", &subject.as_str()[7..19]));
    if !dir.exists() {
        collaboration_archive::reconstruct(&world.store, subject, &dir).unwrap();
    }
    dir
}

/// The stand-in for the gates on one candidate: an opaque failed check
/// when the oracle rejects it. The oracle's own messages are returned
/// separately, only so a test can prove they never reach a request.
fn check(case: &CaseInput, scenario: &str, dir: &Path) -> (Vec<FailedCheck>, Vec<String>) {
    let verdict = fx::judge(case, scenario, Observed::Candidate(dir));
    if verdict.accepted {
        return (Vec::new(), Vec::new());
    }
    (
        vec![FailedCheck {
            check: "independent-behavior".into(),
        }],
        verdict.failures,
    )
}

fn requirements() -> Vec<RequirementRef> {
    vec![RequirementRef {
        id: "independent-behavior".into(),
        path: None,
    }]
}

/// Compose, write the manifest, verify a candidate, and raise a request
/// when its checks fail. The outcome the scenario observes, and the request.
fn pipeline(
    world: &mut World,
    case: &CaseInput,
    input: &ScenarioInput,
    order: &[String],
) -> (Result<PathBuf, Outcome>, Option<ResolutionRequest>) {
    let request = request_for(world, order);
    let composition = compose(world, input, &request);
    let candidate = composition.outcome.candidate().cloned();
    let (observed, raised) = match &candidate {
        Some(candidate) => {
            let dir = reconstruct(world, &candidate.subject);
            let (failures, hidden) = check(case, &input.id, &dir);
            if failures.is_empty() {
                (Ok(dir), None)
            } else {
                let raised = resolution::request_resolution(
                    &mut world.store,
                    &world.repo,
                    &request,
                    &composition,
                    &failures,
                    &requirements(),
                    None,
                )
                .unwrap();
                // The oracle's messages are not in the request.
                let record = resolution::read_record(&world.store, raised.record()).unwrap();
                let bytes = String::from_utf8(record.canonical_bytes()).unwrap();
                for message in &hidden {
                    assert!(!bytes.contains(message.as_str()), "{message}");
                }
                (Err(Outcome::ResolutionRequired), Some(raised))
            }
        }
        None => (Err(outcome_of(composition.outcome.code())), None),
    };
    let resolutions: Vec<&ResolutionRequest> = raised.iter().collect();
    let manifest = resolution::write_manifest(
        &world.store,
        &world.repo,
        &request,
        &composition,
        &resolutions,
    )
    .unwrap();
    // Only a candidate has a subject, and only a candidate awaits
    // verification; every attempt names its retained inputs exactly.
    assert_eq!(
        manifest.subject,
        candidate.as_ref().map(|c| c.subject.clone())
    );
    assert_eq!(manifest.requires_verification, candidate.is_some());
    assert_eq!(manifest.outcome, composition.outcome.code());
    for input in &manifest.inputs {
        if let Some(lineage) = &input.lineage {
            let retained = collaboration_archive::retained_contribution(&world.store, lineage)
                .unwrap()
                .expect("a manifest input with a lineage is retained");
            assert_eq!(input.base.as_ref(), Some(&retained.base.snapshot_id));
            assert_eq!(input.result.as_ref(), Some(&retained.result.snapshot_id));
        } else {
            assert!(input.base.is_none() && input.result.is_none());
        }
    }
    // The record itself, not only the returned summary.
    let record = resolution::read_record(&world.store, &manifest.record).unwrap();
    assert_eq!(record.id(), manifest.record.id);
    let field = |name: &str| match record.get(name) {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    assert_eq!(
        field("subject"),
        candidate.as_ref().map(|c| c.subject.to_string())
    );
    assert_eq!(
        field("candidate_commit"),
        candidate.as_ref().map(|c| c.commit.as_str().to_string())
    );
    assert_eq!(
        record.get("requires_verification"),
        Some(&Value::Bool(candidate.is_some()))
    );
    assert_eq!(field("recipe").is_some(), candidate.is_some());
    (observed, raised)
}

fn judge(case: &CaseInput, input: &ScenarioInput) -> (fx::Verdict, Vec<String>) {
    let mut world = world(case, input);
    let orders = if input.orders.is_empty() {
        vec![Vec::new()]
    } else {
        input.orders.clone()
    };
    let mut seen = Vec::new();
    let mut lines = Vec::new();
    for order in &orders {
        for _ in 0..2 {
            let (observed, raised) = pipeline(&mut world, case, input, order);
            lines.push(format!(
                "{order:?}: {}",
                match (&observed, &raised) {
                    (Ok(_), _) => "candidate".to_string(),
                    (Err(outcome), Some(request)) => format!(
                        "{} (group of {}, scope {:?})",
                        outcome.as_str(),
                        request.members().len(),
                        request.scope()
                    ),
                    (Err(outcome), None) => outcome.as_str().to_string(),
                }
            ));
            seen.push((order.clone(), observed));
        }
    }
    let runs: Vec<(Vec<String>, Observed<'_>)> = seen
        .iter()
        .map(|(order, observed)| {
            let observed = match observed {
                Ok(dir) => Observed::Candidate(dir),
                Err(outcome) => Observed::Refused(*outcome),
            };
            (order.clone(), observed)
        })
        .collect();
    (fx::judge_scenario(case, &input.id, &runs), lines)
}

/// The composer alone met 15 provisional scenarios (#664). Verification
/// turning a clean but wrong candidate into a resolution request adds the
/// semantic interaction; the move-plus-edit positive still needs E1's
/// structural engine.
const ACCEPTED_FLOOR: usize = 16;

#[test]
fn verification_raises_requests_where_a_candidate_fails_its_checks() {
    let mut accepted = 0;
    let mut total = 0;
    let mut lines = Vec::new();
    for case in fx::cases() {
        for input in &case.scenarios {
            let (verdict, runs) = judge(&case, input);
            total += 1;
            accepted += usize::from(verdict.accepted);
            lines.push(format!(
                "{} {}: accepted={} {}\n  {}",
                case.case,
                input.id,
                verdict.accepted,
                verdict.failures.join("; "),
                runs.join("\n  ")
            ));
        }
    }
    eprintln!("{}\n{accepted}/{total} accepted", lines.join("\n"));
    assert!(
        accepted >= ACCEPTED_FLOOR,
        "{accepted}/{total} accepted, below the floor of {ACCEPTED_FLOOR}"
    );
}

#[test]
fn held_out_cases_are_reported() {
    let mut lines = Vec::new();
    for case in fx::held_out_cases() {
        for input in &case.scenarios {
            let (verdict, runs) = judge(&case, input);
            lines.push(format!(
                "{} {}: accepted={} {}\n  {}",
                case.case,
                input.id,
                verdict.accepted,
                verdict.failures.join("; "),
                runs.join("\n  ")
            ));
        }
    }
    eprintln!("{}", lines.join("\n"));
}

// ----------------------------------------------- one failing interaction

/// The first scenario whose composed candidate fails its independent check
/// in every declared order: found by behavior, not by name.
fn failing_interaction(cases: Vec<CaseInput>) -> Option<(CaseInput, ScenarioInput)> {
    for case in cases {
        for input in case.scenarios.clone() {
            if input.request.subtract.is_some() || input.orders.is_empty() {
                continue;
            }
            let mut world = world(&case, &input);
            let raised = input.orders.iter().all(|order| {
                matches!(
                    pipeline(&mut world, &case, &input, order),
                    (Err(Outcome::ResolutionRequired), Some(_))
                )
            });
            if raised {
                return Some((case, input));
            }
        }
    }
    None
}

/// Raise the request for `input`'s first order in a fresh world.
fn raise(case: &CaseInput, input: &ScenarioInput) -> (World, ResolutionRequest) {
    let mut world = world(case, input);
    let (_, raised) = pipeline(&mut world, case, input, &input.orders[0]);
    (world, raised.expect("the candidate fails its checks"))
}

/// A test double that propagates identifier renames: where a member's own
/// change renames `id="old"` to `id="new"`, it rewrites the remaining
/// `"old"` and `"#old"` references in the accumulator's scoped files. It
/// reads only the request's inputs.
struct RenamePropagator;

impl Resolver for RenamePropagator {
    fn identity(&self) -> ResolverIdentity {
        ResolverIdentity {
            name: "test-rename-propagator".into(),
            version: "0".into(),
        }
    }

    fn propose(
        &mut self,
        request: &ResolutionRequest,
        inputs: &ResolutionInputs<'_>,
    ) -> Result<Response, String> {
        let mut renames = Vec::new();
        for member in request.members() {
            for path in request.scope() {
                let read = |snapshot| {
                    inputs
                        .read(snapshot, path)
                        .unwrap()
                        .map(|f| String::from_utf8(f.bytes).unwrap())
                        .unwrap_or_default()
                };
                let (base, result) = (read(&member.base), read(&member.result));
                for (old, new) in base.lines().zip(result.lines()) {
                    if let (Some(a), Some(b)) = (id_of(old), id_of(new))
                        && a != b
                    {
                        renames.push((a, b));
                    }
                }
            }
        }
        let mut proposal = Proposal {
            brief: Some("propagate renamed element ids to their references".into()),
            ..Proposal::default()
        };
        for path in request.scope() {
            let Some(file) = inputs
                .read(&request.accumulator().snapshot_id, path)
                .unwrap()
            else {
                continue;
            };
            let mut text = String::from_utf8(file.bytes.clone()).unwrap();
            for (old, new) in &renames {
                text = text
                    .replace(&format!("\"{old}\""), &format!("\"{new}\""))
                    .replace(&format!("\"#{old}\""), &format!("\"#{new}\""));
            }
            if text.as_bytes() != file.bytes {
                proposal.writes.insert(
                    path.clone(),
                    Some(ProposedFile {
                        kind: file.kind,
                        bytes: text.into_bytes(),
                    }),
                );
            }
        }
        Ok(Response::Proposal(proposal))
    }
}

fn id_of(line: &str) -> Option<String> {
    let start = line.find("id=\"")? + 4;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

type Respond = dyn Fn(&ResolutionRequest, &ResolutionInputs<'_>) -> Result<Response, String>;

/// A test double that returns a fixed response and counts its calls.
struct Fixed {
    response: Box<Respond>,
    calls: Rc<Cell<u32>>,
    name: &'static str,
}

impl Resolver for Fixed {
    fn identity(&self) -> ResolverIdentity {
        ResolverIdentity {
            name: self.name.into(),
            version: "0".into(),
        }
    }

    fn propose(
        &mut self,
        request: &ResolutionRequest,
        inputs: &ResolutionInputs<'_>,
    ) -> Result<Response, String> {
        self.calls.set(self.calls.get() + 1);
        (self.response)(request, inputs)
    }
}

fn fixed(
    name: &'static str,
    response: impl Fn(&ResolutionRequest, &ResolutionInputs<'_>) -> Result<Response, String> + 'static,
) -> (Fixed, Rc<Cell<u32>>) {
    let calls = Rc::new(Cell::new(0));
    (
        Fixed {
            response: Box::new(response),
            calls: calls.clone(),
            name,
        },
        calls,
    )
}

fn write(path: &str, bytes: &[u8], kind: EntryKind) -> Proposal {
    let mut proposal = Proposal::default();
    proposal.writes.insert(
        path.into(),
        Some(ProposedFile {
            kind,
            bytes: bytes.to_vec(),
        }),
    );
    proposal
}

fn failure(resolution: &Resolution) -> Option<ResolutionFailure> {
    match &resolution.outcome {
        ResolutionOutcome::Failed { state, .. } => Some(*state),
        ResolutionOutcome::Synthesized(_) => None,
    }
}

#[test]
fn a_failed_check_raises_an_exact_scoped_request() {
    let (case, input) = failing_interaction(fx::cases()).expect("a failing interaction");
    let (world, request) = raise(&case, &input);
    let Trigger::FailedChecks { candidate, checks } = request.trigger() else {
        panic!("raised by failed checks");
    };
    assert!(!checks.is_empty());
    assert_eq!(*candidate, request.accumulator().snapshot_id);
    // Every member is the retained contribution, read from the archive.
    assert_eq!(request.members().len(), input.request.compose.len());
    for member in request.members() {
        let lineage = world
            .catalog
            .iter()
            .find(|spec| spec.id == member.id)
            .and_then(|spec| spec.lineage.clone())
            .unwrap();
        let retained = collaboration_archive::retained_contribution(&world.store, &lineage)
            .unwrap()
            .unwrap();
        assert_eq!(member.lineage, lineage);
        assert_eq!(member.base, retained.base.snapshot_id);
        assert_eq!(member.result, retained.result.snapshot_id);
    }
    // The scope is what the group's changes touch, nothing else.
    let touched: std::collections::BTreeSet<String> = world
        .commits
        .iter()
        .flat_map(|(_, base, result)| changed(&world.repo, base, result))
        .collect();
    assert_eq!(request.scope(), touched.into_iter().collect::<Vec<_>>());
    assert_eq!(request.allowance().limit, MAX_SYNTHESIS_ATTEMPTS);
    assert_eq!(request.allowance().used, 0);
    assert_eq!(request.baseline().commit, world.baseline);
    assert!(request.protected().iter().any(|p| p == "dir:.aethyme"));
    resolution::read_record(&world.store, request.record()).unwrap();
}

fn changed(repo: &Path, base: &str, result: &str) -> Vec<String> {
    git(repo, &["diff", "--name-only", "--no-renames", base, result])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_correct_proposal_becomes_a_new_candidate_that_passes_its_checks() {
    let (case, input) = failing_interaction(fx::cases()).expect("a failing interaction");
    let (mut world, request) = raise(&case, &input);
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut RenamePropagator,
    )
    .unwrap();
    let ResolutionOutcome::Synthesized(synthesized) = &resolution.outcome else {
        panic!("{:?}", resolution.outcome);
    };
    assert_eq!(resolution.attempt, Some(1));
    assert_eq!(resolution.allowance.used, 1);
    assert!(matches!(
        synthesized.candidate.producer,
        Producer::Resolver { .. }
    ));
    assert_eq!(synthesized.candidate.mode, CompositionMode::Synthesized);
    assert_ne!(
        synthesized.candidate.subject,
        request.accumulator().snapshot_id
    );
    assert_eq!(synthesized.candidate.baseline, world.baseline);
    // A new candidate, checked again as itself.
    let dir = reconstruct(&world, &synthesized.candidate.subject);
    let verdict = fx::judge(&case, &input.id, Observed::Candidate(&dir));
    assert!(verdict.accepted, "{:?}", verdict.failures);
    let record = resolution::read_record(&world.store, &synthesized.group_record).unwrap();
    assert_eq!(record.id(), synthesized.group_record.id);
}

#[test]
fn held_out_interactions_are_reported_after_resolution() {
    let Some((case, input)) = failing_interaction(fx::held_out_cases()) else {
        eprintln!("no held-out interaction fails its checks");
        return;
    };
    let (mut world, request) = raise(&case, &input);
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut RenamePropagator,
    )
    .unwrap();
    let line = match &resolution.outcome {
        ResolutionOutcome::Synthesized(synthesized) => {
            let dir = reconstruct(&world, &synthesized.candidate.subject);
            let verdict = fx::judge(&case, &input.id, Observed::Candidate(&dir));
            format!(
                "synthesized, accepted={} {:?}",
                verdict.accepted, verdict.failures
            )
        }
        other => other.code().to_string(),
    };
    eprintln!("{} {}: {line}", case.case, input.id);
}

// ------------------------------------------------- no authorized preference

/// The first scenario that conflicts in every declared order.
fn contradiction() -> (CaseInput, ScenarioInput) {
    for case in fx::cases() {
        for input in case.scenarios.clone() {
            if input.request.subtract.is_some() || input.orders.len() < 2 {
                continue;
            }
            let mut world = world(&case, &input);
            let conflicts = input.orders.iter().all(|order| {
                let request = request_for(&world, order);
                let composition = compose(&mut world, &input, &request);
                matches!(
                    &composition.outcome,
                    CompositionOutcome::Conflict { conflicts, .. }
                        if conflicts.iter().all(|c| c.reason.code() == "content")
                )
            });
            if conflicts {
                return (case, input);
            }
        }
    }
    panic!("no scenario conflicts in every order");
}

fn conflict_request(
    case: &CaseInput,
    input: &ScenarioInput,
    preference: Option<Preference>,
) -> (World, ResolutionRequest) {
    let mut world = world(case, input);
    let request = request_for(&world, &input.orders[0]);
    let composition = compose(&mut world, input, &request);
    let raised = resolution::request_resolution(
        &mut world.store,
        &world.repo,
        &request,
        &composition,
        &[],
        &requirements(),
        preference,
    )
    .unwrap();
    (world, raised)
}

fn conflict_path(request: &ResolutionRequest) -> String {
    let Trigger::Conflict(conflicts) = request.trigger() else {
        panic!("raised by a conflict");
    };
    conflicts[0].path.clone()
}

/// The conflicting step's own version of the conflicting path.
fn theirs(request: &ResolutionRequest, inputs: &ResolutionInputs<'_>) -> ProposedFile {
    let member = request.members().last().unwrap();
    inputs
        .read(&member.result, &conflict_path(request))
        .unwrap()
        .unwrap()
}

#[test]
fn without_a_preference_a_contradiction_stays_unresolved() {
    let (case, input) = contradiction();

    // The resolver itself declines.
    let (mut world, request) = conflict_request(&case, &input, None);
    assert!(request.preference().is_none());
    assert_eq!(request.members().len(), 2, "both writers are in the group");
    let (mut declines, _) = fixed("declines", |_, _| {
        Ok(Response::Unresolved("no authorized preference".into()))
    });
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut declines,
    )
    .unwrap();
    assert_eq!(failure(&resolution), Some(ResolutionFailure::Unresolved));

    // A last writer is refused mechanically.
    let (mut last_writer, _) = fixed("last-writer", |request, inputs| {
        let theirs = theirs(request, inputs);
        Ok(Response::Proposal(write(
            &conflict_path(request),
            &theirs.bytes,
            theirs.kind,
        )))
    });
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut last_writer,
    )
    .unwrap();
    assert_eq!(failure(&resolution), Some(ResolutionFailure::Unresolved));

    // So is keeping the first writer by writing nothing for the path.
    let (mut world, request) = conflict_request(&case, &input, None);
    let (mut first_writer, _) = fixed("first-writer", |_, _| {
        Ok(Response::Proposal(Proposal::default()))
    });
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut first_writer,
    )
    .unwrap();
    assert_eq!(failure(&resolution), Some(ResolutionFailure::Unresolved));

    // An average is new content the guard cannot tell from a resolution:
    // it becomes a candidate that must be verified, and its checks fail.
    let (mut averager, _) = fixed("averager", |request, inputs| {
        let path = conflict_path(request);
        let ours = inputs
            .read(&request.accumulator().snapshot_id, &path)
            .unwrap()
            .unwrap();
        let theirs = theirs(request, inputs);
        let mut merged = Vec::new();
        for (a, b) in String::from_utf8(ours.bytes)
            .unwrap()
            .lines()
            .zip(String::from_utf8(theirs.bytes).unwrap().lines())
        {
            merged.extend_from_slice(if a == b { a.as_bytes() } else { b.as_bytes() });
            if a != b {
                merged.extend_from_slice(b" / ");
                merged.extend_from_slice(a.as_bytes());
            }
            merged.push(b'\n');
        }
        Ok(Response::Proposal(write(&path, &merged, ours.kind)))
    });
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut averager,
    )
    .unwrap();
    let ResolutionOutcome::Synthesized(synthesized) = &resolution.outcome else {
        panic!("{:?}", resolution.outcome);
    };
    assert!(matches!(
        synthesized.candidate.producer,
        Producer::Resolver { .. }
    ));
    let dir = reconstruct(&world, &synthesized.candidate.subject);
    assert!(!fx::judge(&case, &input.id, Observed::Candidate(&dir)).accepted);
}

#[test]
fn an_authorized_preference_lets_one_side_win() {
    let (case, input) = contradiction();
    let (_, probe) = conflict_request(&case, &input, None);
    let winner = probe.members().last().unwrap().id.clone();
    let (mut world, request) = conflict_request(
        &case,
        &input,
        Some(Preference {
            authorized_by: "issuer".into(),
            prefer: winner,
        }),
    );
    let (mut last_writer, _) = fixed("last-writer", |request, inputs| {
        let theirs = theirs(request, inputs);
        Ok(Response::Proposal(write(
            &conflict_path(request),
            &theirs.bytes,
            theirs.kind,
        )))
    });
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut last_writer,
    )
    .unwrap();
    assert_eq!(resolution.outcome.code(), "synthesized");
}

// -------------------------------------------------------- T23 boundaries

fn interaction() -> (CaseInput, ScenarioInput) {
    failing_interaction(fx::cases()).expect("a failing interaction")
}

#[test]
fn writes_outside_the_scope_are_refused() {
    let (case, input) = interaction();
    let (world, request) = raise(&case, &input);
    let scoped = request.scope()[0].clone();
    let attacks: Vec<(&str, Proposal)> = vec![
        ("outside", write("README.md", b"x\n", EntryKind::Regular)),
        (
            "policy",
            write(".aethyme/gates.toml", b"[gates]\n", EntryKind::Regular),
        ),
        (
            "ci",
            write(".github/workflows/x.yml", b"on: push\n", EntryKind::Regular),
        ),
        (
            "attributes",
            write(".gitattributes", b"* -diff\n", EntryKind::Regular),
        ),
        (
            "executable",
            write(&scoped, b"#!/bin/sh\n", EntryKind::Executable),
        ),
        (
            "symlink",
            write(&scoped, b"/etc/passwd", EntryKind::Symlink),
        ),
    ];
    for (name, proposal) in attacks {
        // A fresh group allowance per attack: each is its own world.
        let (mut world, request) = raise(&case, &input);
        let (mut attacker, _) = fixed("attacker", move |_, _| {
            Ok(Response::Proposal(proposal.clone()))
        });
        let resolution = resolution::resolve(
            &mut world.store,
            &world.repo,
            request.record(),
            &mut attacker,
        )
        .unwrap();
        assert_eq!(
            failure(&resolution),
            Some(ResolutionFailure::ScopeViolation),
            "{name}"
        );
        // A refused attempt is still charged.
        assert_eq!(resolution.allowance.used, 1, "{name}");
    }

    // A requirement's harness file is protected even inside the scope.
    let mut world2 = world;
    let composition_request = request_for(&world2, &input.orders[0]);
    let composition = compose(&mut world2, &input, &composition_request);
    let dir = reconstruct(&world2, &composition.outcome.candidate().unwrap().subject);
    let harnessed = resolution::request_resolution(
        &mut world2.store,
        &world2.repo,
        &composition_request,
        &composition,
        &check(&case, &input.id, &dir).0,
        &[RequirementRef {
            id: "harness".into(),
            path: Some(scoped.clone()),
        }],
        None,
    )
    .unwrap();
    let (mut attacker, _) = fixed("attacker", move |_, _| {
        Ok(Response::Proposal(write(
            &scoped,
            b"x\n",
            EntryKind::Regular,
        )))
    });
    let resolution = resolution::resolve(
        &mut world2.store,
        &world2.repo,
        harnessed.record(),
        &mut attacker,
    )
    .unwrap();
    assert_eq!(
        failure(&resolution),
        Some(ResolutionFailure::ScopeViolation)
    );
    drop(request);
}

#[test]
fn reads_outside_the_request_are_refused() {
    let (case, input) = interaction();
    let (mut world, request) = raise(&case, &input);
    let (mut reader, _) = fixed("reader", |request, inputs| {
        let snapshot = &request.accumulator().snapshot_id;
        let error = inputs.read(snapshot, "README.md").unwrap_err();
        assert_eq!(error.code(), "out_of_scope_read");
        let path = &request.scope()[0];
        let foreign = aethyme_contracts::experimental_v0::SourceSnapshotId::parse(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert_eq!(
            inputs.read(&foreign, path).unwrap_err().code(),
            "out_of_scope_read"
        );
        Ok(Response::Inconclusive("probed".into()))
    });
    let resolution =
        resolution::resolve(&mut world.store, &world.repo, request.record(), &mut reader).unwrap();
    assert_eq!(failure(&resolution), Some(ResolutionFailure::Inconclusive));
}

#[test]
fn the_allowance_is_spent_once_per_group_and_never_replenished() {
    let (case, input) = interaction();
    let (mut world, request) = raise(&case, &input);
    let (mut flaky, calls) = fixed("flaky", |_, _| Err("resolver host unreachable".into()));
    let first =
        resolution::resolve(&mut world.store, &world.repo, request.record(), &mut flaky).unwrap();
    assert_eq!(
        failure(&first),
        Some(ResolutionFailure::InfrastructureDeferred)
    );
    let (mut declines, _) = fixed("another-resolver", |_, _| {
        Ok(Response::Unresolved("no".into()))
    });
    let second = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut declines,
    )
    .unwrap();
    assert_eq!(second.allowance.used, MAX_SYNTHESIS_ATTEMPTS);

    // A third attempt is refused before dispatch.
    let third =
        resolution::resolve(&mut world.store, &world.repo, request.record(), &mut flaky).unwrap();
    assert_eq!(failure(&third), Some(ResolutionFailure::BudgetExhausted));
    assert_eq!(third.attempt, None);
    assert_eq!(
        calls.get(),
        1,
        "the spent allowance never reached the resolver"
    );

    // The same changes under new names, new commits and new operations,
    // composed into a new candidate, are the same group.
    let mut renamed = Vec::new();
    for (id, base, result) in world.commits.clone() {
        let tree = git(&world.repo, &["rev-parse", &format!("{result}^{{tree}}")]);
        let commit = git(
            &world.repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &base,
                "-m",
                "the same change again",
            ],
        );
        assert_ne!(commit, result);
        let lineage = capture_lineage(&mut world.store, &world.repo, &base, &commit);
        let original = world.catalog.iter().find(|spec| spec.id == id).unwrap();
        renamed.push(ContributionSpec {
            id: format!("again-{id}"),
            lineage: Some(lineage),
            ..original.clone()
        });
    }
    let mut again = request_for(&world, &[]);
    again.catalog = renamed.clone();
    again.deliveries = renamed.iter().rev().map(|spec| spec.id.clone()).collect();
    let composition = composer::compose(&mut world.store, &world.repo, &again).unwrap();
    let candidate = composition
        .outcome
        .candidate()
        .expect("a candidate")
        .clone();
    let dir = reconstruct(&world, &candidate.subject);
    let retry = resolution::request_resolution(
        &mut world.store,
        &world.repo,
        &again,
        &composition,
        &check(&case, &input.id, &dir).0,
        &requirements(),
        None,
    )
    .unwrap();
    assert_ne!(retry.record().id, request.record().id);
    assert_eq!(retry.decision_key(), request.decision_key());
    assert_eq!(retry.allowance().remaining(), 0);
    let refused = resolution::resolve(
        &mut world.store,
        &world.repo,
        retry.record(),
        &mut RenamePropagator,
    )
    .unwrap();
    assert_eq!(failure(&refused), Some(ResolutionFailure::BudgetExhausted));
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn only_a_conflict_or_a_failed_candidate_can_be_resolved() {
    let (case, input) = interaction();
    let mut world = world(&case, &input);
    let request = request_for(&world, &input.orders[0]);
    let composition = compose(&mut world, &input, &request);
    // A candidate whose checks did not fail has nothing to resolve.
    let error = resolution::request_resolution(
        &mut world.store,
        &world.repo,
        &request,
        &composition,
        &[],
        &requirements(),
        None,
    )
    .unwrap_err();
    assert_eq!(error.code(), "invalid_request");
}

// ---------------------------------------------- alternate views (§7.4)

#[test]
fn a_synthesized_result_without_one_constituent_is_recomposed_never_relabelled() {
    let (case, input) = interaction();
    let (mut world, request) = raise(&case, &input);
    let resolution = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut RenamePropagator,
    )
    .unwrap();
    let ResolutionOutcome::Synthesized(x) = resolution.outcome else {
        panic!("synthesized");
    };
    // Offer X as a contribution of its own, derived from its constituents.
    let lineage = capture_lineage(
        &mut world.store,
        &world.repo,
        world.baseline.as_str(),
        x.candidate.commit.as_str(),
    );
    let constituents: Vec<String> = request.members().iter().map(|m| m.id.clone()).collect();
    let mut catalog = world.catalog.clone();
    catalog.push(ContributionSpec {
        id: "x".into(),
        lineage: Some(lineage),
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: constituents.clone(),
    });
    let mut composition_request = request_for(&world, &[]);
    composition_request.catalog = catalog;
    let kept = constituents[0].clone();
    let removed = constituents[1].clone();
    let without = composer::recompose_without(
        &mut world.store,
        &world.repo,
        &composition_request,
        &Subtraction {
            from: "x".into(),
            remove: vec![removed],
            keep: Vec::new(),
        },
    )
    .unwrap();
    let candidate = without.outcome.candidate().expect("a recomposed candidate");
    assert_ne!(candidate.subject, x.candidate.subject, "never X relabelled");
    composition_request.deliveries = vec![kept];
    let alone = composer::compose(&mut world.store, &world.repo, &composition_request).unwrap();
    assert_eq!(
        alone.outcome.candidate().unwrap().subject,
        candidate.subject,
        "X without one constituent is the other, recomposed from its original"
    );

    // Without its constituents' records, X is inseparable.
    composition_request.catalog.retain(|spec| spec.id == "x");
    let refused = composer::recompose_without(
        &mut world.store,
        &world.repo,
        &composition_request,
        &Subtraction {
            from: "x".into(),
            remove: vec![constituents[1].clone()],
            keep: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(refused.outcome.code(), "inseparable_selection");
}
