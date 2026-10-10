//! The #664 provisional composer, first against the provisional fixtures
//! (#733), then on small repositories of its own for rules the fixtures do
//! not isolate.
//!
//! Fixture runs use only the fixtures' composer-facing API: a case's
//! `ScenarioInput` and `ContributionInput` metadata, and the repository
//! `CaseInput::materialize` writes. Each contribution is captured through
//! the #658 capture API, composed from the archive alone, rebuilt with
//! `reconstruct`, and judged by `judge_scenario` over every declared order
//! run twice. Nothing here reads the answer key or branches on a case or
//! scenario id. Held-out cases run last, as a report, and nothing was tuned
//! on them.

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
use aethyme_contracts::experimental_v0::{RecordId, SourceSnapshotId};
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

fn snapshot(repo: &Path, commit: &str) -> SourceSnapshotId {
    collaboration_archive::snapshot_of_commit(repo, &CommitOid::parse(commit).unwrap())
        .unwrap()
        .id()
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

// ------------------------------------------------------------ fixtures

struct World {
    root: tempfile::TempDir,
    store: CollaborationStore,
    repo: PathBuf,
    catalog: Vec<ContributionSpec>,
    baseline: CommitOid,
    accepted: Vec<CommitOid>,
}

/// One scenario's repository with every contribution it retains captured.
/// A contribution the scenario does not retain is known by id only.
fn world(case: &CaseInput, input: &ScenarioInput) -> World {
    let root = tempfile::tempdir().unwrap();
    let materialized = case.materialize(&input.id, &root.path().join("repos"));
    let repo = materialized.repo.clone();
    let mut store = open_store(&root.path().join("state"), &repo);
    let catalog = case
        .contributions
        .iter()
        .map(|contribution| ContributionSpec {
            id: contribution.id.clone(),
            lineage: materialized
                .contributions
                .get(&contribution.id)
                .map(|commits| capture_lineage(&mut store, &repo, &commits.base, &commits.result)),
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
    }
}

fn run(world: &mut World, input: &ScenarioInput, order: &[String]) -> Composition {
    let request = CompositionRequest {
        baseline: world.baseline.clone(),
        accepted: world.accepted.clone(),
        catalog: world.catalog.clone(),
        deliveries: order.to_vec(),
        budget: CompositionBudget::default(),
    };
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

/// Run every declared order twice (a subtraction twice with no order) and
/// judge the scenario as a whole. Returns the verdict and one line per run.
fn judge(case: &CaseInput, input: &ScenarioInput) -> (fx::Verdict, Vec<String>) {
    let mut world = world(case, input);
    let orders = if input.orders.is_empty() {
        vec![Vec::new()]
    } else {
        input.orders.clone()
    };
    let mut seen: Vec<(Vec<String>, Result<PathBuf, Outcome>)> = Vec::new();
    let mut lines = Vec::new();
    for order in &orders {
        for _ in 0..2 {
            let composition = run(&mut world, input, order);
            assert_eq!(
                composition.outcome.candidate().is_some(),
                composition.recipe.is_some(),
                "a recipe exactly when there is a candidate"
            );
            lines.push(format!("{order:?}: {}", composition.outcome.code()));
            let observed = match &composition.outcome {
                CompositionOutcome::Candidate(candidate) => {
                    let dir = world.root.path().join(format!("out-{}", seen.len()));
                    collaboration_archive::reconstruct(&world.store, &candidate.subject, &dir)
                        .unwrap();
                    Ok(dir)
                }
                other => Err(outcome_of(other.code())),
            };
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

fn report(
    case: &CaseInput,
    input: &ScenarioInput,
    verdict: &fx::Verdict,
    lines: &[String],
) -> String {
    format!(
        "{} {}: accepted={} {}\n  {}",
        case.case,
        input.id,
        verdict.accepted,
        verdict.failures.join("; "),
        lines.join("\n  ")
    )
}

/// Scenarios the provisional profile meets, measured when it was frozen.
/// The text profile cannot meet every case (plan §7.6: report the gap, do
/// not widen the contract to hide it); this floor only stops regressions.
/// `docs/architecture/local-v3-l4-composer.md` records which fall short.
const ACCEPTED_FLOOR: usize = 15;

#[test]
fn the_provisional_cases_against_the_oracle() {
    let mut accepted = 0;
    let mut total = 0;
    let mut lines = Vec::new();
    for case in fx::cases() {
        for input in &case.scenarios {
            let (verdict, runs) = judge(&case, input);
            total += 1;
            accepted += usize::from(verdict.accepted);
            lines.push(report(&case, input, &verdict, &runs));
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
            lines.push(report(&case, input, &verdict, &runs));
        }
    }
    eprintln!("{}", lines.join("\n"));
}

// ------------------------------------------------- repositories of our own

const R: &str = "100644";
const X: &str = "100755";

type Files<'a> = Vec<(&'a str, &'a str, &'a [u8])>;

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

struct Adhoc {
    _root: tempfile::TempDir,
    store: CollaborationStore,
    repo: PathBuf,
    catalog: Vec<ContributionSpec>,
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
            catalog: Vec::new(),
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

    /// Capture `base..result` and add it to the catalog as `id`.
    fn add(&mut self, id: &str, base: &str, result: &str) -> &mut ContributionSpec {
        let lineage = capture_lineage(&mut self.store, &self.repo, base, result);
        self.catalog.push(ContributionSpec {
            id: id.into(),
            lineage: Some(lineage),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        });
        self.catalog.last_mut().unwrap()
    }

    fn spec(&mut self, id: &str) -> &mut ContributionSpec {
        self.catalog.iter_mut().find(|spec| spec.id == id).unwrap()
    }

    fn request(&self, baseline: &str, deliveries: &[&str]) -> CompositionRequest {
        CompositionRequest {
            baseline: CommitOid::parse(baseline).unwrap(),
            accepted: Vec::new(),
            catalog: self.catalog.clone(),
            deliveries: deliveries.iter().map(|id| id.to_string()).collect(),
            budget: CompositionBudget::default(),
        }
    }

    fn compose(&mut self, request: &CompositionRequest) -> Composition {
        composer::compose(&mut self.store, &self.repo, request).unwrap()
    }

    fn show(&self, commit: &CommitOid, path: &str) -> String {
        git_in(
            &self.repo,
            &["show", &format!("{}:{path}", commit.as_str())],
            None,
        )
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

/// Three files on one base, and contributions `a`, `b`, `c` that each
/// change one of them.
fn three_files() -> (Adhoc, String) {
    let mut world = Adhoc::new();
    let original: Files = vec![
        ("a.txt", R, b"a1\na2\na3\n"),
        ("b.txt", R, b"b1\nb2\nb3\n"),
        ("c.txt", R, b"c1\nc2\nc3\n"),
    ];
    let base = world.commit(&original, None);
    for (id, file, content) in [
        ("a", "a.txt", b"a1\nA2\na3\n" as &[u8]),
        ("b", "b.txt", b"b1\nB2\nb3\n"),
        ("c", "c.txt", b"c1\nC2\nc3\n"),
    ] {
        let mut files = original.clone();
        for entry in &mut files {
            if entry.0 == file {
                entry.2 = content;
            }
        }
        let result = world.commit(&files, Some(&base));
        world.add(id, &base, &result);
    }
    (world, base)
}

#[test]
fn a_candidate_moves_no_ref_and_is_retained_with_its_recipe() {
    let (mut world, base) = three_files();
    let refs = |repo: &Path| git_in(repo, &["for-each-ref"], None);
    let before = refs(&world.repo);
    let request = world.request(&base, &["a", "b", "c"]);
    let composition = world.compose(&request);
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
    for (path, expected) in [
        ("a.txt", "a1\nA2\na3"),
        ("b.txt", "b1\nB2\nb3"),
        ("c.txt", "c1\nC2\nc3"),
    ] {
        assert_eq!(world.show(&candidate.commit, path), expected);
    }
    let retained = collaboration_archive::retained(&world.store, &candidate.subject)
        .unwrap()
        .expect("the candidate is retained");
    assert_eq!(retained.commit, candidate.commit);

    let recipe = composition.recipe.as_ref().unwrap();
    let record = composer::read_recipe(&world.store, recipe).unwrap();
    let json = String::from_utf8(record.canonical_bytes()).unwrap();
    for needle in [
        PROVISIONAL_TEXT_PROFILE,
        candidate.subject.as_str(),
        snapshot(&world.repo, &base).as_str(),
        "git merge-file",
        "\"a\"",
        "\"b\"",
        "\"c\"",
    ] {
        assert!(json.contains(needle), "recipe lacks {needle}: {json}");
    }

    // The same composition again is the same commit and recipe.
    let again = world.compose(&request);
    assert_eq!(again.outcome.candidate().unwrap().commit, candidate.commit);
    assert_eq!(again.recipe.unwrap().id, recipe.id);
}

#[test]
fn a_repeat_by_name_or_by_lineage_applies_once() {
    let (mut world, base) = three_files();
    let lineage = world.spec("a").lineage.clone();
    world.catalog.push(ContributionSpec {
        id: "a-again".into(),
        lineage,
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    });
    let once = world.compose(&world.request(&base, &["a", "b"]));
    for deliveries in [&["a", "a", "b", "a"][..], &["a", "a-again", "b", "a-again"]] {
        let repeated = world.compose(&world.request(&base, deliveries));
        assert_eq!(repeated.order, ["a", "b"], "{deliveries:?}");
        assert_eq!(
            repeated.outcome.candidate().unwrap().subject,
            once.outcome.candidate().unwrap().subject
        );
    }
}

#[test]
fn an_inherited_contribution_goes_first_and_applies_only_its_own_change() {
    let mut world = Adhoc::new();
    let base = world.commit(&[("f.txt", R, b"1\n2\n3\n4\n5\n")], None);
    let first = world.commit(&[("f.txt", R, b"ONE\n2\n3\n4\n5\n")], Some(&base));
    // Built on `first`: keeps its change and adds one of its own.
    let second = world.commit(&[("f.txt", R, b"ONE\n2\n3\n4\nFIVE\n")], Some(&first));
    world.add("first", &base, &first);
    world.add("second", &first, &second);
    let composition = world.compose(&world.request(&base, &["second", "first"]));
    assert_eq!(composition.order, ["first", "second"]);
    let candidate = composition.outcome.candidate().unwrap();
    assert_eq!(world.show(&candidate.commit, "f.txt"), "ONE\n2\n3\n4\nFIVE");

    // Without `first`, `second`'s base is a known but unselected result:
    // missing, not guessed.
    let alone = world.compose(&world.request(&base, &["second"]));
    assert_eq!(alone.outcome.code(), "missing_input");
}

#[test]
fn a_base_in_accepted_history_applies_onto_a_later_baseline() {
    let mut world = Adhoc::new();
    let old = world.commit(&[("f.txt", R, b"1\n2\n3\n4\n5\n")], None);
    let accepted = world.commit(&[("f.txt", R, b"1\n2\n3\n4\nFIVE\n")], Some(&old));
    let change = world.commit(&[("f.txt", R, b"ONE\n2\n3\n4\n5\n")], Some(&old));
    world.add("change", &old, &change);

    let mut request = world.request(&accepted, &["change"]);
    assert_eq!(world.compose(&request).outcome.code(), "unknown_base");
    request.accepted = vec![CommitOid::parse(&old).unwrap()];
    let composition = world.compose(&request);
    let candidate = composition.outcome.candidate().unwrap();
    assert_eq!(world.show(&candidate.commit, "f.txt"), "ONE\n2\n3\n4\nFIVE");
}

#[test]
fn identical_twins_conflict_instead_of_a_silent_wrong_merge() {
    // Two identical cards. One side pins the first card (moved, deeper);
    // the other edits the first card in place. The line diff of the move
    // deletes the *second* card, so a line merge puts the edit on the card
    // that stayed, without a conflict.
    let card = |indent: &str, button: &str| {
        format!(
            "{indent}<article class=\"card\">\n{indent}  <h3>Tips</h3>\n{indent}  <button class=\"more\"{button}>More</button>\n{indent}</article>\n"
        )
    };
    let page = |pinned: &str, first: &str, second: &str| {
        format!(
            "<main id=\"app\">\n  <aside id=\"sidebar\">\n    <h2>Pinned</h2>\n    <div id=\"pinned\">\n{pinned}    </div>\n  </aside>\n  <section id=\"content\">\n{first}{second}  </section>\n</main>\n"
        )
    };
    let plain = card("    ", "");
    let base_page = page("", &plain, &plain);
    let moved_page = page(&card("      ", ""), &plain, "");
    let edited_page = page("", &card("    ", " onkeydown=\"go()\""), &plain);
    let mut world = Adhoc::new();
    let base = world.commit(&[("page.html", R, base_page.as_bytes())], None);
    let mover = world.commit(&[("page.html", R, moved_page.as_bytes())], Some(&base));
    let editor = world.commit(&[("page.html", R, edited_page.as_bytes())], Some(&base));
    world.add("move", &base, &mover);
    world.add("edit", &base, &editor);
    for order in [["move", "edit"], ["edit", "move"]] {
        let composition = world.compose(&world.request(&base, &order));
        assert_eq!(
            reasons(&composition),
            [ConflictReason::MovedBlock],
            "{order:?}"
        );
    }
}

#[test]
fn each_path_conflict_has_its_reason_and_one_side_changes_apply() {
    let mut world = Adhoc::new();
    let original: Files = vec![
        ("doc.txt", R, b"one\ntwo\nthree\n"),
        ("tool.sh", R, b"echo hi\n"),
        ("image.bin", R, b"\0base"),
    ];
    let base = world.commit(&original, None);
    // An empty content deletes the path.
    let with = |world: &Adhoc, changes: &[(&str, &str, &[u8])], parent: &str| {
        let mut files = original.clone();
        for change in changes {
            match files.iter_mut().find(|entry| entry.0 == change.0) {
                Some(entry) => *entry = *change,
                None => files.push(*change),
            }
        }
        files.retain(|entry| !entry.2.is_empty());
        world.commit(&files, Some(parent))
    };
    let pair = |world: &mut Adhoc, left: String, right: String| {
        world.catalog.clear();
        world.add("left", &base, &left);
        world.add("right", &base, &right);
        let request = world.request(&base, &["left", "right"]);
        world.compose(&request)
    };

    let deleted = with(&world, &[("doc.txt", R, b"")], &base);
    let modified = with(&world, &[("doc.txt", R, b"one\nTWO\nthree\n")], &base);
    assert_eq!(
        reasons(&pair(&mut world, deleted, modified.clone())),
        [ConflictReason::DeleteModify]
    );
    let left = with(&world, &[("new.txt", R, b"left\n")], &base);
    let right = with(&world, &[("new.txt", R, b"right\n")], &base);
    assert_eq!(
        reasons(&pair(&mut world, left, right)),
        [ConflictReason::AddAdd]
    );
    let ours = with(&world, &[("image.bin", R, b"\0ours")], &base);
    let theirs = with(&world, &[("image.bin", R, b"\0theirs")], &base);
    assert_eq!(
        reasons(&pair(&mut world, ours, theirs)),
        [ConflictReason::Binary]
    );

    // A mode change and a content change of one file compose.
    let executable = with(&world, &[("tool.sh", X, b"echo hi\n")], &base);
    let reworded = with(&world, &[("tool.sh", R, b"echo hello\n")], &base);
    let composition = pair(&mut world, executable, reworded);
    let candidate = composition.outcome.candidate().expect("a candidate");
    let listing = git_in(
        &world.repo,
        &["ls-tree", candidate.commit.as_str(), "tool.sh"],
        None,
    );
    assert!(listing.starts_with("100755 "), "{listing}");
    assert_eq!(world.show(&candidate.commit, "tool.sh"), "echo hello");

    // A contribution and its revert, built on it, change nothing.
    let reverted = with(&world, &[], &modified);
    world.catalog.clear();
    world.add("modified", &base, &modified);
    world.add("revert", &modified, &reverted);
    let request = world.request(&base, &["modified", "revert"]);
    assert_eq!(world.compose(&request).outcome.code(), "no_change");
}

#[test]
fn planning_refusals_name_the_rule() {
    let (mut world, base) = three_files();
    let code = |world: &mut Adhoc, setup: &dyn Fn(&mut Adhoc), deliveries: &[&str]| {
        let saved = world.catalog.clone();
        setup(world);
        let request = world.request(&base, deliveries);
        let code = world.compose(&request).outcome.code();
        world.catalog = saved;
        code
    };
    let none = |_: &mut Adhoc| {};
    assert_eq!(code(&mut world, &none, &["a", "nope"]), "missing_input");
    let revision = |world: &mut Adhoc| world.spec("b").revision_of = Some("a".into());
    assert_eq!(
        code(&mut world, &revision, &["a", "b"]),
        "competing_revisions"
    );
    let pinned = |world: &mut Adhoc| {
        world.spec("b").revision_of = Some("a".into());
        world.spec("c").requires = vec!["a".into()];
    };
    assert_eq!(
        code(&mut world, &pinned, &["b", "c"]),
        "competing_revisions"
    );
    let requires = |world: &mut Adhoc| world.spec("c").requires = vec!["a".into()];
    assert_eq!(code(&mut world, &requires, &["c"]), "missing_input");
    assert_eq!(code(&mut world, &requires, &["c", "a"]), "candidate");
    let group = |world: &mut Adhoc| {
        world.spec("a").atomic_group = Some("g".into());
        world.spec("b").atomic_group = Some("g".into());
    };
    assert_eq!(code(&mut world, &group, &["a"]), "missing_input");
    assert_eq!(code(&mut world, &group, &["a", "b"]), "candidate");
    let cycle = |world: &mut Adhoc| {
        world.spec("a").requires = vec!["b".into()];
        world.spec("b").requires = vec!["a".into()];
    };
    assert_eq!(code(&mut world, &cycle, &["a", "b"]), "dependency_cycle");
    let synthesized = |world: &mut Adhoc| world.spec("c").derived_from = vec!["a".into()];
    assert_eq!(
        code(&mut world, &synthesized, &["a", "c"]),
        "competing_revisions"
    );
    let unretained = |world: &mut Adhoc| world.spec("a").lineage = None;
    assert_eq!(code(&mut world, &unretained, &["a"]), "missing_input");
    let unknown = |world: &mut Adhoc| {
        world.spec("a").lineage =
            Some(RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap())
    };
    assert_eq!(code(&mut world, &unknown, &["a"]), "missing_input");
}

#[test]
fn recomposition_never_relabels_the_synthesized_result() {
    let mut world = Adhoc::new();
    let base = world.commit(&[("f.txt", R, b"1\n2\n3\n4\n5\n")], None);
    let a = world.commit(&[("f.txt", R, b"ONE\n2\n3\n4\n5\n")], Some(&base));
    let b = world.commit(&[("f.txt", R, b"1\n2\n3\n4\nFIVE\n")], Some(&base));
    let x = world.commit(&[("f.txt", R, b"ONE\n2\nthree\n4\nFIVE\n")], Some(&base));
    let d = world.commit(&[("f.txt", R, b"ONE\n2\nthree\nFOUR\nFIVE\n")], Some(&x));
    world.add("a", &base, &a);
    world.add("b", &base, &b);
    world.add("x", &base, &x).derived_from = vec!["a".into(), "b".into()];
    world.add("d", &x, &d);
    let subtract = |world: &mut Adhoc, keep: &[&str]| {
        let request = world.request(&base, &[]);
        composer::recompose_without(
            &mut world.store,
            &world.repo,
            &request,
            &Subtraction {
                from: "x".into(),
                remove: vec!["b".into()],
                keep: keep.iter().map(|id| id.to_string()).collect(),
            },
        )
        .unwrap()
    };

    let composition = subtract(&mut world, &[]);
    let candidate = composition.outcome.candidate().expect("a recomposition");
    assert_eq!(composition.order, ["a"]);
    assert_ne!(candidate.subject, snapshot(&world.repo, &x));
    assert_eq!(world.show(&candidate.commit, "f.txt"), "ONE\n2\n3\n4\n5");
    let json = String::from_utf8(
        composer::read_recipe(&world.store, composition.recipe.as_ref().unwrap())
            .unwrap()
            .canonical_bytes(),
    )
    .unwrap();
    assert!(json.contains("\"recomposition\""), "{json}");

    // `d` was built on x's result.
    assert_eq!(
        subtract(&mut world, &["d"]).outcome.code(),
        "inseparable_selection"
    );
    // ... or, built on the original base, requires x outright.
    let e = world.commit(&[("f.txt", R, b"1\n2\n3\nfour\n5\n")], Some(&base));
    world.add("e", &base, &e).requires = vec!["x".into()];
    assert_eq!(
        subtract(&mut world, &["e"]).outcome.code(),
        "inseparable_selection"
    );
    // A constituent that is no longer retained.
    world.spec("a").lineage = None;
    assert_eq!(
        subtract(&mut world, &[]).outcome.code(),
        "inseparable_selection"
    );
}

#[test]
fn each_budget_limit_refuses_instead_of_a_partial_candidate() {
    let mut world = Adhoc::new();
    let base = world.commit(&[("f.txt", R, b"1\n2\n3\n4\n5\n")], None);
    let a = world.commit(&[("f.txt", R, b"ONE\n2\n3\n4\n5\n")], Some(&base));
    let b = world.commit(&[("f.txt", R, b"1\n2\n3\n4\nFIVE\n")], Some(&base));
    world.add("a", &base, &a);
    world.add("b", &base, &b);
    let mut request = world.request(&base, &["a", "b"]);
    assert_eq!(world.compose(&request).outcome.code(), "candidate");
    for budget in [
        CompositionBudget {
            max_contributions: 1,
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
    ] {
        request.budget = budget;
        let composition = world.compose(&request);
        assert_eq!(composition.outcome.code(), "budget_exhausted", "{budget:?}");
        assert!(composition.recipe.is_none());
    }
}
