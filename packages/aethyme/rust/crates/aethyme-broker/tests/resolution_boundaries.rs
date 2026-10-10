//! #665's boundaries on small repositories of their own: the regressions
//! from the independent review of #736 (F1-F6), each once a probe that a
//! forged request, a protected path under another spelling, a reordered or
//! padded decision, a dropped change or a cosmetic last writer got through.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use aethyme_broker::collaboration_archive::CommitOid;
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary, capture,
};
use aethyme_broker::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, forbidden_roots,
};
use aethyme_broker::composer::{self, CompositionBudget, CompositionRequest, ContributionSpec};
use aethyme_broker::composition::Producer;
use aethyme_broker::resolution::{
    self, FailedCheck, Preference, Proposal, ProposedFile, RequirementRef, Resolution,
    ResolutionInputs, ResolutionOutcome, ResolutionRequest, Resolver, ResolverIdentity, Response,
};
use aethyme_contracts::experimental_v0::EntryKind;

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

struct World {
    root: tempfile::TempDir,
    store: CollaborationStore,
    repo: PathBuf,
    catalog: Vec<ContributionSpec>,
    baseline: CommitOid,
}

type Files<'a> = &'a [(&'a str, Option<&'a str>)];

fn put(repo: &Path, files: Files<'_>) {
    for (path, content) in files {
        let full = repo.join(path);
        match content {
            Some(content) => {
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, content).unwrap();
                git(repo, &["add", "--", path]);
            }
            None => {
                git(repo, &["rm", "-q", "--", path]);
            }
        }
    }
}

fn capture_contribution(world: &mut World, id: &str, result: &str) {
    let request = CaptureRequest {
        operation_id: OperationId::mint().unwrap(),
        repository: world.repo.clone(),
        base: world.baseline.clone(),
        result: CommitOid::parse(result).unwrap(),
        policy: CapturePolicy::Advisory,
        retention: RetentionBoundary::UntilReleased,
    };
    let CaptureOutcome::Acknowledged(receipt) = capture(&mut world.store, &request).unwrap() else {
        panic!("capture of {id}");
    };
    world.catalog.push(ContributionSpec {
        id: id.to_string(),
        lineage: Some(receipt.contribution),
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    });
}

fn world(base: Files<'_>, contributions: &[(&str, Files<'_>)]) -> World {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "core.ignorecase", "false"]);
    put(&repo, base);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let baseline = git(&repo, &["rev-parse", "HEAD"]);
    let store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(&root.path().join("state")),
        &ProjectKey::parse("boundaries").unwrap(),
        &forbidden_roots(&repo),
    )
    .unwrap();
    let mut world = World {
        root,
        store,
        repo: repo.clone(),
        catalog: Vec::new(),
        baseline: CommitOid::parse(&baseline).unwrap(),
    };
    for (id, files) in contributions {
        git(&repo, &["checkout", "-q", "--detach", &baseline]);
        put(&repo, files);
        git(&repo, &["commit", "-q", "-m", id]);
        let result = git(&repo, &["rev-parse", "HEAD"]);
        capture_contribution(&mut world, id, &result);
    }
    git(&repo, &["checkout", "-q", "--detach", &baseline]);
    world
}

fn composition_request(world: &World, order: &[&str]) -> CompositionRequest {
    CompositionRequest {
        baseline: world.baseline.clone(),
        accepted: Vec::new(),
        catalog: world.catalog.clone(),
        deliveries: order.iter().map(|id| id.to_string()).collect(),
        budget: CompositionBudget::default(),
    }
}

fn opaque() -> Vec<FailedCheck> {
    vec![FailedCheck {
        check: "behavior".into(),
    }]
}

fn raise(
    world: &mut World,
    order: &[&str],
    checks: bool,
    requirements: &[RequirementRef],
    preference: Option<Preference>,
) -> ResolutionRequest {
    let request = composition_request(world, order);
    let composition = composer::compose(&mut world.store, &world.repo, &request).unwrap();
    resolution::request_resolution(
        &mut world.store,
        &world.repo,
        &request,
        &composition,
        &if checks { opaque() } else { Vec::new() },
        requirements,
        preference,
    )
    .unwrap_or_else(|error| panic!("{order:?} {}: {error}", composition.outcome.code()))
}

type Respond = dyn Fn(&ResolutionRequest, &ResolutionInputs<'_>) -> Result<Response, String>;

struct Fixed(Box<Respond>, Rc<Cell<u32>>);

impl Resolver for Fixed {
    fn identity(&self) -> ResolverIdentity {
        ResolverIdentity {
            name: "boundary".into(),
            version: "0".into(),
        }
    }

    fn propose(
        &mut self,
        request: &ResolutionRequest,
        inputs: &ResolutionInputs<'_>,
    ) -> Result<Response, String> {
        self.1.set(self.1.get() + 1);
        (self.0)(request, inputs)
    }
}

fn fixed(
    respond: impl Fn(&ResolutionRequest, &ResolutionInputs<'_>) -> Result<Response, String> + 'static,
) -> (Fixed, Rc<Cell<u32>>) {
    let calls = Rc::new(Cell::new(0));
    (Fixed(Box::new(respond), calls.clone()), calls)
}

fn writes(files: &[(&str, &str)]) -> Proposal {
    let mut proposal = Proposal::default();
    for (path, bytes) in files {
        proposal.writes.insert(
            path.to_string(),
            Some(ProposedFile {
                kind: EntryKind::Regular,
                bytes: bytes.as_bytes().to_vec(),
            }),
        );
    }
    proposal
}

fn proposing(files: &'static [(&'static str, &'static str)]) -> (Fixed, Rc<Cell<u32>>) {
    fixed(move |_, _| Ok(Response::Proposal(writes(files))))
}

fn declines() -> (Fixed, Rc<Cell<u32>>) {
    fixed(|_, _| Ok(Response::Unresolved("no".into())))
}

fn resolve(world: &mut World, request: &ResolutionRequest, resolver: &mut Fixed) -> Resolution {
    resolution::resolve(&mut world.store, &world.repo, request.record(), resolver).unwrap()
}

fn files_of(world: &World, resolution: &Resolution) -> Vec<(String, String)> {
    let ResolutionOutcome::Synthesized(synthesized) = &resolution.outcome else {
        panic!("{:?}", resolution.outcome);
    };
    let commit = synthesized.candidate.commit.as_str();
    git(&world.repo, &["ls-tree", "-r", "--name-only", commit])
        .lines()
        .map(|path| {
            (
                path.to_string(),
                git(&world.repo, &["show", &format!("{commit}:{path}")]),
            )
        })
        .collect()
}

const BASE: Files<'static> = &[
    ("a.txt", Some("one\ntwo\nthree\n")),
    ("b.txt", Some("b\n")),
    ("u.txt", Some("u\n")),
];

fn two_writers() -> World {
    world(
        BASE,
        &[
            ("A", &[("a.txt", Some("one\nA\nthree\n"))]),
            ("B", &[("a.txt", Some("one\nB\nthree\n"))]),
        ],
    )
}

// ------------------------------------------------------------------ F1

#[test]
fn a_request_is_rebuilt_from_its_record() {
    let mut world = two_writers();
    let raised = raise(&mut world, &["A", "B"], false, &[], None);
    let loaded = resolution::load_request(&mut world.store, &world.repo, raised.record()).unwrap();
    assert_eq!(loaded.decision_key(), raised.decision_key());
    assert_eq!(loaded.scope(), raised.scope());
    assert_eq!(loaded.protected(), raised.protected());
    assert_eq!(loaded.members(), raised.members());
    assert_eq!(loaded.contents(), raised.contents());
    assert_eq!(loaded.trigger(), raised.trigger());
    assert!(loaded.preference().is_none());
}

#[test]
fn the_same_decision_raised_again_shares_its_allowance() {
    let mut world = two_writers();
    let first = raise(&mut world, &["A", "B"], false, &[], None);
    for _ in 0..2 {
        let (mut resolver, _) = declines();
        resolve(&mut world, &first, &mut resolver);
    }
    // Another request, another order, a preference: the same decision.
    let again = raise(
        &mut world,
        &["B", "A"],
        false,
        &[],
        Some(Preference {
            authorized_by: "issuer".into(),
            prefer: "A".into(),
        }),
    );
    assert_ne!(again.record().id, first.record().id);
    assert_eq!(again.decision_key(), first.decision_key());
    assert_eq!(again.allowance().remaining(), 0);
    let (mut resolver, calls) = declines();
    let refused = resolve(&mut world, &again, &mut resolver);
    assert_eq!(refused.outcome.code(), "budget_exhausted");
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_preference_counts_only_when_the_request_was_raised_with_it() {
    let mut world = two_writers();
    let without = raise(&mut world, &["A", "B"], false, &[], None);
    let (mut last_writer, _) = proposing(&[("a.txt", "one\nB\nthree\n")]);
    assert_eq!(
        resolve(&mut world, &without, &mut last_writer)
            .outcome
            .code(),
        "unresolved"
    );
}

// ------------------------------------------------------------------ F2

#[test]
fn requirement_paths_are_protected_under_any_spelling() {
    let contribution: Files<'_> = &[
        ("a.txt", Some("one\nA\nthree\n")),
        ("pkg/check.sh", Some("exit 1\n")),
    ];
    for path in [
        "./pkg/check.sh",
        "pkg",
        "pkg/",
        "pkg//check.sh",
        "/pkg/check.sh",
    ] {
        let mut world = world(BASE, &[("A", contribution)]);
        let requirements = vec![RequirementRef {
            id: "harness".into(),
            path: Some(path.into()),
        }];
        let request = raise(&mut world, &["A"], true, &requirements, None);
        let (mut resolver, _) = proposing(&[("pkg/check.sh", "exit 0\n")]);
        assert_eq!(
            resolve(&mut world, &request, &mut resolver).outcome.code(),
            "scope_violation",
            "{path}"
        );
    }
}

#[test]
fn policy_toolchain_and_attribute_paths_are_protected_at_any_depth() {
    for path in [
        "pkg/.gitattributes",
        ".cargo/config.toml",
        "packages/aethyme/rust/.config/nextest.toml",
        "rust-toolchain.toml",
        "packages/aethyme/rust/Cargo.toml",
        ".AETHYME/gates.toml",
        "sub/.gitmodules",
        "crates/x/build.rs",
    ] {
        let contribution: Vec<(&str, Option<&str>)> = vec![(path, Some("v1\n"))];
        let mut world = world(BASE, &[("A", &contribution)]);
        let request = raise(&mut world, &["A"], true, &[], None);
        let path_owned = path.to_string();
        let (mut resolver, _) =
            fixed(move |_, _| Ok(Response::Proposal(writes(&[(path_owned.as_str(), "v2\n")]))));
        assert_eq!(
            resolve(&mut world, &request, &mut resolver).outcome.code(),
            "scope_violation",
            "{path}"
        );
    }
}

#[test]
fn the_baselines_own_policy_is_protected() {
    // A gate command names a manifest; the security review rule lists a
    // source file. Both come from the baseline the request was raised on.
    let base: Files<'_> = &[
        ("a.txt", Some("one\ntwo\nthree\n")),
        (
            ".aethyme/gates.toml",
            Some("[[gate]]\nname = \"t\"\ncommand = \"make -f tools/Makefile check\"\n"),
        ),
        (
            ".aethyme/config.toml",
            Some(
                "[[review.trigger.rule]]\nname = \"s\"\nrequire = [\"security\"]\npaths = [\"src/auth/**\"]\n",
            ),
        ),
        ("tools/Makefile", Some("check:\n\ttrue\n")),
        ("src/auth/token.rs", Some("fn t() {}\n")),
    ];
    for path in ["tools/Makefile", "src/auth/token.rs", "SRC/Auth/token.rs"] {
        let contribution: Vec<(&str, Option<&str>)> = vec![
            ("a.txt", Some("one\nA\nthree\n")),
            (path, Some("changed\n")),
        ];
        let mut world = world(base, &[("A", &contribution)]);
        let request = raise(&mut world, &["A"], true, &[], None);
        let path_owned = path.to_string();
        let (mut resolver, _) =
            fixed(move |_, _| Ok(Response::Proposal(writes(&[(path_owned.as_str(), "x\n")]))));
        assert_eq!(
            resolve(&mut world, &request, &mut resolver).outcome.code(),
            "scope_violation",
            "{path}"
        );
    }
}

#[test]
fn a_symlink_cannot_be_retargeted() {
    let mut world = world(BASE, &[("A", &[("a.txt", Some("one\nA\nthree\n"))])]);
    git(
        &world.repo,
        &["checkout", "-q", "--detach", world.baseline.as_str()],
    );
    std::os::unix::fs::symlink("b.txt", world.repo.join("link")).unwrap();
    git(&world.repo, &["add", "link"]);
    git(&world.repo, &["commit", "-q", "-m", "L"]);
    let result = git(&world.repo, &["rev-parse", "HEAD"]);
    capture_contribution(&mut world, "L", &result);
    let request = raise(&mut world, &["A", "L"], true, &[], None);
    for kind in [EntryKind::Symlink, EntryKind::Regular] {
        let (mut resolver, _) = fixed(move |_, _| {
            let mut proposal = Proposal::default();
            proposal.writes.insert(
                "link".into(),
                Some(ProposedFile {
                    kind,
                    bytes: b"../../../.aethyme/gates.toml".to_vec(),
                }),
            );
            Ok(Response::Proposal(proposal))
        });
        assert_eq!(
            resolve(&mut world, &request, &mut resolver).outcome.code(),
            "scope_violation"
        );
    }
}

// ------------------------------------------------------------------ F3

#[test]
fn a_three_way_conflict_is_one_decision_in_every_order() {
    let mut world = world(
        BASE,
        &[
            ("A", &[("a.txt", Some("one\nA\nthree\n"))]),
            ("B", &[("a.txt", Some("one\nB\nthree\n"))]),
            ("C", &[("a.txt", Some("one\nC\nthree\n"))]),
        ],
    );
    let mut dispatched = 0;
    let mut keys = std::collections::BTreeSet::new();
    for order in [["A", "B", "C"], ["A", "C", "B"], ["B", "C", "A"]] {
        let request = raise(&mut world, &order, false, &[], None);
        keys.insert(request.decision_key());
        for _ in 0..3 {
            let (mut resolver, calls) = declines();
            resolve(&mut world, &request, &mut resolver);
            dispatched += calls.get();
        }
    }
    assert_eq!(keys.len(), 1);
    assert_eq!(dispatched, 2);
}

#[test]
fn padding_or_trimming_a_failed_decision_does_not_reset_its_allowance() {
    let mut world = world(
        BASE,
        &[
            ("A", &[("a.txt", Some("one\nA\nthree\n"))]),
            ("U", &[("u.txt", Some("u2\n"))]),
        ],
    );
    let alone = raise(&mut world, &["A"], true, &[], None);
    for _ in 0..2 {
        let (mut resolver, _) = declines();
        resolve(&mut world, &alone, &mut resolver);
    }
    let padded = raise(&mut world, &["A", "U"], true, &[], None);
    assert_eq!(padded.allowance().remaining(), 0);
    let (mut resolver, calls) = declines();
    assert_eq!(
        resolve(&mut world, &padded, &mut resolver).outcome.code(),
        "budget_exhausted"
    );
    assert_eq!(calls.get(), 0);

    // And the other way round: spend a padded decision, then trim it.
    let mut world = self::world(
        BASE,
        &[
            ("A", &[("a.txt", Some("one\nA\nthree\n"))]),
            ("U", &[("u.txt", Some("u2\n"))]),
        ],
    );
    let padded = raise(&mut world, &["A", "U"], true, &[], None);
    for _ in 0..2 {
        let (mut resolver, _) = declines();
        resolve(&mut world, &padded, &mut resolver);
    }
    let trimmed = raise(&mut world, &["A"], true, &[], None);
    assert_eq!(trimmed.allowance().remaining(), 0);
}

// ------------------------------------------------------------------ F4

fn three_with_an_unrelated_change() -> World {
    world(
        BASE,
        &[
            ("U", &[("u.txt", Some("u2\n"))]),
            ("A", &[("a.txt", Some("one\nA\nthree\n"))]),
            (
                "B",
                &[("a.txt", Some("one\nB\nthree\n")), ("b.txt", Some("b2\n"))],
            ),
        ],
    )
}

#[test]
fn a_member_s_other_changes_may_not_be_dropped() {
    let mut world = three_with_an_unrelated_change();
    let request = raise(&mut world, &["U", "A", "B"], false, &[], None);
    let (mut drops_b, _) = proposing(&[("a.txt", "one\nA and B\nthree\n")]);
    assert_eq!(
        resolve(&mut world, &request, &mut drops_b).outcome.code(),
        "unresolved"
    );
}

#[test]
fn a_synthesized_candidate_names_everything_it_holds() {
    let mut world = three_with_an_unrelated_change();
    let request = raise(&mut world, &["U", "A", "B"], false, &[], None);
    let (mut keeps_all, _) = proposing(&[("a.txt", "one\nA and B\nthree\n"), ("b.txt", "b2\n")]);
    let resolution = resolve(&mut world, &request, &mut keeps_all);
    let files = files_of(&world, &resolution);
    assert!(files.contains(&("u.txt".into(), "u2".into())));
    assert!(files.contains(&("b.txt".into(), "b2".into())));
    let ResolutionOutcome::Synthesized(synthesized) = &resolution.outcome else {
        unreachable!();
    };
    assert!(matches!(
        synthesized.candidate.producer,
        Producer::Resolver { .. }
    ));
    let named: Vec<_> = synthesized
        .candidate
        .inputs
        .iter()
        .map(|input| input.result_snapshot.clone().unwrap())
        .collect();
    for member in request.contents().iter().chain(request.members()) {
        assert!(named.contains(&member.result), "{} is named", member.id);
    }
    assert_eq!(synthesized.candidate.inputs.len(), 3, "U, A and B");
}

#[test]
fn failed_checks_may_not_revert_a_member() {
    let mut world = world(
        BASE,
        &[
            ("A", &[("a.txt", Some("one\nA\nthree\n"))]),
            ("D", &[("b.txt", Some("b2\n"))]),
        ],
    );
    let request = raise(&mut world, &["A", "D"], true, &[], None);
    let (mut reverts_d, _) = proposing(&[("b.txt", "b\n")]);
    assert_eq!(
        resolve(&mut world, &request, &mut reverts_d).outcome.code(),
        "unresolved"
    );
    let (mut cosmetic_revert, _) = proposing(&[("b.txt", "b  \r\n\n")]);
    assert_eq!(
        resolve(&mut world, &request, &mut cosmetic_revert)
            .outcome
            .code(),
        "unresolved"
    );
}

// ------------------------------------------------------------------ F5

#[test]
fn a_cosmetic_last_writer_is_still_a_last_writer() {
    for proposal in [
        "one\nB \nthree\n",
        "one\nB\nthree",
        "one\r\nB\r\nthree\r\n",
        "one\nB\nthree\n\n",
    ] {
        let mut world = two_writers();
        let request = raise(&mut world, &["A", "B"], false, &[], None);
        let (mut resolver, _) =
            fixed(move |_, _| Ok(Response::Proposal(writes(&[("a.txt", proposal)]))));
        assert_eq!(
            resolve(&mut world, &request, &mut resolver).outcome.code(),
            "unresolved",
            "{proposal:?}"
        );
    }
}

// ------------------------------------------------------------------ F6

#[test]
fn an_error_after_dispatch_is_a_recorded_outcome() {
    let mut world = world(BASE, &[("A", &[("a.txt", Some("one\nA\nthree\n"))])]);
    let request = raise(&mut world, &["A"], true, &[], None);
    // The object database refuses writes, so materializing the proposal
    // fails after the resolver ran.
    let objects = PathBuf::from(git(&world.repo, &["rev-parse", "--git-path", "objects"]));
    let objects = world.repo.join(objects);
    let lock = |mode: &str| {
        let status = Command::new("chmod")
            .args(["-R", mode])
            .arg(&objects)
            .status()
            .unwrap();
        assert!(status.success());
    };
    lock("a-w");
    let (mut resolver, calls) = proposing(&[("a.txt", "one\nfixed\nthree\n")]);
    let outcome = resolution::resolve(
        &mut world.store,
        &world.repo,
        request.record(),
        &mut resolver,
    );
    lock("u+w");
    let resolution = outcome.expect("a recorded outcome, not an error");
    assert_eq!(calls.get(), 1);
    assert_eq!(resolution.outcome.code(), "infrastructure_deferred");
    assert_eq!(resolution.allowance.used, 1);
    drop(world.root);
}

// ------------------------------------------------------------ dismissals

#[test]
fn a_panicking_resolver_keeps_its_charge() {
    let mut world = two_writers();
    let request = raise(&mut world, &["A", "B"], false, &[], None);
    let record = request.record().clone();
    let repo = world.repo.clone();
    let store = &mut world.store;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (mut resolver, _) = fixed(|_, _| panic!("resolver crashed"));
        resolution::resolve(store, &repo, &record, &mut resolver)
    }));
    assert!(caught.is_err());
    let again = raise(&mut world, &["A", "B"], false, &[], None);
    assert_eq!(again.allowance().used, 1);
}

#[test]
fn reads_are_confined_to_exact_scoped_paths() {
    let mut world = world(BASE, &[("A", &[("a.txt", Some("one\nA\nthree\n"))])]);
    let request = raise(&mut world, &["A"], true, &[], None);
    let (mut resolver, _) = fixed(|request, inputs| {
        for path in ["b.txt", "./a.txt", "A.TXT", "a.txt/", "../a.txt"] {
            assert!(
                inputs.read(&request.baseline().snapshot_id, path).is_err(),
                "{path}"
            );
        }
        Ok(Response::Inconclusive("probed".into()))
    });
    assert_eq!(
        resolve(&mut world, &request, &mut resolver).outcome.code(),
        "inconclusive"
    );
}

#[test]
fn a_requirement_path_outside_the_tree_is_refused() {
    let mut world = world(BASE, &[("A", &[("a.txt", Some("one\nA\nthree\n"))])]);
    let request = composition_request(&world, &["A"]);
    let composition = composer::compose(&mut world.store, &world.repo, &request).unwrap();
    for path in ["../outside", "pkg/../../x", "/", "./"] {
        let error = resolution::request_resolution(
            &mut world.store,
            &world.repo,
            &request,
            &composition,
            &opaque(),
            &[RequirementRef {
                id: "harness".into(),
                path: Some(path.into()),
            }],
            None,
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_request", "{path}");
    }
}
