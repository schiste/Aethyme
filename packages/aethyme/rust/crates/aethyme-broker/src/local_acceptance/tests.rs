use super::*;
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

#[test]
fn private_current_qualification_requires_verifier_and_refuses_revocation() {
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
    let mut broker = Broker::open(v.f._repo.path())
        .unwrap()
        .with_host_operation_database(v.f._host.path().join("host-operations.db"))
        .with_worktree_root(v.f._host.path().join("worktrees"));
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
    let evidence = v.evidence();
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
    let current = current_guard(&mut v.f.store, &v.owner, &mut broker, &carrier, &grant).unwrap();
    current.check_clock().unwrap();
    drop(current);
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
    assert!(matches!(
        current_guard(&mut v.f.store, &v.owner, &mut broker, &carrier, &grant),
        Err(AcceptanceError::NotAccepted)
    ));
    drop(grant);
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
