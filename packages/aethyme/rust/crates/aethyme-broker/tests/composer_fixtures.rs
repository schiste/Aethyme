//! The #664 provisional composer against the frozen provisional fixtures
//! (#733): materialize each case, capture its contributions through the
//! #658 capture API, compose from the archive alone, and judge the result
//! with the fixtures' independent oracle.
//!
//! The composer sees only a scenario's `ScenarioInput` and each
//! contribution's declared metadata; required outcomes, behaviors and the
//! measured text column are for judging. Held-out cases run last, as a
//! check, and nothing was tuned on them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::collaboration_archive::{self, CommitOid};
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary, capture,
};
use aethyme_broker::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, forbidden_roots,
};
use aethyme_broker::composer::{
    self, Composition, CompositionBudget, CompositionRequest, ContributionSpec,
    PROVISIONAL_TEXT_PROFILE, Subtraction,
};
use aethyme_broker::composition::{CompositionOutcome, ConflictReason, Producer};
use aethyme_contracts::experimental_v0::SourceSnapshotId;
use fixture::{Fixture, ScenarioInput, Seen};

/// The only code that touches the fixture API (#733). The composer side of
/// this file sees a scenario's `ScenarioInput`, each contribution's declared
/// metadata and the materialized commits; judging goes through `judge`.
mod fixture {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    pub use aethyme_testkit::composition_fixtures::ScenarioInput;
    use aethyme_testkit::composition_fixtures::{self as fx, Observed, Outcome};

    /// A contribution's declared metadata: what a capture's caller knows.
    pub struct Meta {
        pub id: String,
        pub requires: Vec<String>,
        pub atomic_group: Option<String>,
        pub revision_of: Option<String>,
        pub derived_from: Vec<String>,
    }

    pub struct Materialized {
        pub repo: PathBuf,
        /// Tree name to commit.
        pub trees: BTreeMap<String, String>,
        /// Contribution to its (base, result) commits.
        pub contributions: BTreeMap<String, (String, String)>,
    }

    pub enum Seen<'a> {
        Candidate(&'a Path),
        /// A non-candidate outcome by its #663 code.
        Refused(&'a str),
    }

    pub struct Fixture {
        case: fx::Case,
        pub name: String,
        pub scenarios: Vec<ScenarioInput>,
        pub contributions: Vec<Meta>,
    }

    fn view(case: fx::Case) -> Fixture {
        Fixture {
            name: case.case.clone(),
            scenarios: case.scenarios.iter().map(|s| s.input.clone()).collect(),
            contributions: case
                .contributions
                .iter()
                .map(|c| Meta {
                    id: c.id.clone(),
                    requires: c.requires.clone(),
                    atomic_group: c.atomic_group.clone(),
                    revision_of: c.revision_of.clone(),
                    derived_from: c.derived_from.clone(),
                })
                .collect(),
            case,
        }
    }

    pub fn cases() -> Vec<Fixture> {
        fx::cases().into_iter().map(view).collect()
    }

    pub fn held_out() -> Vec<Fixture> {
        fx::held_out_cases().into_iter().map(view).collect()
    }

    pub fn named(name: &str) -> Fixture {
        cases().into_iter().find(|f| f.name == name).unwrap()
    }

    impl Fixture {
        pub fn scenario(&self, id: &str) -> &ScenarioInput {
            self.scenarios.iter().find(|s| s.id == id).unwrap()
        }

        pub fn materialize(&self, root: &Path) -> Materialized {
            let m = fx::materialize(&self.case, root);
            Materialized {
                repo: m.repo,
                trees: m.trees,
                contributions: m
                    .contributions
                    .into_iter()
                    .map(|(id, c)| (id, (c.base, c.result)))
                    .collect(),
            }
        }

        /// Whether the oracle accepts what was seen, and why not.
        pub fn judge(&self, scenario: &str, seen: Seen<'_>) -> (bool, Vec<String>) {
            let observed = match seen {
                Seen::Candidate(dir) => Observed::Candidate(dir),
                Seen::Refused(code) => Observed::Refused(outcome_of(code)),
            };
            let verdict = fx::judge(&self.case, scenario, observed);
            (verdict.accepted, verdict.failures)
        }
    }

    fn outcome_of(code: &str) -> Outcome {
        match code {
            "conflict" => Outcome::Conflict,
            "merge_commit" | "snapshot_entry" => Outcome::Unsupported,
            "unknown_base" => Outcome::UnknownBase,
            "dependency_cycle" => Outcome::DependencyCycle,
            "missing_input" => Outcome::MissingInput,
            "competing_revisions" => Outcome::CompetingRevisions,
            "budget_exhausted" => Outcome::BudgetExhausted,
            "inseparable_selection" => Outcome::InseparableSelection,
            other => panic!("no oracle outcome for {other}"),
        }
    }
}

struct World {
    root: tempfile::TempDir,
    store: CollaborationStore,
    repo: PathBuf,
    catalog: Vec<ContributionSpec>,
    trees: BTreeMap<String, SourceSnapshotId>,
}

fn open_store(root: &Path, repo: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(root),
        &ProjectKey::parse("fixtures").unwrap(),
        &forbidden_roots(repo),
    )
    .unwrap()
}

/// Capture every contribution of `case`; those in `unretained` go to a
/// separate store, so their lineage is known but this archive lacks them.
fn world(case: &Fixture, unretained: &[String]) -> World {
    let root = tempfile::tempdir().unwrap();
    let materialized = case.materialize(&root.path().join("repos"));
    let repo = materialized.repo.clone();
    let mut store = open_store(&root.path().join("state"), &repo);
    let mut elsewhere = open_store(&root.path().join("elsewhere"), &repo);
    let mut catalog = Vec::new();
    for contribution in &case.contributions {
        let (base, result) = &materialized.contributions[&contribution.id];
        let request = CaptureRequest {
            operation_id: OperationId::mint().unwrap(),
            repository: repo.clone(),
            base: CommitOid::parse(base).unwrap(),
            result: CommitOid::parse(result).unwrap(),
            policy: CapturePolicy::Advisory,
            retention: RetentionBoundary::UntilReleased,
        };
        let target = if unretained.contains(&contribution.id) {
            &mut elsewhere
        } else {
            &mut store
        };
        let CaptureOutcome::Acknowledged(receipt) = capture(target, &request).unwrap() else {
            panic!("capture of {} was incomplete", contribution.id);
        };
        catalog.push(ContributionSpec {
            id: contribution.id.clone(),
            lineage: receipt.contribution,
            requires: contribution.requires.clone(),
            atomic_group: contribution.atomic_group.clone(),
            revision_of: contribution.revision_of.clone(),
            derived_from: contribution.derived_from.clone(),
        });
    }
    let trees = materialized
        .trees
        .iter()
        .map(|(name, commit)| {
            let id = collaboration_archive::snapshot_of_commit(
                &repo,
                &CommitOid::parse(commit).unwrap(),
            )
            .unwrap()
            .id();
            (name.clone(), id)
        })
        .collect();
    World {
        root,
        store,
        repo,
        catalog,
        trees,
    }
}

fn request(world: &World, input: &ScenarioInput, deliveries: &[String]) -> CompositionRequest {
    CompositionRequest {
        baseline: world.trees[&input.baseline].clone(),
        catalog: world.catalog.clone(),
        deliveries: deliveries.to_vec(),
        budget: CompositionBudget::default(),
    }
}

/// Every delivery order the scenario lists, or its selection once.
fn orders(input: &ScenarioInput) -> Vec<Vec<String>> {
    if input.orders.is_empty() {
        vec![input.request.compose.clone()]
    } else {
        input.orders.clone()
    }
}

fn run(world: &mut World, input: &ScenarioInput, deliveries: &[String]) -> Composition {
    let request = request(world, input, deliveries);
    match &input.request.subtract {
        Some(subtract) => composer::recompose_without(
            &mut world.store,
            &world.repo,
            &request,
            &Subtraction {
                from: subtract.from.clone(),
                remove: subtract.remove.clone(),
                keep: subtract.keep.clone(),
            },
        )
        .unwrap(),
        None => composer::compose(&mut world.store, &world.repo, &request).unwrap(),
    }
}

/// Judge one composition; a candidate is rebuilt from the archive alone.
fn judge(
    world: &World,
    case: &Fixture,
    scenario: &str,
    composition: &Composition,
) -> (bool, Vec<String>) {
    match &composition.outcome {
        CompositionOutcome::Candidate(candidate) => {
            let dir = world.root.path().join(format!(
                "out-{}",
                candidate.subject.as_str().replace(':', "-")
            ));
            if !dir.exists() {
                collaboration_archive::reconstruct(&world.store, &candidate.subject, &dir).unwrap();
            }
            case.judge(scenario, Seen::Candidate(&dir))
        }
        other => case.judge(scenario, Seen::Refused(other.code())),
    }
}

/// Where the provisional text profile cannot meet the required outcome, and
/// what it reports instead (plan §7.6: report the gap, do not widen the
/// contract to hide it). `true` when the result is a candidate that fails
/// its behaviors.
const KNOWN_GAPS: &[(&str, &str, &str)] = &[
    // A line merge cannot carry the edit along the move: the mandatory
    // structural positive waits for E1's engine.
    ("FX02", "move-and-edit", "conflict"),
    // Clean text, wrong behavior (the handler keeps the old id). Only the
    // complete-candidate check sees it; resolution is #665's.
    ("FX04", "interaction", "candidate"),
];

fn gap(case: &Fixture, scenario: &str) -> Option<&'static str> {
    KNOWN_GAPS
        .iter()
        .find(|(name, id, _)| *name == case.name && *id == scenario)
        .map(|(_, _, code)| *code)
}

#[test]
fn every_provisional_scenario_meets_its_requirement_or_a_recorded_gap() {
    let mut table = Vec::new();
    for case in fixture::cases() {
        for input in &case.scenarios {
            let mut world = world(&case, &input.unretained);
            let mut subjects = Vec::new();
            for order in orders(input) {
                let composition = run(&mut world, input, &order);
                let verdict = judge(&world, &case, &input.id, &composition);
                let code = composition.outcome.code();
                table.push(format!(
                    "{} {} {:?}: {code} accepted={}",
                    case.name, input.id, order, verdict.0
                ));
                match gap(&case, &input.id) {
                    Some(expected) => {
                        assert_eq!(code, expected, "{} {} {order:?}", case.name, input.id);
                        assert!(
                            !verdict.0,
                            "{} {} is a recorded gap but now passes: update KNOWN_GAPS",
                            case.name, input.id
                        );
                    }
                    None => assert!(
                        verdict.0,
                        "{} {} {order:?}: {code}: {:?}",
                        case.name, input.id, verdict.1
                    ),
                }
                if let Some(candidate) = composition.outcome.candidate() {
                    subjects.push(candidate.subject.clone());
                } else {
                    subjects.push(
                        SourceSnapshotId::parse(&format!("sha256:{}", "0".repeat(64))).unwrap(),
                    );
                }
                assert!(
                    composition.outcome.candidate().is_some() == composition.recipe.is_some(),
                    "a recipe exactly when there is a candidate"
                );
            }
            if input.commutative {
                assert!(
                    subjects.windows(2).all(|pair| pair[0] == pair[1]),
                    "{} {}: claimed commutative, but orders differ: {subjects:?}",
                    case.name,
                    input.id
                );
            }
        }
    }
    eprintln!("{}", table.join("\n"));
}

#[test]
fn identical_twins_conflict_instead_of_a_silent_wrong_merge() {
    let case = fixture::named("FX02");
    let input = case.scenario("identical-twins");
    let mut world = world(&case, &[]);
    for order in orders(input) {
        let composition = run(&mut world, input, &order);
        let CompositionOutcome::Conflict { conflicts, .. } = &composition.outcome else {
            panic!("{order:?}: {:?}", composition.outcome);
        };
        assert!(
            conflicts
                .iter()
                .all(|conflict| conflict.reason == ConflictReason::MovedBlock),
            "{conflicts:?}"
        );
    }
}

fn case_world(name: &str, scenario: &str) -> (Fixture, World, ScenarioInput) {
    let case = fixture::named(name);
    let input = case.scenario(scenario).clone();
    let world = world(&case, &input.unretained);
    (case, world, input)
}

fn refs(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname) %(objectname)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn a_candidate_moves_no_ref_and_is_retained_with_its_recipe() {
    let (_case, mut world, input) = case_world("FX01", "compose-all");
    let before = refs(&world.repo);
    let composition = run(&mut world, &input, &input.request.compose);
    assert_eq!(refs(&world.repo), before, "the composer moved a ref");

    let candidate = composition.outcome.candidate().expect("a candidate");
    assert_eq!(
        candidate.producer,
        Producer::Composer {
            profile: PROVISIONAL_TEXT_PROFILE.into()
        }
    );
    assert_eq!(candidate.mode.code(), "text");
    assert_eq!(candidate.baseline_source, "retained");
    assert_eq!(candidate.inputs.len(), 3);
    assert!(
        candidate
            .inputs
            .iter()
            .all(|input| input.base_snapshot.is_some() && input.result_snapshot.is_some())
    );
    let retained = collaboration_archive::retained(&world.store, &candidate.subject)
        .unwrap()
        .expect("the candidate is retained");
    assert_eq!(retained.commit, candidate.commit);

    let recipe = composition.recipe.as_ref().unwrap();
    let record = composer::read_recipe(&world.store, recipe).unwrap();
    assert_eq!(record.id(), recipe.id);
    let json = String::from_utf8(record.canonical_bytes().to_vec()).unwrap();
    for needle in [
        PROVISIONAL_TEXT_PROFILE,
        candidate.subject.as_str(),
        world.trees["S0"].as_str(),
        "git merge-file",
        "\"label\"",
        "\"a11y\"",
        "\"child\"",
    ] {
        assert!(json.contains(needle), "recipe lacks {needle}: {json}");
    }

    // The same composition again is the same commit and the same recipe.
    let again = run(&mut world, &input, &input.request.compose);
    let again_candidate = again.outcome.candidate().unwrap();
    assert_eq!(again_candidate.commit, candidate.commit);
    assert_eq!(again.recipe.unwrap().id, recipe.id);
}

#[test]
fn duplicate_delivery_is_the_same_candidate_as_one_delivery() {
    let (_case, mut world, input) = case_world("FX03", "duplicate-delivery");
    let repeated = run(&mut world, &input, &input.orders[0]);
    let once = run(&mut world, &input, &input.request.compose);
    assert_eq!(
        repeated.outcome.candidate().unwrap().subject,
        once.outcome.candidate().unwrap().subject
    );
    assert_eq!(repeated.order, ["c1", "c2"]);
}

#[test]
fn an_inherited_contribution_goes_first_whatever_the_delivery_order() {
    let (_case, mut world, input) = case_world("FX03", "inherited");
    let composition = run(&mut world, &input, &["c2".into(), "c1".into()]);
    assert_eq!(composition.order, ["c1", "c2"]);
    assert_eq!(composition.outcome.code(), "candidate");
}

#[test]
fn recomposition_never_relabels_the_synthesized_result() {
    let (case, mut world, input) = case_world("FX07", "a-without-b-retained");
    let composition = run(&mut world, &input, &[]);
    let candidate = composition.outcome.candidate().expect("a recomposition");
    let x = world
        .catalog
        .iter()
        .find(|spec| spec.id == "x-synthesis")
        .unwrap();
    let x = collaboration_archive::retained_contribution(&world.store, &x.lineage)
        .unwrap()
        .unwrap();
    assert_ne!(candidate.subject, x.result.snapshot_id);
    assert_eq!(composition.order, ["a-aria"]);
    let json = String::from_utf8(
        composer::read_recipe(&world.store, composition.recipe.as_ref().unwrap())
            .unwrap()
            .canonical_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(json.contains("\"recomposition\""), "{json}");
    let dir = world.root.path().join("recomposed");
    collaboration_archive::reconstruct(&world.store, &candidate.subject, &dir).unwrap();
    assert!(case.judge(&input.id, Seen::Candidate(&dir)).0);
}

#[test]
fn each_budget_limit_refuses_instead_of_a_partial_candidate() {
    let (_case, mut world, input) = case_world("FX01", "compose-all");
    let limits = [
        CompositionBudget {
            max_contributions: 2,
            ..CompositionBudget::default()
        },
        CompositionBudget {
            max_merges: 0,
            ..CompositionBudget::default()
        },
        CompositionBudget {
            max_paths: 1,
            ..CompositionBudget::default()
        },
        CompositionBudget {
            max_bytes: 64,
            ..CompositionBudget::default()
        },
    ];
    for budget in limits {
        let mut request = request(&world, &input, &input.request.compose);
        request.budget = budget;
        let composition = composer::compose(&mut world.store, &world.repo, &request).unwrap();
        assert_eq!(composition.outcome.code(), "budget_exhausted", "{budget:?}");
        assert!(composition.recipe.is_none());
    }
}

#[test]
fn an_input_the_archive_lacks_is_missing_not_guessed() {
    let (_case, mut world, input) = case_world("FX01", "compose-all");
    let mut request = request(&world, &input, &input.request.compose);
    // A lineage this archive never retained.
    request.catalog[1].lineage =
        aethyme_contracts::experimental_v0::RecordId::parse(&format!("sha256:{}", "1".repeat(64)))
            .unwrap();
    let composition = composer::compose(&mut world.store, &world.repo, &request).unwrap();
    assert_eq!(composition.outcome.code(), "missing_input");
}

#[test]
fn held_out_cases_never_yield_a_failing_candidate() {
    let mut table = Vec::new();
    for case in fixture::held_out() {
        for input in &case.scenarios {
            let mut world = world(&case, &input.unretained);
            for order in orders(input) {
                let composition = run(&mut world, input, &order);
                let verdict = judge(&world, &case, &input.id, &composition);
                table.push(format!(
                    "{} {} {:?}: {} accepted={} {:?}",
                    case.name,
                    input.id,
                    order,
                    composition.outcome.code(),
                    verdict.0,
                    verdict.1
                ));
                if composition.outcome.candidate().is_some() {
                    assert!(verdict.0, "{}", table.last().unwrap());
                }
            }
        }
    }
    eprintln!("{}", table.join("\n"));
}

// ------------------------------------------- path rules beyond the fixtures

/// A repository with one commit per tree, each a map of path to (mode,
/// content), parented as given.
struct Adhoc {
    _root: tempfile::TempDir,
    store: CollaborationStore,
    repo: PathBuf,
}

fn git_in(repo: &Path, args: &[&str], input: Option<&[u8]>) -> String {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.unwrap_or_default())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

impl Adhoc {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q"], None);
        let store = open_store(&root.path().join("state"), &repo);
        Self {
            _root: root,
            store,
            repo,
        }
    }

    fn commit(&self, files: &[(&str, &str, &[u8])], parent: Option<&str>) -> String {
        let mut listing = Vec::new();
        for (path, mode, content) in files {
            let oid = git_in(&self.repo, &["hash-object", "-w", "--stdin"], Some(content));
            listing.extend_from_slice(format!("{mode} blob {oid}\t{path}\n").as_bytes());
        }
        let tree = git_in(&self.repo, &["mktree"], Some(&listing));
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "t"];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        git_in(&self.repo, &args, None)
    }

    fn capture(&mut self, id: &str, base: &str, result: &str) -> ContributionSpec {
        let request = CaptureRequest {
            operation_id: OperationId::mint().unwrap(),
            repository: self.repo.clone(),
            base: CommitOid::parse(base).unwrap(),
            result: CommitOid::parse(result).unwrap(),
            policy: CapturePolicy::Advisory,
            retention: RetentionBoundary::UntilReleased,
        };
        let CaptureOutcome::Acknowledged(receipt) = capture(&mut self.store, &request).unwrap()
        else {
            panic!("incomplete capture");
        };
        ContributionSpec {
            id: id.into(),
            lineage: receipt.contribution,
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        }
    }

    fn compose(&mut self, baseline: &str, catalog: Vec<ContributionSpec>) -> Composition {
        let baseline = collaboration_archive::snapshot_of_commit(
            &self.repo,
            &CommitOid::parse(baseline).unwrap(),
        )
        .unwrap()
        .id();
        let deliveries = catalog.iter().map(|spec| spec.id.clone()).collect();
        composer::compose(
            &mut self.store,
            &self.repo,
            &CompositionRequest {
                baseline,
                catalog,
                deliveries,
                budget: CompositionBudget::default(),
            },
        )
        .unwrap()
    }
}

fn reasons(composition: &Composition) -> Vec<ConflictReason> {
    match &composition.outcome {
        CompositionOutcome::Conflict { conflicts, .. } => {
            conflicts.iter().map(|conflict| conflict.reason).collect()
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn each_path_conflict_has_its_reason_and_one_side_changes_apply() {
    const R: &str = "100644";
    const X: &str = "100755";
    let mut world = Adhoc::new();
    let base = world.commit(
        &[
            ("doc.txt", R, b"one\ntwo\nthree\n"),
            ("tool.sh", R, b"echo hi\n"),
            ("image.bin", R, b"\0base"),
        ],
        None,
    );
    let edit = |world: &Adhoc, files: &[(&str, &str, &[u8])]| world.commit(files, Some(&base));

    // Delete versus modify.
    let deleted = edit(
        &world,
        &[("tool.sh", R, b"echo hi\n"), ("image.bin", R, b"\0base")],
    );
    let modified = edit(
        &world,
        &[
            ("doc.txt", R, b"one\nTWO\nthree\n"),
            ("tool.sh", R, b"echo hi\n"),
            ("image.bin", R, b"\0base"),
        ],
    );
    let catalog = vec![
        world.capture("deleted", &base, &deleted),
        world.capture("modified", &base, &modified),
    ];
    assert_eq!(
        reasons(&world.compose(&base, catalog)),
        [ConflictReason::DeleteModify]
    );

    // Add versus add, with different content.
    let mut files = vec![
        ("doc.txt", R, b"one\ntwo\nthree\n" as &[u8]),
        ("tool.sh", R, b"echo hi\n"),
        ("image.bin", R, b"\0base"),
    ];
    files.push(("new.txt", R, b"left\n"));
    let left = edit(&world, &files);
    files.pop();
    files.push(("new.txt", R, b"right\n"));
    let right = edit(&world, &files);
    let catalog = vec![
        world.capture("left", &base, &left),
        world.capture("right", &base, &right),
    ];
    assert_eq!(
        reasons(&world.compose(&base, catalog)),
        [ConflictReason::AddAdd]
    );

    // Two different binary replacements.
    let ours = edit(
        &world,
        &[
            ("doc.txt", R, b"one\ntwo\nthree\n"),
            ("tool.sh", R, b"echo hi\n"),
            ("image.bin", R, b"\0ours"),
        ],
    );
    let theirs = edit(
        &world,
        &[
            ("doc.txt", R, b"one\ntwo\nthree\n"),
            ("tool.sh", R, b"echo hi\n"),
            ("image.bin", R, b"\0theirs"),
        ],
    );
    let catalog = vec![
        world.capture("ours", &base, &ours),
        world.capture("theirs", &base, &theirs),
    ];
    assert_eq!(
        reasons(&world.compose(&base, catalog)),
        [ConflictReason::Binary]
    );

    // A mode change on one side and a content change on the other compose.
    let executable = edit(
        &world,
        &[
            ("doc.txt", R, b"one\ntwo\nthree\n"),
            ("tool.sh", X, b"echo hi\n"),
            ("image.bin", R, b"\0base"),
        ],
    );
    let reworded = edit(
        &world,
        &[
            ("doc.txt", R, b"one\ntwo\nthree\n"),
            ("tool.sh", R, b"echo hello\n"),
            ("image.bin", R, b"\0base"),
        ],
    );
    let catalog = vec![
        world.capture("executable", &base, &executable),
        world.capture("reworded", &base, &reworded),
    ];
    let composition = world.compose(&base, catalog);
    let candidate = composition.outcome.candidate().expect("a candidate");
    let listing = git_in(
        &world.repo,
        &["ls-tree", candidate.commit.as_str(), "tool.sh"],
        None,
    );
    assert!(listing.starts_with("100755 "), "{listing}");
    let content = git_in(
        &world.repo,
        &["show", &format!("{}:tool.sh", candidate.commit.as_str())],
        None,
    );
    assert_eq!(content, "echo hello");

    // A contribution and its revert, built on it, leave the baseline as
    // it was; a contribution based on something else is unknown.
    let reverted = world.commit(
        &[
            ("doc.txt", R, b"one\ntwo\nthree\n"),
            ("tool.sh", R, b"echo hi\n"),
            ("image.bin", R, b"\0base"),
        ],
        Some(&modified),
    );
    let catalog = vec![
        world.capture("modified", &base, &modified),
        world.capture("revert", &modified, &reverted),
    ];
    assert_eq!(world.compose(&base, catalog).outcome.code(), "no_change");
    let catalog = vec![world.capture("modified", &base, &modified)];
    assert_eq!(
        world.compose(&modified, catalog).outcome.code(),
        "unknown_base"
    );
}
