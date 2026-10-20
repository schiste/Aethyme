use super::*;

#[test]
fn legacy_historical_qualification_parses_but_missing_object_identity_fails_closed() {
    let legacy: HistoricalQualificationSpec = serde_json::from_value(serde_json::json!({
        "binding": "sha256:binding",
        "receipt": "sha256:receipt",
        "evidence": "sha256:evidence",
        "execution": "sha256:execution",
        "epoch": "sha256:epoch",
        "expires_ms": 1
    }))
    .unwrap();

    assert!(matches!(
        historical_object_digest(legacy.execution_object_sha256.as_deref(), "execution"),
        Err(AcceptanceError::MissingHistoricalObjectIdentity(
            "execution"
        ))
    ));
    assert!(matches!(
        historical_object_digest(
            legacy.inventory_object_sha256.as_deref(),
            "assertion inventory"
        ),
        Err(AcceptanceError::MissingHistoricalObjectIdentity(
            "assertion inventory"
        ))
    ));
    assert!(matches!(
        historical_object_digest(legacy.artifact_object_sha256.as_deref(), "sealed artifact"),
        Err(AcceptanceError::MissingHistoricalObjectIdentity(
            "sealed artifact"
        ))
    ));
}

fn oid() -> String {
    "a".repeat(40)
}
fn pr() -> serde_json::Value {
    serde_json::json!({"number":4,"head":{"sha":oid()},"base":{"ref":"main","repo":{"full_name":"operator/Playground"}},"user":{"login":"author"},"merged":true,"merged_at":"2026-10-11T00:00:00Z","merge_commit_sha":"b".repeat(40),"merged_by":{"login":"mergify[bot]"},"state":"closed","draft":false})
}
#[test]
fn synthetic_test_merge_is_never_acceptance() {
    let mut p = pr();
    p["merged"] = false.into();
    p["state"] = "open".into();
    p["merged_at"] = serde_json::Value::Null;
    assert_eq!(
        provider_decision(&p, &ProviderRoute::Github { pull_request: 4 }).unwrap(),
        (ProviderDecision::Waiting, None)
    );
    p["state"] = "closed".into();
    assert_eq!(
        provider_decision(&p, &ProviderRoute::Github { pull_request: 4 }).unwrap(),
        (ProviderDecision::Rejected, None)
    );
}
#[test]
fn exact_provider_subject_and_actual_merge_are_required() {
    let p = pr();
    assert!(pr_subject(&p, "operator/Playground", 4, &oid(), "main").is_ok());
    for path in ["/head/sha", "/base/ref", "/base/repo/full_name"] {
        let mut bad = p.clone();
        *bad.pointer_mut(path).unwrap() = "wrong".into();
        assert!(pr_subject(&bad, "operator/Playground", 4, &oid(), "main").is_err());
    }
    let mut bad = p.clone();
    bad["number"] = 5.into();
    assert!(pr_subject(&bad, "operator/Playground", 4, &oid(), "main").is_err());
    for key in ["merged_at", "merge_commit_sha", "draft"] {
        let mut bad = p.clone();
        bad[key] = serde_json::Value::Null;
        assert!(provider_decision(&bad, &ProviderRoute::Github { pull_request: 4 }).is_err());
    }
    assert_eq!(
        provider_decision(
            &p,
            &ProviderRoute::Mergify {
                pull_request: 4,
                merger: "mergify[bot]".into()
            }
        )
        .unwrap()
        .0,
        ProviderDecision::Accepted
    );
    assert!(
        provider_decision(
            &p,
            &ProviderRoute::Mergify {
                pull_request: 4,
                merger: "another[bot]".into()
            }
        )
        .is_err()
    );
}
#[test]
fn approvals_are_exact_latest_distinct_and_exclude_author() {
    let policy = crate::ReviewPolicy {
        enabled: true,
        required_approvals: 1,
        ..Default::default()
    };
    let approval = serde_json::json!({"id":1,"user":{"id":10,"login":"reviewer"},"state":"APPROVED","commit_id":oid()});
    assert!(
        review_pass(
            &policy,
            &oid(),
            "author",
            Some("APPROVED"),
            std::slice::from_ref(&approval),
            &[]
        )
        .unwrap()
    );
    assert!(
        !review_pass(
            &policy,
            &oid(),
            "reviewer",
            Some("APPROVED"),
            std::slice::from_ref(&approval),
            &[]
        )
        .unwrap()
    );
    let mut wrong = approval.clone();
    wrong["commit_id"] = "b".repeat(40).into();
    assert!(!review_pass(&policy, &oid(), "author", Some("APPROVED"), &[wrong], &[]).unwrap());
    let mut dismissed = approval.clone();
    dismissed["id"] = 2.into();
    dismissed["state"] = "DISMISSED".into();
    assert!(
        !review_pass(
            &policy,
            &oid(),
            "author",
            Some("APPROVED"),
            &[dismissed, approval.clone()],
            &[]
        )
        .unwrap()
    );
    assert!(
        review_pass(
            &policy,
            &oid(),
            "author",
            Some("APPROVED"),
            &[approval.clone(), approval],
            &[]
        )
        .is_err()
    );
}
#[test]
fn missing_stale_wrong_app_and_ambiguous_checks_cannot_pass() {
    let policy = crate::ReviewPolicy {
        enabled: true,
        evidence_adapter: crate::ReviewEvidenceAdapter::GithubCheckRun,
        evidence_check_name: Some("review".into()),
        evidence_app_slug: Some("appointed".into()),
        ..Default::default()
    };
    let check = serde_json::json!({"name":"review","app":{"slug":"appointed"},"head_sha":oid(),"status":"completed","conclusion":"success"});
    assert!(
        review_pass(
            &policy,
            &oid(),
            "author",
            Some("APPROVED"),
            &[],
            std::slice::from_ref(&check)
        )
        .unwrap()
    );
    assert!(!review_pass(&policy, &oid(), "author", Some("APPROVED"), &[], &[]).unwrap());
    for path in ["/head_sha", "/app/slug", "/status", "/conclusion"] {
        let mut bad = check.clone();
        *bad.pointer_mut(path).unwrap() = "wrong".into();
        assert!(!review_pass(&policy, &oid(), "author", Some("APPROVED"), &[], &[bad]).unwrap());
    }
    assert!(
        !review_pass(
            &policy,
            &oid(),
            "author",
            Some("APPROVED"),
            &[],
            &[check.clone(), check]
        )
        .unwrap()
    );
}

use crate::local_target::tests::{Fixture, git};
struct Subject {
    f: Fixture,
    _remote: tempfile::TempDir,
    owner: ControllerOwner,
    broker: Broker,
    carrier: GitCarrier,
    intent: Intent,
    binding: AcceptanceBinding,
}
impl Subject {
    fn new() -> Self {
        let mut f = Fixture::new();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(
            f._repo.path(),
            &[
                "remote",
                "add",
                "publication",
                remote.path().to_str().unwrap(),
            ],
        );
        let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
        let spec = f.spec();
        let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
        target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
        crate::local_wave::available(&mut f.store, &owner, "A", 0).unwrap();
        let w = match crate::local_wave::admit(
            &mut f.store,
            &owner,
            crate::local_wave::WavePolicy {
                debounce_ms: 0,
                ..Default::default()
            },
            1,
        )
        .unwrap()
        {
            crate::local_wave::Admission::Frozen(w) => w,
            _ => panic!(),
        };
        let w = crate::local_wave::compose(&mut f.store, &owner, f._repo.path(), &w.id).unwrap();
        let version = target::status(&f.store, "target").unwrap().version;
        target::resume(&mut f.store, &owner, version).unwrap();
        let mut broker = Broker::open(f._repo.path())
            .unwrap()
            .with_host_operation_database(f._host.path().join("host-operations.db"))
            .with_worktree_root(f._host.path().join("worktrees"));
        let session = broker
            .adopt(f._repo.path(), Some("acceptance Playground fixture"))
            .unwrap();
        let carrier = GitCarrier::new(
            &mut broker,
            session.id,
            "publication",
            "refs/heads/accepted",
            30,
            "operator authorizes exact Playground source route",
            true,
        )
        .unwrap();
        let baseline = f.a.base.commit.as_str();
        // This setup write is owned by the isolated disposable fixture, not product
        // publication. The implementation reads and fetches through native services.
        git(
            f._repo.path(),
            &[
                "push",
                "-q",
                "publication",
                &format!("{baseline}:refs/heads/accepted"),
            ],
        );
        let intent = transport::prepare(
            &mut f.store,
            &owner,
            &carrier,
            &w.id,
            &"a".repeat(64),
            Some(&CommitOid::parse(baseline).unwrap()),
        )
        .unwrap();
        let binding = appoint(
            &mut f.store,
            &owner,
            &mut broker,
            &carrier,
            &intent.id,
            ProviderRoute::GenericGit,
            "refs/heads/accepted",
            8,
        )
        .unwrap();
        Self {
            f,
            _remote: remote,
            owner,
            broker,
            carrier,
            intent,
            binding,
        }
    }
    fn observe(&mut self) -> AcceptanceReceipt {
        observe(
            &mut self.f.store,
            &self.owner,
            &mut self.broker,
            &self.carrier,
            self.binding.id(),
        )
        .unwrap()
    }
    fn publish(&mut self) {
        transport::publish(
            &mut self.f.store,
            &self.owner,
            &self.carrier,
            &mut self.broker,
            &self.intent.id,
        )
        .unwrap();
    }
    fn fixture_route(&mut self, route: ProviderRoute) {
        self.binding.spec.route = route;
        let r = canonical(&BINDING, &self.binding.spec).unwrap();
        self.binding.id = r.id();
        self.f
            .store
            .connection()
            .execute(
                "INSERT INTO local_acceptance_bindings(id,revision,body) VALUES(?1,?2,?3)",
                params![
                    r.id().as_str(),
                    self.binding.spec.revision,
                    r.canonical_bytes()
                ],
            )
            .unwrap();
    }
    fn provider(&self, accepted: &str) -> ProviderFixture {
        let mut p = pr();
        p["head"]["sha"] = self.intent.spec.commit.clone().into();
        p["base"]["repo"]["full_name"] = self.carrier.identity.repository_path.clone().into();
        p["merge_commit_sha"] = accepted.into();
        p["base"]["ref"] = "accepted".into();
        ProviderFixture {
            pr: p,
            reviews: vec![],
            checks: vec![],
            aggregate: Some("APPROVED".into()),
        }
    }
}
#[test]
fn actual_generic_ref_is_observed_without_acceptance_permission_or_checkout_mutation() {
    let mut s = Subject::new();
    let head = git(s.f._repo.path(), &["rev-parse", "HEAD"]);
    let before = s.observe();
    assert_eq!(before.spec.decision, ProviderDecision::Waiting);
    s.publish();
    let result = s.observe();
    assert_eq!(result.spec.decision, ProviderDecision::Accepted);
    assert_eq!(
        result.spec.accepted_commit.as_deref(),
        Some(s.intent.spec.commit.as_str())
    );
    assert_eq!(
        last_receipt(&s.f.store, s.binding.id()).unwrap(),
        Some(result)
    );
    assert_eq!(git(s.f._repo.path(), &["rev-parse", "HEAD"]), head);
    assert!(!s.f._repo.path().join(".git/FETCH_HEAD").exists());
    assert_eq!(
        target::status(&s.f.store, s.owner.target()).unwrap().phase,
        target::Phase::Verifying
    );
    let count: i64 =
        s.f.store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_acceptance_qualifications",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn merge_squash_and_rebase_representations_require_complete_exact_source() {
    for mode in ["merge", "squash", "rebase"] {
        let mut s = Subject::new();
        let tree = s.intent.spec.tree.clone();
        let accepted = git(
            s.f._repo.path(),
            &[
                "commit-tree",
                &tree,
                "-p",
                &s.binding.spec.policy_commit,
                "-m",
                mode,
            ],
        );
        git(
            s.f._repo.path(),
            &[
                "push",
                "-q",
                "publication",
                &format!("{accepted}:refs/heads/accepted"),
            ],
        );
        s.fixture_route(ProviderRoute::Github { pull_request: 4 });
        let fixture = s.provider(&accepted);
        let receipt = observe_with(
            &mut s.f.store,
            &s.owner,
            &mut s.broker,
            &s.carrier,
            s.binding.id(),
            Some(&fixture),
        )
        .unwrap();
        assert_eq!(receipt.spec.decision, ProviderDecision::Accepted, "{mode}");
        assert_eq!(receipt.spec.accepted_tree.as_deref(), Some(tree.as_str()));
        assert_ne!(accepted, s.intent.spec.commit);
    }
}
#[test]
fn external_main_or_wrong_source_requires_revalidation_not_transferred_approval() {
    let mut s = Subject::new();
    s.fixture_route(ProviderRoute::Github { pull_request: 4 });
    let wrong = s.f.b.result.commit.as_str().to_string();
    git(
        s.f._repo.path(),
        &[
            "push",
            "-q",
            "--force",
            "publication",
            &format!("{wrong}:refs/heads/accepted"),
        ],
    );
    let fixture = s.provider(&wrong);
    let result = observe_with(
        &mut s.f.store,
        &s.owner,
        &mut s.broker,
        &s.carrier,
        s.binding.id(),
        Some(&fixture),
    )
    .unwrap();
    assert_eq!(result.spec.decision, ProviderDecision::TargetDrift);
    let fixture = s.provider(&s.intent.spec.commit);
    let result = observe_with(
        &mut s.f.store,
        &s.owner,
        &mut s.broker,
        &s.carrier,
        s.binding.id(),
        Some(&fixture),
    )
    .unwrap();
    assert_eq!(result.spec.decision, ProviderDecision::TargetDrift);
}
#[test]
fn observer_is_finite_and_imported_receipts_are_only_history() {
    let mut s = Subject::new();
    for _ in 0..8 {
        s.observe();
    }
    assert!(matches!(
        observe(
            &mut s.f.store,
            &s.owner,
            &mut s.broker,
            &s.carrier,
            s.binding.id()
        ),
        Err(AcceptanceError::Exhausted)
    ));
    let receipt = last_receipt(&s.f.store, s.binding.id()).unwrap().unwrap();
    let mut forged = receipt.spec.clone();
    forged.source = format!("sha256:{}", "b".repeat(64));
    let rec = canonical(&RECEIPT, &forged).unwrap();
    s.f.store
        .connection()
        .execute(
            "INSERT INTO local_acceptance_receipts VALUES(?1,?2,?3)",
            params![
                rec.id().as_str(),
                s.binding.id().as_str(),
                rec.canonical_bytes()
            ],
        )
        .unwrap();
    s.f.store
        .connection()
        .execute(
            "UPDATE local_acceptance_bindings SET last_receipt=?2 WHERE id=?1",
            params![s.binding.id().as_str(), rec.id().as_str()],
        )
        .unwrap();
    assert!(last_receipt(&s.f.store, s.binding.id()).is_err());
}
#[test]
fn stopped_historical_observation_never_delivers_or_restarts_a_target() {
    let mut s = Subject::new();
    s.publish();
    let st = target::status(&s.f.store, s.owner.target()).unwrap();
    let st = target::transition(
        &mut s.f.store,
        &s.owner,
        st.version,
        target::Phase::StopRequested,
    )
    .unwrap();
    target::transition(&mut s.f.store, &s.owner, st.version, target::Phase::Stopped).unwrap();
    assert_eq!(s.observe().spec.decision, ProviderDecision::Accepted);
    assert_eq!(
        target::status(&s.f.store, s.owner.target()).unwrap().phase,
        target::Phase::Stopped
    );
}

fn accepted_wave_fixture() -> (
    verifier::tests::Subject,
    tempfile::TempDir,
    Broker,
    GitCarrier,
    RecordId,
    Intent,
    AcceptanceBinding,
    AcceptanceReceipt,
    TrustedEvidence,
) {
    let mut v = verifier::tests::Subject::acceptance_fixture();
    // The existing verifier test fixture supplies test-only sealed capabilities.
    // No production API can manufacture those capabilities from imported bytes.
    v.f.store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='idle' WHERE target=?1",
            [v.owner.target()],
        )
        .unwrap();
    crate::local_wave::available(&mut v.f.store, &v.owner, "A", 0).unwrap();
    let w = match crate::local_wave::admit(
        &mut v.f.store,
        &v.owner,
        crate::local_wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        1,
    )
    .unwrap()
    {
        crate::local_wave::Admission::Frozen(w) => w,
        _ => panic!(),
    };
    let w = crate::local_wave::compose(&mut v.f.store, &v.owner, v.f._repo.path(), &w.id).unwrap();
    let st = target::status(&v.f.store, v.owner.target()).unwrap();
    target::resume(&mut v.f.store, &v.owner, st.version).unwrap();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        v.f._repo.path(),
        &[
            "remote",
            "add",
            "publication",
            remote.path().to_str().unwrap(),
        ],
    );
    let fixture_owner = v.f._host.path().canonicalize().unwrap();
    let host_cache = fixture_owner.join("broker-host-cache");
    let preparation_cache = fixture_owner.join("preparation-cache");
    let worktree_root = fixture_owner.join("worktrees");
    let host_operation_db = fixture_owner.join("host-operations.db");
    let collaboration_root =
        crate::collaboration_state::CollaborationRoot::under_host_state(&fixture_owner)
            .path()
            .to_path_buf();
    for path in [
        &host_cache,
        &preparation_cache,
        &worktree_root,
        &collaboration_root,
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    let mut broker = Broker::open_with_options(
        v.f._repo.path(),
        crate::BrokerOpenOptions::default()
            .with_host_state_dir(&fixture_owner)
            .with_host_cache_dir(&host_cache)
            .with_preparation_cache_root(&preparation_cache)
            .with_host_operation_database(host_operation_db)
            .with_worktree_root(worktree_root)
            .with_collaboration_state(collaboration_root),
    )
    .unwrap();
    let session = broker
        .adopt(v.f._repo.path(), Some("trusted acceptance fixture"))
        .unwrap();
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "publication",
        "refs/heads/accepted",
        30,
        "operator authorizes disposable Playground acceptance",
        true,
    )
    .unwrap();
    let base = v.f.a.base.commit.clone();
    git(
        v.f._repo.path(),
        &[
            "push",
            "-q",
            "publication",
            &format!("{}:refs/heads/accepted", base.as_str()),
        ],
    );
    let intent = transport::prepare(
        &mut v.f.store,
        &v.owner,
        &carrier,
        &w.id,
        &"a".repeat(64),
        Some(&base),
    )
    .unwrap();
    let binding = appoint(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        &intent.id,
        ProviderRoute::GenericGit,
        "refs/heads/accepted",
        8,
    )
    .unwrap();
    transport::publish(&mut v.f.store, &v.owner, &carrier, &mut broker, &intent.id).unwrap();
    let receipt = observe(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        binding.id(),
    )
    .unwrap();
    assert_eq!(receipt.spec.decision, ProviderDecision::Accepted);

    // The test artifact is synthetic. Retain its manifest and file objects the
    // same way the native supervisor retains a real build output closure.
    let mut output_objects = v
        .artifact
        .output
        .files
        .values()
        .map(|bytes| archive::put_object(&v.f.store, bytes).unwrap())
        .collect::<Vec<_>>();
    output_objects.push(
        archive::put_object(&v.f.store, &v.artifact.output.manifest.manifest_bytes()).unwrap(),
    );
    let revision_id = v.execution.target_revision().clone();
    target::retain_artifacts(&mut v.f.store, &v.owner, &revision_id, &[], &output_objects).unwrap();
    let evidence = v.evidence();
    (
        v, remote, broker, carrier, w.id, intent, binding, receipt, evidence,
    )
}

fn settle_test_synthesis(
    v: &mut verifier::tests::Subject,
    repo: &std::path::Path,
    request: &crate::resolution::RecordRef,
    session_label: &str,
    path: &str,
    bytes_hex: &str,
) -> (crate::resolution::RecordRef, String) {
    let started = crate::resolution::begin_attempt(
        &mut v.f.store,
        repo,
        request,
        session_label,
        "local-accepted-history-test",
        crate::resolution::DispatchLimits {
            max_manifest_bytes: 1_000_000,
            max_manifest_entries: 256,
            max_file_bytes: 1_000_000,
            max_total_input_bytes: 4_000_000,
            max_response_bytes: 1_000_000,
        },
    )
    .unwrap();
    let crate::resolution::BeginResolutionAttempt::Started { status, .. } = started else {
        panic!("the history test must create exactly one resolver attempt");
    };
    let token = status.token.clone();
    crate::resolution::mark_attempt_dispatching(&mut v.f.store, &status.token, session_label)
        .unwrap();
    let envelope = serde_json::json!({
        "schema": crate::resolution::ASYNC_RESPONSE_ENVELOPE_SCHEMA,
        "attempt_token": status.token,
        "session_label": session_label,
        "request_record_id": status.request.id.as_str(),
        "request_object_sha256": status.request.object.hex(),
        "input_manifest_sha256": status.input_manifest_sha256,
        "response": {
            "kind": "proposal",
            "detail": null,
            "brief": "resolve the frozen Local conflict",
            "writes": [{
                "path": path,
                "delete": false,
                "kind": "regular",
                "bytes_hex": bytes_hex
            }]
        }
    });
    let bound = crate::resolution::decode_attempt_response_envelope(
        &status,
        &serde_json::to_vec(&envelope).unwrap(),
    )
    .unwrap();
    let settled = crate::resolution::finish_bound_attempt(&mut v.f.store, repo, &bound).unwrap();
    assert!(matches!(
        settled.outcome,
        crate::resolution::ResolutionOutcome::Synthesized(_)
    ));
    let ledger =
        crate::resolution::attempt_status(&v.f.store, &status.token, session_label).unwrap();
    assert_eq!(
        ledger.state,
        crate::resolution::ResolutionAttemptState::Settled
    );
    assert_eq!(&ledger.request, request);
    (
        ledger
            .result
            .expect("the settled attempt retains its exact result"),
        token,
    )
}

fn direct_synthesized_wave_fixture() -> (verifier::tests::Subject, RecordId) {
    let mut v = verifier::tests::Subject::acceptance_fixture();
    let repo = v.f._repo.path().to_path_buf();
    v.f.store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='idle',resume_phase=NULL WHERE target=?1",
            [v.owner.target()],
        )
        .unwrap();
    crate::local_wave::available(&mut v.f.store, &v.owner, "A", 0).unwrap();
    let frozen = match crate::local_wave::admit(
        &mut v.f.store,
        &v.owner,
        crate::local_wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        1,
    )
    .unwrap()
    {
        crate::local_wave::Admission::Frozen(wave) => wave,
        other => panic!("direct synthesis wave was not frozen: {other:?}"),
    };
    crate::local_wave::compose(&mut v.f.store, &v.owner, &repo, &frozen.id).unwrap();
    let composed =
        crate::local_wave::compose_with_source(&mut v.f.store, &v.owner, &repo, &frozen.id)
            .unwrap();
    let source = composed
        .source
        .expect("direct synthesis retains its original text composition source");
    let direct_candidate = source
        .composition
        .outcome
        .candidate()
        .expect("direct synthesis starts from a complete text candidate");
    assert_eq!(
        direct_candidate.mode,
        crate::composition::CompositionMode::Text
    );
    assert_eq!(
        direct_candidate.producer.profile(),
        crate::composer::PROVISIONAL_TEXT_PROFILE
    );
    let requested = crate::local_wave::raise_resolution_for_failed_checks(
        &mut v.f.store,
        &v.owner,
        &repo,
        &frozen.id,
        &source,
        &[crate::resolution::FailedCheck {
            check: format!("native-gate:direct-history:{}", "a".repeat(64)),
        }],
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let request = requested
        .resolution_request
        .expect("the current wave retains its exact failed-check request");
    let (result, token) = settle_test_synthesis(
        &mut v,
        &repo,
        &request,
        "direct-history-synthesis",
        "one.txt",
        "410a7265736f6c7665722061646a7573746d656e740a",
    );
    let expected_version = target::status(&v.f.store, v.owner.target())
        .unwrap()
        .version;
    let attached = crate::local_wave::attach_synthesized_attempt_result(
        &mut v.f.store,
        &v.owner,
        &repo,
        &frozen.id,
        expected_version,
        &request,
        &token,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(attached.phase, crate::local_wave::WavePhase::Verifying);
    assert_eq!(attached.resolution_request.as_ref(), Some(&request));
    assert_eq!(attached.resolution_result.as_ref(), Some(&result));
    let attached_candidate = attached
        .candidate_snapshot
        .as_ref()
        .expect("direct synthesized candidate is attached");
    let candidate_entries =
        crate::composer::snapshot_entries(&v.f.store, attached_candidate).unwrap();
    let (_, candidate_one) = candidate_entries
        .get(b"one.txt".as_slice())
        .expect("direct synthesized candidate retains A's file");
    let candidate_one_bytes = crate::composer::blob(&v.f.store, candidate_one).unwrap();
    assert_eq!(candidate_one_bytes.as_slice(), b"A\nresolver adjustment\n");
    let attached_revision = RecordId::parse(&attached.spec.revision).unwrap();
    v = retarget_verifier_subject(v, &attached_revision, attached_candidate.as_str());
    assert_eq!(
        v.execution.spec().candidate_source,
        attached_candidate.as_str(),
        "verification is pinned to the exact synthesized candidate"
    );
    let (synthesis, provenance) =
        crate::local_wave::read_synthesized_wave_result(&mut v.f.store, &repo, &attached)
            .unwrap()
            .expect("the attached direct result has exact settled provenance");
    assert_eq!(
        synthesis.status.state,
        crate::resolution::ResolutionAttemptState::Settled
    );
    assert_eq!(synthesis.status.request, request);
    assert_eq!(synthesis.status.result.as_ref(), Some(&result));
    assert_eq!(provenance.request_record_id, request.id.as_str());
    assert_eq!(provenance.request_object, request.object.hex());
    assert_eq!(provenance.group_record_id, result.id.as_str());
    assert_eq!(provenance.group_object, result.object.hex());
    (v, frozen.id)
}

fn seeded_synthesized_wave_fixture() -> (verifier::tests::Subject, RecordId) {
    let mut v = verifier::tests::Subject::acceptance_fixture();
    let repo = v.f._repo.path().to_path_buf();
    v.f.store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='idle',resume_phase=NULL WHERE target=?1",
            [v.owner.target()],
        )
        .unwrap();

    let base = v.f.a.base.commit.clone();
    git(&repo, &["checkout", "-q", "--detach", base.as_str()]);
    std::fs::write(repo.join("one.txt"), "B\n").unwrap();
    git(&repo, &["add", "one.txt"]);
    git(&repo, &["commit", "-qm", "conflicting B"]);
    let b_commit = CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let b = archive::retain_contribution(&mut v.f.store, &repo, &base, &b_commit).unwrap();
    let c = v.f.feature(&base);

    let policy_id = v.permit.policy().record().unwrap().id().to_string();
    let mut target_spec = v.f.spec();
    target_spec.acceptance_policy = policy_id;
    target_spec.contributions = vec![
        crate::local_target::RequestedContribution {
            id: "A".into(),
            lineage: v.f.a.lineage_record_id.to_string(),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        },
        crate::local_target::RequestedContribution {
            id: "B".into(),
            lineage: b.lineage_record_id.to_string(),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        },
        c,
    ];
    target_spec.selected = vec!["A".into(), "B".into(), "C".into()];
    let revision = target::propose_revision(&mut v.f.store, &v.owner, target_spec).unwrap();
    target::select_revision(&mut v.f.store, &v.owner, &revision.id).unwrap();
    let policy = v.permit.policy().clone();
    let producer = v.permit.producer().to_owned();
    let generation = v.permit.generation();
    v.permit = verifier::configure(
        &mut v.f.store,
        &v.owner,
        &producer,
        Some(generation),
        policy,
    )
    .unwrap();

    for (index, selected) in ["A", "B", "C"].iter().enumerate() {
        crate::local_wave::available(&mut v.f.store, &v.owner, selected, index as i64).unwrap();
    }
    let blocked = match crate::local_wave::tick(
        &mut v.f.store,
        &v.owner,
        &repo,
        crate::local_wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        3,
    )
    .unwrap()
    {
        crate::local_wave::Run::Blocked(wave) => wave,
        other => panic!("A/B must block the selected A/B/C wave: {other:?}"),
    };
    let request = blocked
        .resolution_request
        .clone()
        .expect("the blocked wave retains its exact resolution request");
    let (result, token) = settle_test_synthesis(
        &mut v,
        &repo,
        &request,
        "seeded-history-synthesis",
        "one.txt",
        "41420a",
    );
    let expected_version = target::status(&v.f.store, v.owner.target())
        .unwrap()
        .version;
    let recomposed = crate::local_wave::recompose_synthesized_conflict(
        &mut v.f.store,
        &v.owner,
        &repo,
        &blocked.id,
        expected_version,
        &result,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(recomposed.phase, crate::local_wave::WavePhase::Verifying);
    assert_eq!(recomposed.resolution_request.as_ref(), Some(&request));
    assert_eq!(recomposed.resolution_result.as_ref(), Some(&result));
    let settled =
        crate::resolution::attempt_status(&v.f.store, &token, "seeded-history-synthesis").unwrap();
    assert_eq!(
        settled.state,
        crate::resolution::ResolutionAttemptState::Settled
    );
    assert_eq!(settled.request, request);
    assert_eq!(settled.result.as_ref(), Some(&result));
    let (recomposed, composition_source) =
        crate::local_wave::load_composition_source(&mut v.f.store, &v.owner, &repo, &blocked.id)
            .unwrap();
    let composition_source = composition_source.expect("seeded hybrid source is retained");
    let final_candidate = composition_source
        .composition
        .outcome
        .candidate()
        .expect("the seeded source has a complete candidate");
    assert_eq!(
        final_candidate.mode,
        crate::composition::CompositionMode::Hybrid
    );
    assert_eq!(
        final_candidate.producer.profile(),
        crate::composer::PROVISIONAL_TEXT_PROFILE
    );
    let recipe_ref = composition_source
        .composition
        .recipe
        .as_ref()
        .expect("the seeded candidate is backed by a recipe");
    let recipe_record = crate::composer::read_recipe(&v.f.store, recipe_ref).unwrap();
    let recipe: serde_json::Value =
        serde_json::from_slice(&recipe_record.canonical_bytes()).unwrap();
    let seeded = &recipe["seeded_conflict"];
    assert_eq!(
        seeded["schema"],
        crate::composer::SEEDED_CONFLICT_RECIPE_SCHEMA_NAME
    );
    assert_eq!(seeded["trust"], "unverified");
    assert_eq!(seeded["selected"], serde_json::json!(["A", "B", "C"]));
    assert_eq!(seeded["suffix"], serde_json::json!(["C"]));
    assert_eq!(seeded["prefix_len"], 2);
    assert_eq!(
        seeded["resolution_request"]["record_id"],
        request.id.as_str()
    );
    assert_eq!(
        seeded["resolution_request"]["object_sha256"],
        request.object.hex()
    );
    assert_eq!(seeded["resolution_result"]["record_id"], result.id.as_str());
    assert_eq!(
        seeded["resolution_result"]["object_sha256"],
        result.object.hex()
    );
    let (synthesis, provenance) =
        crate::local_wave::read_synthesized_wave_result(&mut v.f.store, &repo, &recomposed)
            .unwrap()
            .expect("the seeded result has exact settled provenance");
    assert_eq!(
        synthesis.status.state,
        crate::resolution::ResolutionAttemptState::Settled
    );
    assert_eq!(synthesis.status.request, request);
    assert_eq!(synthesis.status.result.as_ref(), Some(&result));
    assert_eq!(provenance.request_record_id, request.id.as_str());
    assert_eq!(provenance.request_object, request.object.hex());
    assert_eq!(provenance.group_record_id, result.id.as_str());
    assert_eq!(provenance.group_object, result.object.hex());
    let candidate_source = recomposed
        .candidate_snapshot
        .as_ref()
        .expect("seeded candidate snapshot")
        .to_string();
    let candidate_revision = RecordId::parse(&recomposed.spec.revision).unwrap();
    v = retarget_verifier_subject(v, &candidate_revision, &candidate_source);
    (v, blocked.id)
}

fn retarget_verifier_subject(
    subject: verifier::tests::Subject,
    revision: &RecordId,
    candidate_source: &str,
) -> verifier::tests::Subject {
    let verifier::tests::Subject {
        mut f,
        owner,
        execution: prior_execution,
        artifact: prior_artifact,
        permit,
        inventory,
    } = subject;
    let mut execution_spec = prior_execution.spec().clone();
    execution_spec.candidate_source = candidate_source.into();
    let execution =
        crate::execution_snapshot::freeze(&mut f.store, &owner, revision, execution_spec).unwrap();

    let output = prior_artifact.output;
    let generation = prior_artifact.generation;
    let runtime = prior_artifact.runtime;
    let packaging = format!(
        "sha256:{}",
        ObjectDigest::of(&serde_json::to_vec(&execution.spec().build).unwrap()).hex()
    );
    let raw = serde_json::json!({
        "schema": crate::execution_supervisor::ARTIFACT_SCHEMA,
        "execution": execution.id().as_str(),
        "target_revision": revision.as_str(),
        "output": output.manifest.id().as_str(),
        "packaging": packaging,
        "runtime": runtime,
        "generation": generation,
    });
    let bytes = serde_json::to_vec(&raw).unwrap();
    let record = archive::put_object(&f.store, &bytes).unwrap();
    let logical = RecordId::parse(&format!(
        "sha256:{}",
        ObjectDigest::of(&[b"aethyme record v0\0".as_slice(), bytes.as_slice()].concat()).hex()
    ))
    .unwrap();
    let mut output_objects = output
        .files
        .values()
        .map(|file| archive::put_object(&f.store, file).unwrap())
        .collect::<Vec<_>>();
    output_objects.push(archive::put_object(&f.store, &output.manifest.manifest_bytes()).unwrap());
    output_objects.push(record);
    target::retain_artifacts(&mut f.store, &owner, revision, &[], &output_objects).unwrap();
    let artifact = crate::execution_supervisor::SealedArtifact {
        id: logical,
        record,
        output,
        execution: execution.id().clone(),
        generation,
        runtime,
        epoch: owner.authority_epoch().clone(),
    };
    verifier::tests::Subject {
        f,
        owner,
        execution,
        artifact,
        permit,
        inventory,
    }
}

fn complete_synthesized_wave(
    v: &mut verifier::tests::Subject,
    wave_id: &RecordId,
) -> (crate::local_controller::CompletionSpec, RecordId) {
    let wave = crate::local_wave::load(&v.f.store, wave_id).unwrap();
    let baseline =
        aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&wave.spec.baseline).unwrap();
    let base = archive::retained(&v.f.store, &baseline)
        .unwrap()
        .unwrap()
        .commit;
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        v.f._repo.path(),
        &[
            "remote",
            "add",
            "publication",
            remote.path().to_str().unwrap(),
        ],
    );
    git(
        v.f._repo.path(),
        &[
            "push",
            "-q",
            "publication",
            &format!("{}:refs/heads/accepted", base.as_str()),
        ],
    );
    let fixture_owner = v.f._host.path().canonicalize().unwrap();
    let host_cache = fixture_owner.join("broker-host-cache");
    let preparation_cache = fixture_owner.join("preparation-cache");
    let worktree_root = fixture_owner.join("worktrees");
    let host_operation_db = fixture_owner.join("host-operations.db");
    let collaboration_root =
        crate::collaboration_state::CollaborationRoot::under_host_state(&fixture_owner)
            .path()
            .to_path_buf();
    for path in [
        &host_cache,
        &preparation_cache,
        &worktree_root,
        &collaboration_root,
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    let mut broker = Broker::open_with_options(
        v.f._repo.path(),
        crate::BrokerOpenOptions::default()
            .with_host_state_dir(&fixture_owner)
            .with_host_cache_dir(&host_cache)
            .with_preparation_cache_root(&preparation_cache)
            .with_host_operation_database(host_operation_db.clone())
            .with_worktree_root(worktree_root.clone())
            .with_collaboration_state(collaboration_root.clone()),
    )
    .unwrap();
    let storage = broker.storage_context();
    assert_eq!(
        storage.host_state_dir.as_deref(),
        Some(fixture_owner.as_path())
    );
    assert_eq!(
        storage.host_cache_dir.as_deref(),
        Some(host_cache.as_path())
    );
    assert_eq!(
        storage.preparation_cache_root_override.as_deref(),
        Some(preparation_cache.as_path())
    );
    assert_eq!(
        storage.host_operation_db_path.as_deref(),
        Some(host_operation_db.as_path())
    );
    assert_eq!(
        storage.worktree_root_override.as_deref(),
        Some(worktree_root.as_path())
    );
    assert_eq!(
        storage.collaboration_state_dir.as_deref(),
        Some(collaboration_root.as_path())
    );
    let session = broker
        .adopt(
            v.f._repo.path(),
            Some("accepted synthesized history fixture"),
        )
        .unwrap();
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "publication",
        "refs/heads/accepted",
        30,
        "operator authorizes an exact disposable local Git acceptance test",
        true,
    )
    .unwrap();
    let intent = transport::prepare(
        &mut v.f.store,
        &v.owner,
        &carrier,
        wave_id,
        &"a".repeat(64),
        Some(&base),
    )
    .unwrap();
    let binding = appoint(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        &intent.id,
        ProviderRoute::GenericGit,
        "refs/heads/accepted",
        8,
    )
    .unwrap();
    transport::publish(&mut v.f.store, &v.owner, &carrier, &mut broker, &intent.id).unwrap();
    let transport_observation =
        transport::inspect(&mut v.f.store, &v.owner, &carrier, &mut broker, &intent.id).unwrap();
    assert_eq!(
        transport_observation.spec.observation,
        transport::Observation::Available
    );
    assert!(transport_observation.spec.content_checked);
    assert_eq!(
        transport_observation.spec.write_outcome.as_deref(),
        Some("succeeded")
    );
    let receipt = observe(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        binding.id(),
    )
    .unwrap();
    assert_eq!(receipt.spec.decision, ProviderDecision::Accepted);
    let evidence = v.evidence();
    let grant = qualify_receipt(
        &mut v.f.store,
        &v.owner,
        binding.id(),
        &v.execution,
        &v.artifact,
        &v.permit,
        &evidence,
        receipt,
    )
    .unwrap();
    let completion = crate::local_controller::complete(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        wave_id,
        &grant,
    )
    .unwrap();
    let raw_id: String =
        v.f.store
            .read_connection()
            .query_row(
                "SELECT id FROM local_controller_completions WHERE wave=?1",
                [wave_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
    let completion_id = RecordId::parse(&raw_id).unwrap();
    assert_eq!(
        crate::local_controller::load(&v.f.store, &completion_id).unwrap(),
        completion
    );
    assert_eq!(
        crate::local_wave::load(&v.f.store, wave_id).unwrap().phase,
        crate::local_wave::WavePhase::Accepted
    );
    (completion, completion_id)
}

fn assert_completion_admits_next_wave(
    v: &mut verifier::tests::Subject,
    completion: &crate::local_controller::CompletionSpec,
    completion_id: &RecordId,
) {
    // Preserve the underlying historical-provenance failure in this fixture.
    // Admission intentionally collapses corrupted historical authority to
    // `WaveError::Stale`; these test-only calls identify which retained proof
    // rejects the accepted completion before admission makes that collapse.
    let historical = HistoricalCompletionIdentity {
        completion: completion_id.to_string(),
        target: completion.target.clone(),
        issuer: v.owner.issuer().to_owned(),
        revision: completion.revision.clone(),
        epoch: completion.epoch.clone(),
        wave: completion.wave.clone(),
        source: completion.source.clone(),
        candidate_commit: completion.candidate_commit.clone(),
        candidate_tree: completion.candidate_tree.clone(),
        qualification: completion.qualification.clone(),
        receipt: completion.acceptance_receipt.clone(),
        accepted_commit: completion.accepted_commit.clone(),
        accepted_tree: completion.accepted_tree.clone(),
    };
    crate::local_acceptance::verify_historical_completion(&v.f.store, &historical).unwrap_or_else(
        |error| panic!("historical completion verifier rejected fixture: {error:?}"),
    );
    let source_wave_id = RecordId::parse(&completion.wave).unwrap();
    let source_wave = crate::local_wave::load(&v.f.store, &source_wave_id).unwrap();
    crate::local_wave::prior_completion::verify_historical_source_for_test(
        &v.f.store,
        completion_id,
        completion,
        &source_wave.spec,
    )
    .unwrap_or_else(|error| panic!("historical source verifier rejected fixture: {error:?}"));

    let accepted = CommitOid::parse(&completion.accepted_commit).unwrap();
    git(
        v.f._repo.path(),
        &[
            "switch",
            "-qc",
            "next-after-synthesized-acceptance",
            accepted.as_str(),
        ],
    );
    std::fs::write(v.f._repo.path().join("next.txt"), "next contribution\n").unwrap();
    git(v.f._repo.path(), &["add", "next.txt"]);
    git(
        v.f._repo.path(),
        &["commit", "-qm", "contribution after synthesized acceptance"],
    );
    let next_commit = CommitOid::parse(&git(v.f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    let retained =
        archive::retain_contribution(&mut v.f.store, v.f._repo.path(), &accepted, &next_commit)
            .unwrap();

    let old_revision = RecordId::parse(&completion.revision).unwrap();
    let mut next_spec = target::load_revision(&v.f.store, &old_revision)
        .unwrap()
        .spec;
    next_spec.baseline = completion.source.clone();
    next_spec.contributions = vec![crate::local_target::RequestedContribution {
        id: "next".into(),
        lineage: retained.lineage_record_id.to_string(),
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    }];
    next_spec.selected = vec!["next".into()];
    let state = target::status(&v.f.store, v.owner.target()).unwrap();
    assert_eq!(state.phase, target::Phase::PublicationConfirmed);
    target::transition(&mut v.f.store, &v.owner, state.version, target::Phase::Idle).unwrap();
    let next_revision = target::propose_revision(&mut v.f.store, &v.owner, next_spec).unwrap();
    target::select_revision(&mut v.f.store, &v.owner, &next_revision.id).unwrap();
    target::release_revision(&mut v.f.store, &v.owner, &old_revision).unwrap();

    crate::local_wave::available(&mut v.f.store, &v.owner, "next", 1).unwrap();
    let next_wave = match crate::local_wave::admit(
        &mut v.f.store,
        &v.owner,
        crate::local_wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        2,
    )
    .unwrap()
    {
        crate::local_wave::Admission::Frozen(wave) => wave,
        other => panic!("accepted synthesized source did not admit the next wave: {other:?}"),
    };
    assert_eq!(
        next_wave.spec.prior_completion.as_deref(),
        Some(completion_id.as_str())
    );
    assert_eq!(next_wave.spec.baseline, completion.source);
    let composed =
        crate::local_wave::compose(&mut v.f.store, &v.owner, v.f._repo.path(), &next_wave.id)
            .unwrap();
    assert_eq!(composed.phase, crate::local_wave::WavePhase::Verifying);
    let (_, source) = crate::local_wave::load_composition_source(
        &mut v.f.store,
        &v.owner,
        v.f._repo.path(),
        &next_wave.id,
    )
    .unwrap();
    assert_eq!(
        source
            .expect("the next wave records its exact accepted baseline source")
            .request
            .baseline
            .as_str(),
        completion.accepted_commit
    );
}

#[test]
fn accepted_direct_synthesis_is_reused_by_the_next_wave() {
    let (mut v, wave_id) = direct_synthesized_wave_fixture();
    let (completion, completion_id) = complete_synthesized_wave(&mut v, &wave_id);
    assert_completion_admits_next_wave(&mut v, &completion, &completion_id);
}

#[test]
fn accepted_seeded_synthesis_is_reused_by_the_next_wave() {
    let (mut v, wave_id) = seeded_synthesized_wave_fixture();
    let (completion, completion_id) = complete_synthesized_wave(&mut v, &wave_id);
    assert_completion_admits_next_wave(&mut v, &completion, &completion_id);
}

#[test]
fn private_current_qualification_requires_verifier_and_refuses_revocation() {
    let (mut v, _remote, mut broker, carrier, _wave, intent, binding, receipt, evidence) =
        accepted_wave_fixture();
    let grant = qualify_receipt(
        &mut v.f.store,
        &v.owner,
        binding.id(),
        &v.execution,
        &v.artifact,
        &v.permit,
        &evidence,
        receipt.clone(),
    )
    .unwrap();
    assert_eq!(grant.receipt(), &receipt);
    assert_eq!(grant.expires_ms(), evidence.expires_ms());

    let qualification_body: Vec<u8> =
        v.f.store
            .read_connection()
            .query_row(
                "SELECT body FROM local_acceptance_qualifications WHERE id=?1",
                [grant.id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
    let qualification: serde_json::Value = serde_json::from_slice(&qualification_body).unwrap();
    let expected_execution_object = v.execution.reference().object.hex();
    let expected_inventory_object = inventory_object_for_execution(&v.f.store, &v.execution)
        .unwrap()
        .hex();
    let expected_artifact_object = v.artifact.record_object().hex();
    assert_eq!(
        qualification["spec"]["execution_object_sha256"].as_str(),
        Some(expected_execution_object.as_str())
    );
    assert_eq!(
        qualification["spec"]["inventory_object_sha256"].as_str(),
        Some(expected_inventory_object.as_str())
    );
    assert_eq!(
        qualification["spec"]["artifact_object_sha256"].as_str(),
        Some(expected_artifact_object.as_str())
    );

    let current = current_guard(&mut v.f.store, &v.owner, &mut broker, &carrier, &grant)
        .unwrap_or_else(|error| {
            panic!("freshly qualified accepted ref should be current: {error:?}")
        });
    current.check_clock().unwrap();
    drop(current);
    let newest_receipt = last_receipt(&v.f.store, binding.id()).unwrap().unwrap();
    assert_ne!(newest_receipt.id, receipt.id);
    let wrong = v.f.b.result.commit.as_str();
    git(
        v.f._repo.path(),
        &[
            "push",
            "-q",
            "--force",
            "publication",
            &format!("{wrong}:refs/heads/accepted"),
        ],
    );
    match current_guard(&mut v.f.store, &v.owner, &mut broker, &carrier, &grant) {
        Err(AcceptanceError::NotAccepted) => {}
        Err(error) => panic!("revoked accepted ref returned the wrong refusal: {error:?}"),
        Ok(_) => panic!("revoked accepted ref unexpectedly remained current"),
    }
    drop(grant);
    // Restore the exact accepted provider representation and obtain its fresh
    // receipt, so stale source history cannot mask the producer revocation gate.
    git(
        v.f._repo.path(),
        &[
            "push",
            "-q",
            "--force",
            "publication",
            &format!("{}:refs/heads/accepted", intent.spec.commit),
        ],
    );
    let receipt = observe(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        binding.id(),
    )
    .unwrap();
    assert_eq!(receipt.spec.decision, ProviderDecision::Accepted);
    verifier::revoke(
        &mut v.f.store,
        &v.owner,
        v.permit.producer(),
        v.permit.generation(),
    )
    .unwrap();
    assert!(matches!(
        qualify_receipt(
            &mut v.f.store,
            &v.owner,
            binding.id(),
            &v.execution,
            &v.artifact,
            &v.permit,
            &evidence,
            receipt
        ),
        Err(AcceptanceError::Verification(
            verifier::VerificationError::Producer
        ))
    ));
}

fn age_archive_objects(store: &CollaborationStore) {
    for prefix in std::fs::read_dir(archive::objects_dir(store)).unwrap() {
        for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(object.unwrap().path())
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
                )
                .unwrap();
        }
    }
}

#[test]
fn historical_completion_roots_survive_revision_release_and_gc() {
    let (mut v, _remote, mut broker, carrier, wave_id, _intent, binding, receipt, evidence) =
        accepted_wave_fixture();
    let grant = qualify_receipt(
        &mut v.f.store,
        &v.owner,
        binding.id(),
        &v.execution,
        &v.artifact,
        &v.permit,
        &evidence,
        receipt.clone(),
    )
    .unwrap();
    let completion = crate::local_controller::complete(
        &mut v.f.store,
        &v.owner,
        &mut broker,
        &carrier,
        &wave_id,
        &grant,
    )
    .unwrap();
    let completion_raw: String =
        v.f.store
            .read_connection()
            .query_row(
                "SELECT id FROM local_controller_completions WHERE wave=?1",
                [wave_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
    let completion_id = RecordId::parse(&completion_raw).unwrap();
    let historical = HistoricalCompletionIdentity {
        completion: completion_id.to_string(),
        target: completion.target.clone(),
        issuer: v.owner.issuer().into(),
        revision: completion.revision.clone(),
        epoch: completion.epoch.clone(),
        wave: completion.wave.clone(),
        source: completion.source.clone(),
        candidate_commit: completion.candidate_commit.clone(),
        candidate_tree: completion.candidate_tree.clone(),
        qualification: completion.qualification.clone(),
        receipt: completion.acceptance_receipt.clone(),
        accepted_commit: completion.accepted_commit.clone(),
        accepted_tree: completion.accepted_tree.clone(),
    };
    assert_eq!(historical.receipt, receipt.id.as_str());
    assert_ne!(
        last_receipt(&v.f.store, binding.id()).unwrap().unwrap().id,
        receipt.id
    );
    let (generation_column, evidence_body): (i64, Vec<u8>) = v
        .f
        .store
        .read_connection()
        .query_row(
            "SELECT generation,body FROM local_verification_evidence WHERE id=(SELECT evidence FROM local_acceptance_qualifications WHERE id=?1)",
            [&historical.qualification],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let evidence_json: serde_json::Value = serde_json::from_slice(&evidence_body).unwrap();
    let body_generation = evidence_json["observation"]["generation"]
        .as_str()
        .expect("the evidence producer stores generation as a decimal string");
    assert_eq!(
        body_generation.parse::<i64>().unwrap(),
        generation_column,
        "the serialized producer generation agrees with the integer journal column"
    );
    verify_historical_completion(&v.f.store, &historical).unwrap();

    let mut wrong_completion = historical.clone();
    wrong_completion.completion = format!("sha256:{}", "f".repeat(64));
    assert!(verify_historical_completion(&v.f.store, &wrong_completion).is_err());
    let mut wrong_epoch = historical.clone();
    wrong_epoch.epoch = format!("sha256:{}", "e".repeat(64));
    assert!(verify_historical_completion(&v.f.store, &wrong_epoch).is_err());
    let mut wrong_receipt = historical.clone();
    wrong_receipt.receipt = format!("sha256:{}", "d".repeat(64));
    assert!(verify_historical_completion(&v.f.store, &wrong_receipt).is_err());
    let mut wrong_qualification = historical.clone();
    wrong_qualification.qualification = format!("sha256:{}", "c".repeat(64));
    assert!(verify_historical_completion(&v.f.store, &wrong_qualification).is_err());

    let receipt_body: Vec<u8> =
        v.f.store
            .read_connection()
            .query_row(
                "SELECT body FROM local_acceptance_receipts WHERE id=?1",
                [&historical.receipt],
                |row| row.get(0),
            )
            .unwrap();
    v.f.store
        .connection()
        .execute(
            "UPDATE local_acceptance_receipts SET body=?1 WHERE id=?2",
            rusqlite::params![b"{}".as_slice(), &historical.receipt],
        )
        .unwrap();
    assert!(verify_historical_completion(&v.f.store, &historical).is_err());
    v.f.store
        .connection()
        .execute(
            "UPDATE local_acceptance_receipts SET body=?1 WHERE id=?2",
            rusqlite::params![receipt_body, &historical.receipt],
        )
        .unwrap();
    let qualification_body: Vec<u8> =
        v.f.store
            .read_connection()
            .query_row(
                "SELECT body FROM local_acceptance_qualifications WHERE id=?1",
                [&historical.qualification],
                |row| row.get(0),
            )
            .unwrap();
    v.f.store
        .connection()
        .execute(
            "UPDATE local_acceptance_qualifications SET body=?1 WHERE id=?2",
            rusqlite::params![b"{}".as_slice(), &historical.qualification],
        )
        .unwrap();
    assert!(verify_historical_completion(&v.f.store, &historical).is_err());
    v.f.store
        .connection()
        .execute(
            "UPDATE local_acceptance_qualifications SET body=?1 WHERE id=?2",
            rusqlite::params![qualification_body, &historical.qualification],
        )
        .unwrap();

    // Rebase a fresh contribution on the exact accepted source so the next
    // immutable revision can legitimately use that source as its baseline.
    let accepted_base =
        crate::collaboration_archive::CommitOid::parse(&completion.accepted_commit).unwrap();
    git(
        v.f._repo.path(),
        &["switch", "-qc", "post-completion", accepted_base.as_str()],
    );
    std::fs::write(v.f._repo.path().join("post-completion.txt"), "continued\n").unwrap();
    git(v.f._repo.path(), &["add", "post-completion.txt"]);
    git(
        v.f._repo.path(),
        &["commit", "-qm", "post completion contribution"],
    );
    let child = crate::collaboration_archive::CommitOid::parse(&git(
        v.f._repo.path(),
        &["rev-parse", "HEAD"],
    ))
    .unwrap();
    let retained =
        archive::retain_contribution(&mut v.f.store, v.f._repo.path(), &accepted_base, &child)
            .unwrap();
    let old_revision = RecordId::parse(&completion.revision).unwrap();
    let mut next_spec = target::load_revision(&v.f.store, &old_revision)
        .unwrap()
        .spec;
    next_spec.baseline = completion.source.clone();
    next_spec.contributions = vec![target::RequestedContribution {
        id: "next".into(),
        lineage: retained.lineage_record_id.to_string(),
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    }];
    next_spec.selected = vec!["next".into()];
    let next_revision = target::propose_revision(&mut v.f.store, &v.owner, next_spec).unwrap();
    v.f.store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='idle',resume_phase=NULL WHERE target=?1",
            [v.owner.target()],
        )
        .unwrap();
    target::select_revision(&mut v.f.store, &v.owner, &next_revision.id).unwrap();
    target::release_revision(&mut v.f.store, &v.owner, &old_revision).unwrap();

    let orphan = archive::put_object(&v.f.store, b"historical completion GC sentinel").unwrap();
    age_archive_objects(&v.f.store);
    let options = crate::collaboration_gc::GcOptions::default();
    let plan = crate::collaboration_gc::plan(&mut v.f.store, &options).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let report = crate::collaboration_gc::apply(&mut v.f.store, &plan.digest, &options).unwrap();
    assert!(report.reclaimed_bytes > 0);
    assert!(archive::read_object(&v.f.store, &orphan).is_err());
    verify_historical_completion(&v.f.store, &historical).unwrap();

    drop(grant);
    let prior_epoch = v.owner.authority_epoch().clone();
    let target_name = historical.target.clone();
    let issuer = historical.issuer.clone();
    let frozen =
        crate::local_authority::freeze(&mut v.f.store, &prior_epoch, "historical proof fixture")
            .unwrap();
    let _resumed = crate::local_authority::resume(&mut v.f.store, &frozen.epoch).unwrap();
    drop(v.owner);
    let new_owner = target::acquire_controller(&mut v.f.store, &target_name, &issuer).unwrap();
    assert_ne!(new_owner.authority_epoch(), &prior_epoch);
    verify_historical_completion(&v.f.store, &historical).unwrap();

    // The normal admission path selects the historical completion after the
    // owner epoch changes, then composition binds the actual accepted commit
    // separately from its retained source snapshot.
    crate::local_wave::available(&mut v.f.store, &new_owner, "next", 0).unwrap();
    let policy = crate::local_wave::WavePolicy {
        batch_width: 1,
        ..Default::default()
    };
    let crate::local_wave::Admission::Frozen(admitted) =
        crate::local_wave::admit(&mut v.f.store, &new_owner, policy, 1_000).unwrap()
    else {
        panic!("historical baseline was not admitted after owner reacquisition")
    };
    assert_eq!(
        admitted.spec.prior_completion.as_deref(),
        Some(historical.completion.as_str())
    );
    assert_eq!(admitted.spec.baseline, historical.source);
    let composed =
        crate::local_wave::compose(&mut v.f.store, &new_owner, v.f._repo.path(), &admitted.id)
            .unwrap();
    assert_eq!(composed.phase, crate::local_wave::WavePhase::Verifying);
    let (composed, source) = crate::local_wave::load_composition_source(
        &mut v.f.store,
        &new_owner,
        v.f._repo.path(),
        &admitted.id,
    )
    .unwrap();
    let source = source.expect("the exact composition source remains attached");
    assert_eq!(source.request.baseline.as_str(), historical.accepted_commit);
    assert_ne!(source.request.baseline.as_str(), historical.source);
    assert_eq!(composed.spec.baseline, historical.source);
}

#[test]
fn aggregate_changes_requested_blocks_and_comments_do_not_erase_approval() {
    let policy = crate::ReviewPolicy {
        enabled: true,
        ..Default::default()
    };
    let approved = serde_json::json!({"id":1,"user":{"id":10,"login":"reviewer"},"state":"APPROVED","commit_id":oid()});
    let requested = serde_json::json!({"id":2,"user":{"id":11,"login":"other"},"state":"CHANGES_REQUESTED","commit_id":oid()});
    assert!(
        !review_pass(
            &policy,
            &oid(),
            "author",
            Some("CHANGES_REQUESTED"),
            &[approved.clone(), requested],
            &[]
        )
        .unwrap()
    );
    assert!(
        !review_pass(
            &policy,
            &oid(),
            "author",
            None,
            std::slice::from_ref(&approved),
            &[]
        )
        .unwrap()
    );
    let commented = serde_json::json!({"id":3,"user":{"id":10,"login":"reviewer"},"state":"COMMENTED","commit_id":oid()});
    assert!(
        review_pass(
            &policy,
            &oid(),
            "author",
            Some("APPROVED"),
            &[approved, commented],
            &[]
        )
        .unwrap()
    );
}

#[test]
fn retained_policy_survives_unreferenced_git_commit_removal() {
    let mut s = Subject::new();
    s.publish();
    let oid = s.binding.spec.policy_commit.clone();
    let path =
        s.f._repo
            .path()
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
    assert!(path.is_file());
    std::fs::remove_file(path).unwrap();
    assert_eq!(s.observe().spec.decision, ProviderDecision::Accepted);
    let object = ObjectDigest::of(b"");
    let rooted:bool=s.f.store.read_connection().query_row("SELECT EXISTS(SELECT 1 FROM local_target_roots WHERE revision=?1 AND kind='object' AND reference=?2)",params![s.intent.spec.revision,object.hex()],|r|r.get(0)).unwrap();
    assert!(rooted);
}

#[test]
fn generic_git_refuses_existing_review_and_publication_policy() {
    let mut s = Subject::new();
    for policy in [
        "[review]\nenabled=true\n",
        "[publication]\nmode='review_gated'\n",
    ] {
        std::fs::create_dir_all(s.f._repo.path().join(".aethyme")).unwrap();
        std::fs::write(s.f._repo.path().join(".aethyme/config.toml"), policy).unwrap();
        git(s.f._repo.path(), &["add", ".aethyme/config.toml"]);
        git(
            s.f._repo.path(),
            &["commit", "-qm", "existing operator policy"],
        );
        let commit = git(s.f._repo.path(), &["rev-parse", "HEAD"]);
        git(
            s.f._repo.path(),
            &[
                "push",
                "-q",
                "publication",
                &format!("{commit}:refs/heads/accepted"),
            ],
        );
        assert!(
            matches!(appoint(&mut s.f.store,&s.owner,&mut s.broker,&s.carrier,&s.intent.id,ProviderRoute::GenericGit,"refs/heads/accepted",8),Err(AcceptanceError::Invalid(message)) if message.contains("mandatory review"))
        );
    }
}

#[test]
fn independent_receipt_vector_binds_source_policy_provider_and_representation() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("receipt-vector.json")).unwrap();
    let bytes = vector["canonical"].as_str().unwrap().as_bytes();
    let record = Record::decode(bytes, &[&RECEIPT]).unwrap();
    assert_eq!(record.canonical_bytes(), bytes);
    assert_eq!(record.id().as_str(), vector["id"].as_str().unwrap());
    let raw: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let spec: ReceiptSpec = serde_json::from_value(raw["spec"].clone()).unwrap();
    assert_eq!(canonical(&RECEIPT, &spec).unwrap().canonical_bytes(), bytes);
    for key in [
        "source",
        "config_digest",
        "candidate_commit",
        "candidate_tree",
        "accepted_commit",
        "base_ref",
        "peer",
        "binding",
    ] {
        let mut changed = raw.clone();
        changed["spec"][key] = "different".into();
        assert_ne!(
            Record::decode(&serde_json::to_vec(&changed).unwrap(), &[&RECEIPT])
                .unwrap()
                .id(),
            record.id()
        );
    }
}
