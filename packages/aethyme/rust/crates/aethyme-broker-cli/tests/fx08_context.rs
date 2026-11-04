//! Explicit matched Playground brief export; ordinary CI validates the freeze.
use aethyme_broker::collaboration_archive::pin_commit;
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary, capture,
};
use aethyme_broker::collaboration_context::{
    Budget, ContextQuery, LocalProject, attach_brief, retrieve,
};
use aethyme_broker::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, forbidden_roots,
};
use aethyme_contracts::experimental_v0::brief::Brief;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

const TASKS: &[(&str, &str)] = &[
    ("declared", "01-optional"),
    ("declared", "02-pagination"),
    ("declared", "03-protocol"),
    ("declared", "04-completion"),
    ("sealed", "05-identifiers"),
    ("sealed", "06-lookup"),
];
const ARMS: &[&str] = &["correct", "absent", "incomplete", "misleading"];
fn fixtures() -> PathBuf {
    aethyme_testkit::rust_workspace_root().join("crates/aethyme-testkit/fixtures/fx08")
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn commit(repo: &Path, bytes: &[u8]) -> String {
    std::fs::write(repo.join("source.rs"), bytes).unwrap();
    git(repo, &["add", "source.rs"]);
    git(repo, &["commit", "-qm", "synthetic source"]);
    git(repo, &["rev-parse", "HEAD"])
}
#[test]
fn every_arm_fits_the_existing_brief_profile_and_has_a_real_prior_change() {
    for (group, task) in TASKS {
        let fixture = fixtures().join(group).join(task);
        assert_ne!(
            std::fs::read(fixture.join("private/prior-base.rs")).unwrap(),
            std::fs::read(fixture.join("public/source.rs")).unwrap()
        );
        let arms: Value =
            serde_json::from_slice(&std::fs::read(fixture.join("brief-arms.json")).unwrap())
                .unwrap();
        assert!(arms["absent"].is_null());
        let mut counts = Vec::new();
        for arm in ["correct", "incomplete", "misleading"] {
            let brief =
                Brief::from_decision_file(&serde_json::to_vec(&arms[arm]).unwrap()).unwrap();
            assert!(brief.token_count() <= 150);
            counts.push(brief.token_count());
        }
        assert!(
            *counts.iter().max().unwrap() <= *counts.iter().min().unwrap() * 2,
            "unmatched brief length: {task}: {counts:?}"
        );
    }
}
#[test]
#[ignore = "explicit actual Local capture and L3 retrieval for a frozen synthetic FX08 campaign"]
fn export_actual_l3_contexts_for_matched_trials() {
    let evidence = PathBuf::from(
        std::env::var_os("AETHYME_FX08_CONTEXT_EVIDENCE").expect("fresh evidence root"),
    );
    std::fs::create_dir(&evidence).expect("never overwrite earlier qualification");
    let mut rows = Vec::new();
    for (group, task) in TASKS {
        let fixture = fixtures().join(group).join(task);
        let arms: Value =
            serde_json::from_slice(&std::fs::read(fixture.join("brief-arms.json")).unwrap())
                .unwrap();
        for arm in ARMS {
            let root = evidence.join(format!("{task}-{arm}"));
            std::fs::create_dir(&root).unwrap();
            let repo = root.join("Playground");
            std::fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(&repo, &["config", "user.name", "Playground"]);
            git(
                &repo,
                &["config", "user.email", "playground@example.invalid"],
            );
            let base = commit(
                &repo,
                &std::fs::read(fixture.join("private/prior-base.rs")).unwrap(),
            );
            let result = commit(
                &repo,
                &std::fs::read(fixture.join("public/source.rs")).unwrap(),
            );
            let mut store = CollaborationStore::open(
                &CollaborationRoot::under_host_state(&root.join("state")),
                &ProjectKey::parse("fx08-matched").unwrap(),
                &forbidden_roots(&repo),
            )
            .unwrap();
            let outcome = capture(
                &mut store,
                &CaptureRequest {
                    operation_id: OperationId::mint().unwrap(),
                    repository: repo.clone(),
                    base: pin_commit(&repo, &base).unwrap(),
                    result: pin_commit(&repo, &result).unwrap(),
                    policy: CapturePolicy::Required,
                    retention: RetentionBoundary::UntilReleased,
                },
            )
            .unwrap();
            let CaptureOutcome::Acknowledged(receipt) = outcome else {
                panic!("actual prior source must be retained");
            };
            let brief_id = if arms[arm].is_null() {
                None
            } else {
                let brief =
                    Brief::from_decision_file(&serde_json::to_vec(&arms[arm]).unwrap()).unwrap();
                Some(attach_brief(&mut store, &receipt.contribution, &brief).unwrap())
            };
            let context = retrieve(
                &mut store,
                &ContextQuery {
                    scope: vec![b"source.rs".to_vec()],
                    source: Some(receipt.result.clone()),
                    analysis: Vec::new(),
                    budget: Budget {
                        max_items: 1,
                        max_brief_tokens: 150,
                        ..Budget::default()
                    },
                },
                &LocalProject,
            )
            .unwrap();
            assert_eq!(context.contributions, vec![receipt.contribution.as_str()]);
            let decoded: Value = serde_json::from_slice(&context.record).unwrap();
            assert_eq!(decoded["authority"], "advisory_context");
            if brief_id.is_some() {
                let expected =
                    Brief::from_decision_file(&serde_json::to_vec(&arms[arm]).unwrap()).unwrap();
                let (bytes, id) = expected.to_record();
                let actual = &decoded["items"][0]["brief"];
                assert_eq!(brief_id.as_ref(), Some(&id));
                assert_eq!(actual["record"], id.as_str());
                assert_eq!(actual["tokens"], expected.token_count());
                assert_eq!(
                    actual["content"],
                    serde_json::from_slice::<Value>(&bytes).unwrap()
                );
                assert_eq!(actual["role"], "untrusted_data");
                assert!(decoded["items"][0]["brief_omitted"].is_null());
            } else {
                assert!(decoded["items"][0]["brief"].is_null());
                assert!(decoded["items"][0]["brief_omitted"].is_null());
            }
            let public = root.join("public");
            std::fs::create_dir(&public).unwrap();
            for name in ["source.rs", "task.md"] {
                std::fs::copy(fixture.join("public").join(name), public.join(name)).unwrap();
            }
            std::fs::write(public.join("context.json"), &context.record).unwrap();
            rows.push(json!({"task":task,"group":group,"arm":arm,"public":public,
                "contribution":receipt.contribution.as_str(),"source":receipt.result.as_str(),
                "brief":brief_id.map(|id|id.as_str().to_string()),"context_record":context.id.as_str(),
                "context_sha256":sha(&context.record),"cache_key":context.cache_key,"coverage":decoded["coverage"],
                "freshness":decoded["freshness"],"delivered":true,"agent_access":"not_yet_run"}));
            std::fs::write(
                evidence.join("contexts.json"),
                serde_json::to_vec_pretty(&rows).unwrap(),
            )
            .unwrap();
        }
    }
    assert_eq!(rows.len(), 24);
}
