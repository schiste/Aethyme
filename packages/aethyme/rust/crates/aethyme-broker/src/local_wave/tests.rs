use super::*;
use crate::collaboration_capture::OperationId;
use crate::collaboration_cli::project_test_support::BoundProjectFixture;
use crate::collaboration_identity_store::{
    ContributionRevisionRequest, ResolvedContributionRevision, revise_contribution_for_operation,
};
use crate::local_target::tests::Fixture;

fn composed_candidate(f: &mut Fixture) -> (ControllerOwner, Wave) {
    let (owner, _) = setup(f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
    let composed = compose_with_source(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    assert_eq!(composed.wave.phase, WavePhase::Verifying);
    (owner, composed.wave)
}

#[test]
fn composition_source_accepts_content_identical_candidate_with_new_commit_locator() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());

    let composed = compose_with_source(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    let source = composed
        .source
        .as_ref()
        .expect("the fresh composition source should be retained");
    let candidate = source.composition.outcome.candidate().unwrap();
    let retained = archive::retained(&f.store, &candidate.subject)
        .unwrap()
        .expect("candidate subject remains retained");
    assert_ne!(retained.commit, candidate.commit);
    assert_eq!(
        archive::snapshot_of_commit(f._repo.path(), &candidate.commit)
            .unwrap()
            .id(),
        candidate.subject
    );

    let resumed = compose_with_source(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    assert!(resumed.source.is_some());
}

fn rewrite_source_record(
    f: &mut Fixture,
    owner: &ControllerOwner,
    wave: &Wave,
    edit: impl FnOnce(&mut CompositionSourcePayload),
) -> resolution::RecordRef {
    let source_ref = wave.composition_source.as_ref().unwrap();
    let bytes = archive::read_object(&f.store, &source_ref.object).unwrap();
    let mut payload: CompositionSourcePayload = serde_json::from_slice(&bytes).unwrap();
    edit(&mut payload);
    let encoded = serde_json::to_vec(&payload).unwrap();
    let record = Record::decode(&encoded, &[&COMPOSITION_SOURCE_SCHEMA]).unwrap();
    let object = archive::put_object(&f.store, &record.canonical_bytes()).unwrap();
    let revision = id(&wave.spec.revision).unwrap();
    target::retain_artifacts(
        &mut f.store,
        owner,
        &revision,
        &[],
        std::slice::from_ref(&object),
    )
    .unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_waves SET composition_source_record_id=?2, composition_source_object=?3 WHERE wave=?1",
            params![wave.id.as_str(), record.id().as_str(), object.hex()],
        )
        .unwrap();
    resolution::RecordRef {
        id: record.id(),
        object,
    }
}

fn setup(f: &mut Fixture, selected: &[&str]) -> (ControllerOwner, RecordId) {
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.selected = selected.iter().map(|s| (*s).into()).collect();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    (owner, revision.id)
}

fn policy() -> WavePolicy {
    WavePolicy {
        debounce_ms: 0,
        age_promotion_ms: 1000,
        ..WavePolicy::default()
    }
}

fn revise_bound_operation(
    fixture: &mut BoundProjectFixture,
    operation_id: &str,
) -> ResolvedContributionRevision {
    let operation_id = OperationId::parse(operation_id).unwrap();
    revise_contribution_for_operation(
        &mut fixture.store,
        &fixture.binding,
        ContributionRevisionRequest {
            operation_id: &operation_id,
            file_operations: None,
            brief: None,
            depends_on: None,
        },
    )
    .unwrap()
}

fn conflicting_bound_revisions() -> (
    BoundProjectFixture,
    ResolvedContributionRevision,
    ResolvedContributionRevision,
) {
    let mut fixture = BoundProjectFixture::new().unwrap();
    let base = fixture.bootstrap_commit.clone();
    let first_result = fixture
        .commit_file("src/shared.rs", b"pub fn shared_a() {}\n")
        .unwrap();

    // Create a second independently captured result from the same base. The
    // path overlaps, while the distinct source commits keep the two legacy
    // capture lineages separate for wave admission.
    crate::local_target::tests::git(fixture.repo_path(), &["switch", "--detach", base.as_str()]);
    std::fs::create_dir_all(fixture.repo_path().join("src")).unwrap();
    std::fs::write(
        fixture.repo_path().join("src/shared.rs"),
        b"pub fn shared_b() {}\n",
    )
    .unwrap();
    crate::local_target::tests::git(fixture.repo_path(), &["add", "--", "src/shared.rs"]);
    crate::local_target::tests::git(
        fixture.repo_path(),
        &["commit", "-q", "-m", "second independent source change"],
    );
    let second_result = CommitOid::parse(&crate::local_target::tests::git(
        fixture.repo_path(),
        &["rev-parse", "HEAD"],
    ))
    .unwrap();

    fixture
        .capture_operation("wave-operation-a", &base, &first_result)
        .unwrap();
    fixture
        .capture_operation("wave-operation-b", &base, &second_result)
        .unwrap();
    let first = revise_bound_operation(&mut fixture, "wave-operation-a");
    let second = revise_bound_operation(&mut fixture, "wave-operation-b");
    assert_ne!(first.capture_lineage(), second.capture_lineage());
    (fixture, first, second)
}

fn propose_d10_wave_target(
    fixture: &mut BoundProjectFixture,
    name: &str,
    revisions: &[(&str, &ResolvedContributionRevision)],
    selected: &[&str],
) -> (ControllerOwner, RecordId) {
    let contributions = revisions
        .iter()
        .map(|(alias, revision)| target::RequestedContribution {
            id: (*alias).into(),
            lineage: revision.capture_lineage().to_string(),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        })
        .collect();
    let pins = selected
        .iter()
        .map(|alias| {
            let revision = revisions
                .iter()
                .find(|(candidate, _)| candidate == alias)
                .expect("each selected alias has a bound revision")
                .1;
            target::D10RevisionPin {
                contribution: (*alias).into(),
                logical_contribution_id: revision.revision().contribution_id().to_string(),
                revision_record_id: revision.record_id().to_string(),
                revision_object_sha256: revision.object_digest().hex(),
            }
        })
        .collect();
    let spec = target::TargetSpec {
        objective_authorization: format!("sha256:{}", "1".repeat(64)),
        objective: "compose a source-bound selected cohort".into(),
        baseline: fixture.bootstrap_snapshot.as_str().into(),
        contributions,
        selected: selected.iter().map(|alias| (*alias).into()).collect(),
        requirements: vec![format!("sha256:{}", "2".repeat(64))],
        acceptance_policy: format!("sha256:{}", "3".repeat(64)),
        composer_profile: crate::composer::PROVISIONAL_TEXT_PROFILE.into(),
    };
    let bindings = target::D10RevisionBindings {
        profile: target::D10_TARGET_PROFILE.into(),
        project_id: fixture.binding.project_id().to_string(),
        descriptor_record_id: fixture.binding.descriptor_record_id().to_string(),
        descriptor_object_sha256: fixture.binding.descriptor_object().hex(),
        revisions: pins,
    };
    let owner = target::acquire_controller(&mut fixture.store, name, "issuer").unwrap();
    let revision =
        target::propose_revision_with_d10_bindings(&mut fixture.store, &owner, spec, bindings)
            .unwrap();
    target::select_revision(&mut fixture.store, &owner, &revision.id).unwrap();
    (owner, revision.id)
}

#[test]
fn split_single_item_waves_refuse_conflicts_across_the_full_selected_cohort() {
    let (mut fixture, first, second) = conflicting_bound_revisions();
    let (owner, _) = propose_d10_wave_target(
        &mut fixture,
        "d10-wave-conflict",
        &[("A", &first), ("B", &second)],
        &["A", "B"],
    );
    available(&mut fixture.store, &owner, "A", 0).unwrap();
    available(&mut fixture.store, &owner, "B", 1).unwrap();
    let one_item = WavePolicy {
        batch_width: 1,
        ..policy()
    };

    let first_wave = frozen(admit(&mut fixture.store, &owner, one_item, 1).unwrap());
    assert_eq!(first_wave.spec.selected, ["A"]);
    let repo = fixture.repo_path().to_path_buf();
    let first_result = compose(&mut fixture.store, &owner, &repo, &first_wave.id).unwrap();
    assert_eq!(first_result.phase, WavePhase::Blocked);
    assert!(first_result.candidate_snapshot.is_none());

    let second_wave = frozen(admit(&mut fixture.store, &owner, one_item, 2).unwrap());
    assert_eq!(second_wave.spec.selected, ["B"]);
    let second_result = compose(&mut fixture.store, &owner, &repo, &second_wave.id).unwrap();
    assert_eq!(second_result.phase, WavePhase::Blocked);
    assert!(second_result.candidate_snapshot.is_none());
}

#[test]
fn d10_wave_validation_does_not_include_conflicting_unselected_catalog_entries() {
    let (mut fixture, first, second) = conflicting_bound_revisions();
    let (owner, _) = propose_d10_wave_target(
        &mut fixture,
        "d10-wave-subset",
        &[("A", &first), ("B", &second)],
        &["A"],
    );
    available(&mut fixture.store, &owner, "A", 0).unwrap();
    let one_item = WavePolicy {
        batch_width: 1,
        ..policy()
    };
    let wave = frozen(admit(&mut fixture.store, &owner, one_item, 0).unwrap());
    assert_eq!(wave.spec.selected, ["A"]);

    let repo = fixture.repo_path().to_path_buf();
    let result = compose(&mut fixture.store, &owner, &repo, &wave.id).unwrap();
    assert_eq!(result.phase, WavePhase::Verifying);
    assert!(result.candidate_snapshot.is_some());
}

#[test]
fn independent_wave_identity_vector_binds_order_and_policy() {
    let vector: serde_json::Value = serde_json::from_str(include_str!("wave-vector.json")).unwrap();
    let bytes = vector["canonical"].as_str().unwrap().as_bytes();
    let record = Record::decode(bytes, &[&SCHEMA]).unwrap();
    assert_eq!(record.canonical_bytes(), bytes);
    assert_eq!(record.id().as_str(), vector["id"].as_str().unwrap());
    let body: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let original: WaveSpec = serde_json::from_value(body["spec"].clone()).unwrap();
    original.policy.validate().unwrap();
    let mut changed = original.clone();
    changed.selected.reverse();
    let bytes =
        serde_json::to_vec(&serde_json::json!({"schema":SCHEMA_NAME,"spec":changed})).unwrap();
    assert_ne!(
        Record::decode(&bytes, &[&SCHEMA]).unwrap().id(),
        record.id()
    );
    let mut changed = original;
    changed.policy.drift_rechecks += 1;
    let bytes =
        serde_json::to_vec(&serde_json::json!({"schema":SCHEMA_NAME,"spec":changed})).unwrap();
    assert_ne!(
        Record::decode(&bytes, &[&SCHEMA]).unwrap().id(),
        record.id()
    );
}

fn frozen(admission: Admission) -> Wave {
    match admission {
        Admission::Frozen(wave) => wave,
        other => panic!("{other:?}"),
    }
}

#[test]
fn availability_is_exact_idempotent_and_not_permission_to_add_work() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A", "B"]);
    available(&mut f.store, &owner, "B", 10).unwrap();
    available(&mut f.store, &owner, "B", 1000).unwrap();
    available(&mut f.store, &owner, "A", 20).unwrap();
    assert!(available(&mut f.store, &owner, "unselected", 30).is_err());
    let wave = frozen(admit(&mut f.store, &owner, policy(), 30).unwrap());
    assert_eq!(wave.spec.selected, ["B", "A"]);
    let count: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_wave_arrivals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn timed_admission_recovers_and_debounce_does_not_slide_with_arrivals() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A", "B"]);
    assert!(matches!(
        admit(&mut f.store, &owner, WavePolicy::default(), 0).unwrap(),
        Admission::WaitingForInput { .. }
    ));
    available(&mut f.store, &owner, "A", 10).unwrap();
    assert_eq!(
        admit(&mut f.store, &owner, WavePolicy::default(), 100).unwrap(),
        Admission::Debouncing { until_ms: 210 }
    );
    available(&mut f.store, &owner, "B", 200).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, WavePolicy::default(), 210).unwrap());
    assert_eq!(wave.spec.selected, ["A", "B"]);
}

#[test]
fn arriving_work_and_duplicate_ticks_cannot_replace_an_active_wave() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A", "B"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let p = WavePolicy {
        batch_width: 1,
        ..policy()
    };
    let wave = frozen(admit(&mut f.store, &owner, p, 0).unwrap());
    available(&mut f.store, &owner, "B", 10).unwrap();
    for tick in 10..110 {
        available(&mut f.store, &owner, "B", tick).unwrap();
        assert_eq!(
            admit(&mut f.store, &owner, WavePolicy::default(), tick).unwrap(),
            Admission::Existing(wave.clone())
        );
    }
    assert_eq!(wave.spec.selected, ["A"]);
    let count: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_waves", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn oldest_large_group_gets_a_dedicated_wave_before_new_short_work() {
    let mut f = Fixture::new();
    let base = f.a.base.commit.clone();
    let c = f.feature(&base);
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.contributions[0].atomic_group = Some("long".into());
    spec.contributions[1].atomic_group = Some("long".into());
    spec.contributions.push(c);
    spec.selected = vec!["A".into(), "B".into(), "C".into()];
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    available(&mut f.store, &owner, "A", 0).unwrap();
    available(&mut f.store, &owner, "B", 1).unwrap();
    available(&mut f.store, &owner, "C", 2).unwrap();
    for tick in 3..103 {
        available(&mut f.store, &owner, "C", tick).unwrap();
    }
    let wave = frozen(
        admit(
            &mut f.store,
            &owner,
            WavePolicy {
                batch_width: 1,
                ..policy()
            },
            103,
        )
        .unwrap(),
    );
    assert_eq!(wave.spec.selected, ["A", "B"]);
    assert_eq!(
        target::dispositions(&f.store, &revision.id).unwrap()[2].status,
        target::Disposition::Pending
    );
}

#[test]
fn age_promotion_keeps_the_oldest_group_dedicated() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A", "B"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    available(&mut f.store, &owner, "B", 900).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 1000).unwrap());
    assert_eq!(wave.spec.selected, ["A"]);
}

#[test]
fn lineage_prerequisites_are_admitted_together_after_the_parent_arrives() {
    let mut f = Fixture::new();
    let base = f.a.result.commit.clone();
    let c = f.feature(&base);
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.contributions.push(c);
    spec.selected = vec!["C".into(), "A".into()];
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    available(&mut f.store, &owner, "C", 0).unwrap();
    assert_eq!(
        admit(&mut f.store, &owner, policy(), 0).unwrap(),
        Admission::WaitingForInput {
            missing: vec!["A".into()]
        }
    );
    available(&mut f.store, &owner, "A", 1).unwrap();
    let wave = frozen(
        admit(
            &mut f.store,
            &owner,
            WavePolicy {
                batch_width: 1,
                ..policy()
            },
            1,
        )
        .unwrap(),
    );
    assert_eq!(wave.spec.selected, ["A", "C"]);
}

#[test]
fn atomic_replacements_keep_the_original_group_in_one_wave() {
    let mut f = Fixture::new();
    let base = f.a.base.commit.clone();
    let mut c = f.feature(&base);
    c.revision_of = Some("A".into());
    c.atomic_group = Some("group".into());
    let mut spec = f.spec();
    for member in &mut spec.contributions {
        member.atomic_group = Some("group".into());
    }
    spec.contributions.push(c);
    spec.selected = vec!["C".into(), "B".into()];
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    available(&mut f.store, &owner, "C", 0).unwrap();
    assert_eq!(
        admit(&mut f.store, &owner, policy(), 0).unwrap(),
        Admission::WaitingForInput {
            missing: vec!["B".into()]
        }
    );
    available(&mut f.store, &owner, "B", 1).unwrap();
    let wave = frozen(
        admit(
            &mut f.store,
            &owner,
            WavePolicy {
                batch_width: 1,
                ..policy()
            },
            1,
        )
        .unwrap(),
    );
    assert_eq!(wave.spec.selected, ["C", "B"]);
}

#[test]
fn a_real_composer_dispatch_roots_candidate_before_releasing_archive_guard() {
    let mut f = Fixture::new();
    let (owner, revision) = setup(&mut f, &["A", "B"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    available(&mut f.store, &owner, "B", 1).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 1).unwrap());
    let before = archive::pin_commit(f._repo.path(), "HEAD").unwrap();
    let complete = compose(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    assert_eq!(complete.phase, WavePhase::Verifying);
    assert_eq!(
        complete.detail.as_deref(),
        Some("trusted verification adapter unavailable")
    );
    assert_eq!(archive::pin_commit(f._repo.path(), "HEAD").unwrap(), before);
    assert_eq!(
        compose(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap(),
        complete
    );
    let output = tempfile::tempdir().unwrap();
    archive::reconstruct(
        &f.store,
        complete.candidate_snapshot.as_ref().unwrap(),
        output.path(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(output.path().join("one.txt")).unwrap(),
        "A\n"
    );
    assert_eq!(
        std::fs::read_to_string(output.path().join("two.txt")).unwrap(),
        "B\n"
    );
    let roots: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_target_roots WHERE revision=?1 AND kind='object'",
            [revision.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(roots, 2, "recipe and exact composition source are rooted");
    let candidate_roots: i64 = f.store.read_connection().query_row(
        "SELECT count(*) FROM local_target_roots WHERE revision=?1 AND kind='snapshot' AND reference=?2",
        params![revision.as_str(),complete.candidate_snapshot.as_ref().unwrap().as_str()], |r| r.get(0),
    ).unwrap();
    assert_eq!(candidate_roots, 1);
    let state = target::status(&f.store, owner.target()).unwrap();
    assert_eq!(state.phase, Phase::WaitingForInput);
    assert_eq!(state.resume_phase, Some(Phase::Verifying));
    let state = target::resume(&mut f.store, &owner, state.version).unwrap();
    assert!(matches!(
        target::transition(
            &mut f.store,
            &owner,
            state.version,
            Phase::ReadyForAcceptance
        ),
        Err(TargetError::VerificationUnavailable)
    ));
}

#[test]
fn accepted_baseline_incremental_resolution_is_explicitly_unsupported() {
    use crate::composer::{self, CompositionBudget, CompositionRequest, ContributionSpec};

    let mut f = Fixture::new();
    let contribution = |id: &str, retained: &crate::collaboration_archive::RetainedContribution| {
        ContributionSpec {
            id: id.into(),
            lineage: Some(retained.lineage_record_id.clone()),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        }
    };
    let original_baseline = f.a.base.commit.clone();
    let mut request = CompositionRequest {
        baseline: original_baseline.clone(),
        accepted: Vec::new(),
        catalog: vec![contribution("A", &f.a), contribution("B", &f.b)],
        deliveries: vec!["A".into()],
        budget: CompositionBudget::default(),
    };
    let first = composer::compose(&mut f.store, f._repo.path(), &request).unwrap();
    let first = first.outcome.candidate().unwrap();
    request.baseline = first.commit.clone();
    request.accepted.push(original_baseline);
    request.deliveries.push("B".into());
    let proof =
        RecordId::parse(&format!("sha256:{}", ObjectDigest::of(b"completion").hex())).unwrap();
    let incremental = composer::compose_after_accepted(
        &mut f.store,
        f._repo.path(),
        &request,
        proof,
        vec!["A".into()],
    )
    .unwrap();
    assert!(incremental.outcome.candidate().is_some());
    assert!(recipe_has_accepted_baseline(&f.store, &incremental).unwrap());
    assert!(matches!(
        ensure_resolution_supported(&f.store, &incremental),
        Err(WaveError::ResolutionUnsupported)
    ));
    let refused = resolution::request_resolution(
        &mut f.store,
        f._repo.path(),
        &request,
        &incremental,
        &[FailedCheck {
            check:
                "native-gate:unit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
        }],
        &[],
        None,
    );
    assert!(matches!(
        refused,
        Err(resolution::ResolutionError::InvalidRequest(detail))
            if detail.contains("accepted-baseline")
    ));
}

#[test]
fn composition_source_survives_restart_and_raises_one_exact_failed_check_request() {
    let mut f = Fixture::new();
    let (owner, wave) = composed_candidate(&mut f);
    let source_ref = wave.composition_source.clone().unwrap();
    let repo = f._repo.path().to_path_buf();
    let host = f._host.path().to_path_buf();
    let revision = id(&wave.spec.revision).unwrap();
    drop(owner);
    drop(f.store);

    let mut reopened = crate::collaboration_state::CollaborationStore::open(
        &crate::collaboration_state::CollaborationRoot::under_host_state(&host),
        &crate::collaboration_state::ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap();
    let owner = target::acquire_controller(&mut reopened, "target", "appointed").unwrap();
    let resumed = compose_with_source(&mut reopened, &owner, &repo, &wave.id).unwrap();
    assert_eq!(resumed.wave.composition_source.as_ref(), Some(&source_ref));
    let source = resumed
        .source
        .as_ref()
        .expect("exact source survives restart");
    assert_eq!(source.request.deliveries, ["A"]);
    assert_eq!(
        source.composition.outcome.candidate().unwrap().subject,
        *resumed.wave.candidate_snapshot.as_ref().unwrap()
    );

    let checks = [FailedCheck {
        check: "native-gate:unit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
    }];
    assert!(matches!(
        raise_resolution_for_failed_checks(
            &mut reopened,
            &owner,
            &repo,
            &wave.id,
            source,
            &checks,
            &std::sync::atomic::AtomicBool::new(true),
        ),
        Err(WaveError::Stale)
    ));
    assert!(
        load(&reopened, &wave.id)
            .unwrap()
            .resolution_request
            .is_none()
    );
    let attached = raise_resolution_for_failed_checks(
        &mut reopened,
        &owner,
        &repo,
        &wave.id,
        source,
        &checks,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let request_ref = attached.resolution_request.as_ref().unwrap();
    let record = resolution::read_record(&reopened, request_ref).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&record.canonical_bytes()).unwrap();
    assert_eq!(request["trigger"]["kind"], "failed_checks");
    assert_eq!(
        request["trigger"]["candidate"],
        resumed.wave.candidate_snapshot.as_ref().unwrap().as_str()
    );
    assert_eq!(request["trigger"]["checks"][0], checks[0].check);
    let roots: i64 = reopened
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_target_roots WHERE revision=?1 AND kind='object' AND reference=?2",
            params![revision.as_str(), request_ref.object.hex()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        roots, 1,
        "the attached request remains retained by the revision"
    );
    let repeated = raise_resolution_for_failed_checks(
        &mut reopened,
        &owner,
        &repo,
        &wave.id,
        source,
        &checks,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(repeated.resolution_request.as_ref(), Some(request_ref));
}

#[test]
fn resolution_preparation_requires_the_exact_current_failed_check_request() {
    let mut f = Fixture::new();
    let (owner, composed) = composed_candidate(&mut f);
    let repo = f._repo.path().to_path_buf();
    let resolved = compose_with_source(&mut f.store, &owner, &repo, &composed.id).unwrap();
    let source = resolved
        .source
        .as_ref()
        .expect("new composition retains source");
    let checks = [FailedCheck {
        check: "native-gate:unit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
    }];
    let wave = raise_resolution_for_failed_checks(
        &mut f.store,
        &owner,
        &repo,
        &resolved.wave.id,
        source,
        &checks,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let request_ref = wave.resolution_request.as_ref().unwrap().clone();
    let state = target::status(&f.store, owner.target()).unwrap();

    require_current_resolution_request(
        &mut f.store,
        &repo,
        &owner,
        &wave.id,
        state.version,
        &request_ref,
    )
    .unwrap();

    assert!(matches!(
        require_current_resolution_request(
            &mut f.store,
            &repo,
            &owner,
            &wave.id,
            state.version - 1,
            &request_ref,
        ),
        Err(WaveError::Stale)
    ));
    let swapped_ref = resolution::RecordRef {
        id: request_ref.id.clone(),
        object: ObjectDigest::of(b"not the linked request"),
    };
    assert!(matches!(
        require_current_resolution_request(
            &mut f.store,
            &repo,
            &owner,
            &wave.id,
            state.version,
            &swapped_ref,
        ),
        Err(WaveError::Stale)
    ));

    let stopped = stop(&mut f.store, &owner, &wave.id).unwrap();
    assert_eq!(stopped.phase, WavePhase::Stopped);
    let stopped_state = target::status(&f.store, owner.target()).unwrap();
    assert!(matches!(
        require_current_resolution_request(
            &mut f.store,
            &repo,
            &owner,
            &wave.id,
            stopped_state.version,
            &request_ref,
        ),
        Err(WaveError::Stale)
    ));
}

static REQUEST_LINK_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn note_request_link_busy(_: i32) -> bool {
    REQUEST_LINK_BUSY.store(true, Ordering::SeqCst);
    true
}

#[test]
fn cancellation_while_waiting_for_wave_write_lock_prevents_request_link() {
    let mut f = Fixture::new();
    let (owner, wave) = composed_candidate(&mut f);
    let composed = compose_with_source(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    let wave = composed.wave;
    let source = composed.source.unwrap();
    let checks = [FailedCheck {
        check: "native-gate:unit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
    }];
    let request = resolution::request_resolution(
        &mut f.store,
        f._repo.path(),
        &source.request,
        &source.composition,
        &checks,
        &source.requirements,
        None,
    )
    .unwrap();
    let request_ref = request.record().clone();
    let revision = id(&wave.spec.revision).unwrap();
    let archive_guard = archive_use(&f.store).unwrap();
    let response_guard = crate::collaboration_sensitive::response_use(&f.store).unwrap();
    let authority_guard = target::authorize(&f.store, &owner).unwrap();
    retain_resolution_request(
        &mut f.store,
        &owner,
        &revision,
        &request,
        &source.request,
        RetentionGuards {
            archive: &archive_guard,
            response: &response_guard,
            authority: &authority_guard,
        },
    )
    .unwrap();
    let state = current(&f.store, &owner, &wave).unwrap();

    REQUEST_LINK_BUSY.store(false, Ordering::SeqCst);
    f.store
        .connection()
        .busy_handler(Some(note_request_link_busy))
        .unwrap();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let database = f.store.project_dir().join("state.db");
    let blocker = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked_tx.send(()).unwrap();
        release_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
    });
    locked_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();

    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let cancel_waiter = std::sync::Arc::clone(&cancel);
    let releaser = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !REQUEST_LINK_BUSY.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let observed_wait = REQUEST_LINK_BUSY.load(Ordering::SeqCst);
        cancel_waiter.store(true, Ordering::SeqCst);
        release_tx.send(()).unwrap();
        observed_wait
    });

    let result = attach_resolution_request(
        &mut f.store,
        &owner,
        &wave,
        &source,
        &request_ref,
        state.version,
        &cancel,
    );
    assert!(
        releaser.join().unwrap(),
        "request link never waited on SQLite"
    );
    blocker.join().unwrap();
    assert!(matches!(result, Err(WaveError::Stale)));
    assert!(
        load(&f.store, &wave.id)
            .unwrap()
            .resolution_request
            .is_none()
    );
}

#[test]
fn legacy_candidate_waves_without_exact_composition_source_stay_request_free() {
    let mut f = Fixture::new();
    let (owner, wave) = composed_candidate(&mut f);
    f.store
        .connection()
        .execute(
            "UPDATE local_waves SET composition_source_record_id=NULL, composition_source_object=NULL WHERE wave=?1",
            [wave.id.as_str()],
        )
        .unwrap();
    let resumed = compose_with_source(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    assert!(resumed.source.is_none());
    assert!(resumed.wave.resolution_request.is_none());
}

#[test]
fn tampered_composition_source_bindings_never_attach_a_request() {
    for change in [
        "revision",
        "candidate_commit",
        "recipe_object",
        "noncanonical_budget",
        "noncanonical_usage",
    ] {
        let mut f = Fixture::new();
        let (owner, wave) = composed_candidate(&mut f);
        let repo = f._repo.path().to_path_buf();
        rewrite_source_record(&mut f, &owner, &wave, |payload| match change {
            "revision" => payload.revision = format!("sha256:{}", "9".repeat(64)),
            "candidate_commit" => payload.composition.candidate.commit = "f".repeat(40),
            "recipe_object" => payload.composition.recipe.object = "e".repeat(64),
            "noncanonical_budget" => {
                payload.request.budget.max_bytes = format!("0{}", payload.request.budget.max_bytes)
            }
            "noncanonical_usage" => {
                payload.composition.usage.bytes = format!("0{}", payload.composition.usage.bytes)
            }
            _ => unreachable!(),
        });
        assert!(
            compose_with_source(&mut f.store, &owner, &repo, &wave.id).is_err(),
            "{change}"
        );
        assert!(
            load(&f.store, &wave.id)
                .unwrap()
                .resolution_request
                .is_none()
        );
    }
}

#[test]
fn one_timed_driver_call_composes_three_available_inputs_and_explicitly_holds() {
    let mut f = Fixture::new();
    let base = f.a.base.commit.clone();
    let c = f.feature(&base);
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.contributions.push(c);
    spec.selected = vec!["A".into(), "B".into(), "C".into()];
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    for (clock, contribution) in ["A", "B", "C"].iter().enumerate() {
        available(&mut f.store, &owner, contribution, clock as i64).unwrap();
    }
    let run = tick(&mut f.store, &owner, f._repo.path(), policy(), 10).unwrap();
    let Run::RequiresVerification(wave) = &run else {
        panic!("{run:?}")
    };
    assert_eq!(wave.spec.selected, ["A", "B", "C"]);
    assert_eq!(
        tick(&mut f.store, &owner, f._repo.path(), policy(), 100).unwrap(),
        run
    );
    let output = tempfile::tempdir().unwrap();
    archive::reconstruct(
        &f.store,
        wave.candidate_snapshot.as_ref().unwrap(),
        output.path(),
    )
    .unwrap();
    for (path, bytes) in [("one.txt", "A\n"), ("two.txt", "B\n"), ("three.txt", "C\n")] {
        assert_eq!(
            std::fs::read_to_string(output.path().join(path)).unwrap(),
            bytes
        );
    }
}

#[test]
fn unsupported_profile_and_inadmissible_group_have_persistent_dispositions() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.composer_profile = "unqualified/profile".into();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    assert!(matches!(
        admit(&mut f.store, &owner, policy(), 0).unwrap(),
        Admission::Blocked { .. }
    ));
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        Phase::BlockedRequirement
    );
    assert_eq!(
        target::dispositions(&f.store, &revision.id).unwrap()[0].status,
        target::Disposition::Unsupported
    );
    let mut spec = f.spec();
    for c in &mut spec.contributions {
        c.atomic_group = Some("group".into());
    }
    spec.selected.push("B".into());
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    available(&mut f.store, &owner, "A", 0).unwrap();
    available(&mut f.store, &owner, "B", 1).unwrap();
    let p = WavePolicy {
        batch_width: 1,
        max_group: 1,
        ..policy()
    };
    assert!(matches!(
        admit(&mut f.store, &owner, p, 1).unwrap(),
        Admission::Blocked { .. }
    ));
    assert!(
        target::dispositions(&f.store, &revision.id)
            .unwrap()
            .iter()
            .all(|d| d.status == target::Disposition::Blocked)
    );
}

#[test]
fn repeated_external_movement_is_finite_and_does_not_relabel_the_baseline() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(
        admit(
            &mut f.store,
            &owner,
            WavePolicy {
                drift_rechecks: 1,
                ..policy()
            },
            0,
        )
        .unwrap(),
    );
    let first = f.a.result.commit.clone();
    let second = f.b.result.commit.clone();
    let seen = observe_target(&mut f.store, &owner, &wave.id, &first).unwrap();
    assert_eq!(seen.drift_count, 1);
    assert_eq!(
        observe_target(&mut f.store, &owner, &wave.id, &first)
            .unwrap()
            .drift_count,
        1
    );
    let blocked = observe_target(&mut f.store, &owner, &wave.id, &second).unwrap();
    assert_eq!(blocked.phase, WavePhase::TargetDrift);
    assert_eq!(blocked.spec.baseline, wave.spec.baseline);
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        Phase::BlockedTargetDrift
    );
    assert!(matches!(
        compose(&mut f.store, &owner, f._repo.path(), &wave.id),
        Err(WaveError::Stale)
    ));
}

#[test]
fn crash_intent_is_unknown_and_stop_does_not_claim_process_death() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
    f.store
        .connection()
        .execute(
            "UPDATE local_waves SET phase='composing' WHERE wave=?1",
            [wave.id.as_str()],
        )
        .unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='composing' WHERE target='target'",
            [],
        )
        .unwrap();
    assert!(matches!(
        compose(&mut f.store, &owner, f._repo.path(), &wave.id),
        Err(WaveError::OutcomeUnknown)
    ));
    assert_eq!(
        recover_dispatch(&mut f.store, &owner, &wave.id)
            .unwrap()
            .phase,
        WavePhase::OutcomeUnknown
    );
    assert_eq!(
        stop(&mut f.store, &owner, &wave.id).unwrap().phase,
        WavePhase::OutcomeUnknown
    );
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        Phase::StopRequested
    );
    assert!(matches!(
        admit(&mut f.store, &owner, policy(), 100).unwrap(),
        Admission::Existing(_)
    ));
    // Even a generic model transition to stopped cannot erase a durable
    // unknown dispatch or authorize new work under another revision.
    let state = target::status(&f.store, owner.target()).unwrap();
    target::transition(&mut f.store, &owner, state.version, Phase::Stopped).unwrap();
    let mut spec = f.spec();
    spec.selected = vec!["B".into()];
    let next = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    assert!(matches!(
        target::select_revision(&mut f.store, &owner, &next.id),
        Err(TargetError::Frozen)
    ));
    assert!(matches!(
        target::release_revision(&mut f.store, &owner, &id(&wave.spec.revision).unwrap()),
        Err(TargetError::Frozen)
    ));
}

#[test]
fn stopping_a_frozen_wave_cannot_revive_it_after_selecting_a_new_revision() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
    assert_eq!(
        stop(&mut f.store, &owner, &wave.id).unwrap().phase,
        WavePhase::Stopped
    );
    let mut spec = f.spec();
    spec.selected = vec!["B".into()];
    let next = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &next.id).unwrap();
    assert!(matches!(
        compose(&mut f.store, &owner, f._repo.path(), &wave.id),
        Err(WaveError::Stale)
    ));
    available(&mut f.store, &owner, "B", 1).unwrap();
    let next = frozen(admit(&mut f.store, &owner, policy(), 1).unwrap());
    assert_eq!(next.spec.sequence, 2);
    assert_ne!(next.id, wave.id);
}

#[test]
fn missing_or_corrupt_wave_is_not_an_execution_order() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
    f.store
        .connection()
        .execute(
            "UPDATE local_waves SET body=?2 WHERE wave=?1",
            params![wave.id.as_str(), b"{}".to_vec()],
        )
        .unwrap();
    assert!(load(&f.store, &wave.id).is_err());
    assert!(compose(&mut f.store, &owner, f._repo.path(), &wave.id).is_err());
}

#[test]
fn a_terminal_conflict_cannot_stop_or_drift_a_later_independent_wave() {
    let mut f = Fixture::new();
    let base = f.a.base.commit.clone();
    let git = crate::local_target::tests::git;
    git(
        f._repo.path(),
        &["switch", "-qc", "conflict", base.as_str()],
    );
    std::fs::write(f._repo.path().join("one.txt"), "incompatible\n").unwrap();
    git(f._repo.path(), &["commit", "-qam", "independent conflict"]);
    let tip = CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    let conflict = archive::retain_contribution(&mut f.store, f._repo.path(), &base, &tip).unwrap();
    let mut spec = f.spec();
    spec.contributions[0].atomic_group = Some("collision".into());
    spec.contributions.push(target::RequestedContribution {
        id: "C".into(),
        lineage: conflict.lineage_record_id.to_string(),
        requires: vec![],
        atomic_group: Some("collision".into()),
        revision_of: None,
        derived_from: vec![],
    });
    spec.selected = vec!["A".into(), "B".into(), "C".into()];
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    for (i, c) in ["A", "C", "B"].iter().enumerate() {
        available(&mut f.store, &owner, c, i as i64).unwrap();
    }
    let p = WavePolicy {
        batch_width: 1,
        ..policy()
    };
    let older = frozen(admit(&mut f.store, &owner, p, 2).unwrap());
    assert_eq!(older.spec.selected, ["A", "C"]);
    let older = compose(&mut f.store, &owner, f._repo.path(), &older.id).unwrap();
    assert_eq!(older.phase, WavePhase::Blocked);
    let request_ref = older
        .resolution_request
        .as_ref()
        .expect("real composer conflict stores its request")
        .clone();
    let conflict_state = target::status(&f.store, owner.target()).unwrap();
    require_current_resolution_request(
        &mut f.store,
        f._repo.path(),
        &owner,
        &older.id,
        conflict_state.version,
        &request_ref,
    )
    .unwrap();

    let newer = frozen(admit(&mut f.store, &owner, p, 3).unwrap());
    assert_eq!(newer.spec.selected, ["B"]);
    assert_eq!(newer.spec.sequence, 2);
    let state = target::status(&f.store, owner.target()).unwrap();
    assert!(matches!(
        stop(&mut f.store, &owner, &older.id),
        Err(WaveError::Stale)
    ));
    let moved = CommitOid::parse(&"4".repeat(40)).unwrap();
    assert!(matches!(
        observe_target(&mut f.store, &owner, &older.id, &moved),
        Err(WaveError::Stale)
    ));
    assert_eq!(target::status(&f.store, owner.target()).unwrap(), state);
    assert_eq!(load(&f.store, &newer.id).unwrap(), newer);
    assert_eq!(
        compose(&mut f.store, &owner, f._repo.path(), &newer.id)
            .unwrap()
            .phase,
        WavePhase::Verifying
    );
    let current_state = target::status(&f.store, owner.target()).unwrap();
    assert!(matches!(
        require_current_resolution_request(
            &mut f.store,
            f._repo.path(),
            &owner,
            &older.id,
            current_state.version,
            &request_ref,
        ),
        Err(WaveError::Stale)
    ));
}

#[test]
fn drift_cannot_release_a_composing_or_recovered_unknown_dispatch() {
    for recovered in [false, true] {
        let mut f = Fixture::new();
        let (owner, revision) = setup(&mut f, &["A"]);
        available(&mut f.store, &owner, "A", 0).unwrap();
        let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
        f.store
            .connection()
            .execute(
                "UPDATE local_waves SET phase='composing' WHERE wave=?1",
                [wave.id.as_str()],
            )
            .unwrap();
        f.store.connection().execute("UPDATE local_targets SET phase='composing', version=version+1 WHERE target='target'", []).unwrap();
        if recovered {
            recover_dispatch(&mut f.store, &owner, &wave.id).unwrap();
        }
        for digit in ["4", "5", "6"] {
            let moved = CommitOid::parse(&digit.repeat(40)).unwrap();
            observe_target(&mut f.store, &owner, &wave.id, &moved).unwrap();
        }
        assert_eq!(
            load(&f.store, &wave.id).unwrap().phase,
            WavePhase::OutcomeUnknown
        );
        assert_eq!(
            target::status(&f.store, owner.target()).unwrap().phase,
            Phase::BlockedTargetDrift
        );
        let mut spec = f.spec();
        spec.selected = vec!["B".into()];
        let next = target::propose_revision(&mut f.store, &owner, spec).unwrap();
        assert!(matches!(
            target::select_revision(&mut f.store, &owner, &next.id),
            Err(TargetError::Frozen)
        ));
        assert!(matches!(
            target::release_revision(&mut f.store, &owner, &revision),
            Err(TargetError::Frozen)
        ));
        assert!(matches!(
            admit(&mut f.store, &owner, policy(), 1).unwrap(),
            Admission::Existing(_)
        ));
        assert!(matches!(
            compose(&mut f.store, &owner, f._repo.path(), &wave.id),
            Err(WaveError::OutcomeUnknown)
        ));
        assert_eq!(
            stop(&mut f.store, &owner, &wave.id).unwrap().phase,
            WavePhase::OutcomeUnknown
        );
    }
}

#[test]
fn a_dispatch_state_change_invalidates_a_pre_dispatch_stop_observation() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
    let read_before_dispatch = target::status(&f.store, owner.target()).unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_waves SET phase='composing' WHERE wave=?1",
            [wave.id.as_str()],
        )
        .unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='composing', version=version+1 WHERE target='target'",
            [],
        )
        .unwrap();
    assert!(matches!(
        commit_stop(&mut f.store, &owner, &wave, &read_before_dispatch),
        Err(WaveError::Stale)
    ));
    assert_eq!(
        load(&f.store, &wave.id).unwrap().phase,
        WavePhase::Composing
    );
    assert_eq!(
        stop(&mut f.store, &owner, &wave.id).unwrap().phase,
        WavePhase::OutcomeUnknown
    );
}

#[test]
fn returning_to_the_baseline_does_not_reset_or_bypass_drift_allowance() {
    let mut f = Fixture::new();
    let (owner, _) = setup(&mut f, &["A"]);
    available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
    let base = f.a.base.commit.clone();
    let moved = CommitOid::parse(&"4".repeat(40)).unwrap();
    for expected in 1..=3 {
        let departure = observe_target(&mut f.store, &owner, &wave.id, &moved).unwrap();
        assert_eq!(departure.drift_count, expected);
        if expected < 3 {
            assert_eq!(
                observe_target(&mut f.store, &owner, &wave.id, &base)
                    .unwrap()
                    .drift_count,
                expected
            );
        } else {
            assert_eq!(departure.phase, WavePhase::TargetDrift);
        }
    }
}

#[test]
fn rehashed_wave_records_cannot_expand_the_pinned_authorization() {
    for change in [
        "excluded",
        "baseline",
        "objective",
        "sequence",
        "duplicate",
        "issuer",
        "profile",
    ] {
        let mut f = Fixture::new();
        let (owner, _) = setup(&mut f, &["A"]);
        available(&mut f.store, &owner, "A", 0).unwrap();
        let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
        let mut spec = wave.spec.clone();
        match change {
            "excluded" => spec.selected = vec!["B".into()],
            "baseline" => spec.baseline = f.b.result.snapshot_id.to_string(),
            "objective" => spec.objective_authorization = format!("sha256:{}", "8".repeat(64)),
            "sequence" => spec.sequence += 1,
            "duplicate" => spec.selected.push("A".into()),
            "issuer" => spec.issuer = "other".into(),
            "profile" => {
                let mut target = f.spec();
                target.composer_profile = "unknown".into();
                spec.revision = target::propose_revision(&mut f.store, &owner, target)
                    .unwrap()
                    .id
                    .to_string();
            }
            _ => unreachable!(),
        }
        let bytes =
            serde_json::to_vec(&serde_json::json!({"schema":SCHEMA_NAME,"spec":spec})).unwrap();
        let record = Record::decode(&bytes, &[&SCHEMA]).unwrap();
        f.store
            .connection()
            .execute(
                "UPDATE local_waves SET wave=?2, body=?3, revision=?4 WHERE wave=?1",
                params![
                    wave.id.as_str(),
                    record.id().as_str(),
                    record.canonical_bytes(),
                    spec.revision
                ],
            )
            .unwrap();
        assert!(
            matches!(load(&f.store, &record.id()), Err(WaveError::Invalid(_))),
            "{change}"
        );
        assert!(
            matches!(
                compose(&mut f.store, &owner, f._repo.path(), &record.id()),
                Err(WaveError::Invalid(_))
            ),
            "{change}"
        );
        assert_eq!(
            target::status(&f.store, owner.target()).unwrap().phase,
            Phase::WaveFrozen
        );
    }
}

#[test]
fn interrupted_dispatch_recovers_after_generic_stop_or_drift_without_clearing_intent() {
    for phase in [
        Phase::StopRequested,
        Phase::Stopped,
        Phase::BlockedTargetDrift,
    ] {
        let mut f = Fixture::new();
        let (owner, revision) = setup(&mut f, &["A"]);
        available(&mut f.store, &owner, "A", 0).unwrap();
        let wave = frozen(admit(&mut f.store, &owner, policy(), 0).unwrap());
        f.store
            .connection()
            .execute(
                "UPDATE local_waves SET phase='composing' WHERE wave=?1",
                [wave.id.as_str()],
            )
            .unwrap();
        let state = target::status(&f.store, owner.target()).unwrap();
        let composing =
            target::transition(&mut f.store, &owner, state.version, Phase::Composing).unwrap();
        let next = if phase == Phase::Stopped {
            Phase::StopRequested
        } else {
            phase
        };
        let intent = target::transition(&mut f.store, &owner, composing.version, next).unwrap();
        if phase == Phase::Stopped {
            target::transition(&mut f.store, &owner, intent.version, Phase::Stopped).unwrap();
        }
        assert_eq!(
            recover_dispatch(&mut f.store, &owner, &wave.id)
                .unwrap()
                .phase,
            WavePhase::OutcomeUnknown
        );
        let expected = if phase == Phase::Stopped {
            Phase::StopRequested
        } else {
            phase
        };
        assert_eq!(
            target::status(&f.store, owner.target()).unwrap().phase,
            expected
        );
        assert_eq!(
            recover_dispatch(&mut f.store, &owner, &wave.id)
                .unwrap()
                .phase,
            WavePhase::OutcomeUnknown
        );
        assert!(matches!(
            target::release_revision(&mut f.store, &owner, &revision),
            Err(TargetError::Frozen)
        ));
        assert!(matches!(
            admit(&mut f.store, &owner, policy(), 1).unwrap(),
            Admission::Existing(_)
        ));
    }
}
