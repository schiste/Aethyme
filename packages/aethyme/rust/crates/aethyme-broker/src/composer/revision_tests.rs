use super::*;

fn composition_request(deliveries: Vec<String>) -> CompositionRequest {
    CompositionRequest {
        baseline: CommitOid::parse(&"0".repeat(40)).expect("commit id"),
        accepted: Vec::new(),
        catalog: vec![ContributionSpec {
            id: "A".into(),
            lineage: None,
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        }],
        deliveries,
        budget: CompositionBudget::default(),
    }
}

fn revision_id(digit: char) -> RecordId {
    RecordId::parse(&format!("sha256:{}", digit.to_string().repeat(64))).expect("record id")
}

#[test]
fn unbound_recipe_order_keeps_the_legacy_shape() {
    let order = recipe_order_entry("A", "sha256:lineage", "sha256:base", "sha256:result", None);
    assert_eq!(
        order,
        object(vec![
            ("id", text("A")),
            ("lineage", text("sha256:lineage")),
            ("base", text("sha256:base")),
            ("result", text("sha256:result")),
        ])
    );
}

#[test]
fn bound_recipe_order_carries_the_exact_immutable_revision_id() {
    let order = recipe_order_entry(
        "A",
        "sha256:lineage",
        "sha256:base",
        "sha256:result",
        Some(revision_id('a').as_str()),
    );
    assert_eq!(
        order,
        object(vec![
            ("id", text("A")),
            ("lineage", text("sha256:lineage")),
            ("base", text("sha256:base")),
            ("result", text("sha256:result")),
            ("revision", text(&format!("sha256:{}", "a".repeat(64)))),
        ])
    );
}

#[test]
fn revision_binding_map_is_exact_for_every_delivery() {
    let request = composition_request(vec!["A".into()]);
    assert!(
        validate_revision_bindings(
            &request,
            &request.deliveries,
            &BTreeMap::from([("A".into(), revision_id('a'))]),
        )
        .is_ok()
    );
    assert!(validate_revision_bindings(&request, &request.deliveries, &BTreeMap::new()).is_err());
    assert!(
        validate_revision_bindings(
            &request,
            &request.deliveries,
            &BTreeMap::from([("outside-catalog".into(), revision_id('a'))]),
        )
        .is_err()
    );
}

#[test]
fn one_revision_cannot_be_pinned_to_two_contributions() {
    let mut request = composition_request(vec!["A".into()]);
    request.catalog.push(ContributionSpec {
        id: "B".into(),
        lineage: None,
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    });
    let revision = revision_id('a');
    assert!(
        validate_revision_bindings(
            &request,
            &request.deliveries,
            &BTreeMap::from([("A".into(), revision.clone()), ("B".into(), revision)]),
        )
        .is_err()
    );
}

#[test]
fn synthesized_middle_conflict_replays_the_exact_selected_suffix() {
    use crate::{local_target as target, local_wave as wave, resolution};

    let mut fixture = crate::local_target::tests::Fixture::new();
    let repo = fixture._repo.path();
    let baseline = fixture.a.base.commit.clone();
    let git = crate::local_target::tests::git;

    git(repo, &["switch", "-qc", "B-conflict", baseline.as_str()]);
    std::fs::write(repo.join("one.txt"), "B\n").unwrap();
    git(repo, &["add", "one.txt"]);
    git(repo, &["commit", "-qm", "B conflicts with A"]);
    let b_commit = CommitOid::parse(&git(repo, &["rev-parse", "HEAD"])).unwrap();
    fixture.b = crate::collaboration_archive::retain_contribution(
        &mut fixture.store,
        repo,
        &baseline,
        &b_commit,
    )
    .unwrap();
    let c = fixture.feature(&baseline);

    let owner = target::acquire_controller(&mut fixture.store, "tail", "appointed").unwrap();
    let mut spec = fixture.spec();
    spec.contributions.push(c);
    spec.selected = vec!["A".into(), "B".into(), "C".into()];
    let revision = target::propose_revision(&mut fixture.store, &owner, spec).unwrap();
    target::select_revision(&mut fixture.store, &owner, &revision.id).unwrap();
    for (index, contribution) in ["A", "B", "C"].iter().enumerate() {
        wave::available(&mut fixture.store, &owner, contribution, index as i64).unwrap();
    }
    let wave = match wave::tick(
        &mut fixture.store,
        &owner,
        repo,
        wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        3,
    )
    .unwrap()
    {
        wave::Run::Blocked(wave) => wave,
        other => panic!("the A/B line collision must block the full A/B/C wave: {other:?}"),
    };
    let request_ref = wave
        .resolution_request
        .clone()
        .expect("the real composition conflict attaches a request");
    let expected_version = target::status(&fixture.store, owner.target())
        .unwrap()
        .version;
    let binding = wave::current_conflict_request_binding(
        &mut fixture.store,
        repo,
        &owner,
        &wave.id,
        expected_version,
        &request_ref,
    )
    .unwrap();
    assert_eq!(binding.plan_payload.selected, ["A", "B", "C"]);

    let session_label = "middle-conflict-tail-test";
    let started = resolution::begin_attempt(
        &mut fixture.store,
        repo,
        &request_ref,
        session_label,
        "middle-conflict-tail-client",
        resolution::DispatchLimits {
            max_manifest_bytes: 1_000_000,
            max_manifest_entries: 256,
            max_file_bytes: 1_000_000,
            max_total_input_bytes: 4_000_000,
            max_response_bytes: 1_000_000,
        },
    )
    .unwrap();
    let resolution::BeginResolutionAttempt::Started { status, .. } = started else {
        panic!("the focused test must create one charged attempt");
    };
    resolution::mark_attempt_dispatching(&mut fixture.store, &status.token, session_label).unwrap();
    let envelope = serde_json::json!({
        "schema": resolution::ASYNC_RESPONSE_ENVELOPE_SCHEMA,
        "attempt_token": status.token,
        "session_label": session_label,
        "request_record_id": status.request.id.as_str(),
        "request_object_sha256": status.request.object.hex(),
        "input_manifest_sha256": status.input_manifest_sha256,
        "response": {
            "kind": "proposal",
            "detail": null,
            "brief": "resolve the A/B prefix",
            "writes": [{
                "path": "one.txt",
                "delete": false,
                "kind": "regular",
                "bytes_hex": "41420a"
            }]
        }
    });
    let response = serde_json::to_vec(&envelope).unwrap();
    let bound = resolution::decode_attempt_response_envelope(&status, &response).unwrap();
    let settled = resolution::finish_bound_attempt(&mut fixture.store, repo, &bound).unwrap();
    assert!(matches!(
        settled.outcome,
        resolution::ResolutionOutcome::Synthesized(_)
    ));
    let ledger = resolution::attempt_status(&fixture.store, &status.token, session_label).unwrap();
    assert_eq!(ledger.state, resolution::ResolutionAttemptState::Settled);
    let result = ledger
        .result
        .expect("the settled attempt records the exact group");

    let composition = compose_after_synthesized_conflict(
        &mut fixture.store,
        repo,
        &owner,
        &wave.id,
        expected_version,
        &binding,
        &result,
    )
    .unwrap();
    let candidate = composition
        .outcome
        .candidate()
        .expect("the exact C suffix applies to the synthesized A/B prefix");
    assert_eq!(candidate.mode, CompositionMode::Hybrid);
    assert!(matches!(&candidate.producer, Producer::Composer { .. }));
    assert_eq!(candidate.inputs.len(), 3);
    assert_eq!(
        crate::local_target::tests::git(
            repo,
            &["show", &format!("{}:one.txt", candidate.commit.as_str())],
        ),
        "AB"
    );
    assert_eq!(
        crate::local_target::tests::git(
            repo,
            &["show", &format!("{}:three.txt", candidate.commit.as_str())],
        ),
        "C"
    );

    let recipe = composition
        .recipe
        .as_ref()
        .expect("the final candidate is rooted by a recipe");
    let recipe: serde_json::Value = serde_json::from_slice(
        &read_recipe(&fixture.store, recipe)
            .unwrap()
            .canonical_bytes(),
    )
    .unwrap();
    let seeded = &recipe["seeded_conflict"];
    assert_eq!(seeded["schema"], SEEDED_CONFLICT_RECIPE_SCHEMA_NAME);
    assert_eq!(seeded["trust"], "unverified");
    assert_eq!(seeded["selected"], serde_json::json!(["A", "B", "C"]));
    assert_eq!(seeded["suffix"], serde_json::json!(["C"]));
    assert_eq!(seeded["prefix_len"], 2);
    let synthesis = resolution::read_synthesized_attempt_result_for_refs(
        &mut fixture.store,
        repo,
        &binding.request,
        &result,
    )
    .unwrap();
    assert_eq!(
        seeded,
        &serde_json::from_slice::<serde_json::Value>(
            &expected_seeded_conflict_metadata(
                &binding,
                &result,
                &synthesis.request,
                &synthesis.candidate,
                &synthesis.input_manifest.sha256,
            )
            .to_canonical_bytes(),
        )
        .unwrap()
    );

    let unchanged = wave::load(&fixture.store, &wave.id).unwrap();
    assert_eq!(unchanged.phase, wave::WavePhase::Blocked);
    assert!(unchanged.resolution_result.is_none());
    assert!(unchanged.candidate_snapshot.is_none());
    assert!(unchanged.recipe_object.is_none());
}
