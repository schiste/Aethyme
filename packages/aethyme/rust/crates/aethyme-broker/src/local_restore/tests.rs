use super::*;

use crate::collaboration_state::{CollaborationRoot, ProjectKey};

fn selected_record(
    store: &CollaborationStore,
    id: &RecordId,
    table: &str,
    key: &str,
    digest_column: &str,
) -> RecordRef {
    let digest: String = store
        .read_connection()
        .query_row(
            &format!("SELECT {digest_column} FROM {table} WHERE {key}=?1"),
            [id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    RecordRef {
        id: id.clone(),
        object: manager::digest(&digest).unwrap(),
    }
}
struct Fixture {
    f: crate::local_target::tests::Fixture,
    owner: ControllerOwner,
    scope: Scope,
    selection: Selection,
    wire: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = crate::local_target::tests::Fixture::new();
        let owner =
            target::acquire_controller(&mut f.store, "restore", "appointed-exporter").unwrap();
        let capture = capture::capture(
            &mut f.store,
            &capture::CaptureRequest {
                operation_id: capture::OperationId::parse("restore-capture").unwrap(),
                repository: f._repo.path().into(),
                base: f.a.base.commit.clone(),
                result: f.a.result.commit.clone(),
                policy: capture::CapturePolicy::Required,
                retention: capture::RetentionBoundary::UntilReleased,
            },
        )
        .unwrap();
        let brief = brief::Brief::from_decision_file(
            br#"{"intent":"Preserve the independently edited source."}"#,
        )
        .unwrap();
        let brief_id = context::attach_brief(&mut f.store, &capture.contribution, &brief).unwrap();
        let query = context::ContextQuery {
            scope: vec![b"one.txt".to_vec()],
            source: Some(capture.result.clone()),
            analysis: vec![],
            budget: Default::default(),
        };
        let context = context::retrieve(&mut f.store, &query, &context::LocalProject).unwrap();
        assert_eq!(context.contributions.len(), 1);
        let context_object = archive::put_object(&f.store, &context.record).unwrap();
        let mut records = vec![
            selected_record(
                &f.store,
                &capture.contribution,
                "retained_contributions",
                "lineage_record_id",
                "record_sha256",
            ),
            selected_record(
                &f.store,
                &capture.receipt_record_id,
                "capture_receipts",
                "receipt_record_id",
                "receipt_sha256",
            ),
            selected_record(
                &f.store,
                &brief_id,
                "contribution_briefs",
                "brief_record_id",
                "brief_sha256",
            ),
            RecordRef {
                id: context.id,
                object: context_object,
            },
        ];
        for retained in [&f.a.base, &f.a.result] {
            records.push(selected_record(
                &f.store,
                &retained.record_id,
                "retained_snapshots",
                "record_id",
                "record_sha256",
            ));
        }
        let selection = Selection {
            sources: vec![capture.base, capture.result],
            records,
        };
        let scope = Scope {
            origin: f.store.project().as_str().into(),
            visibility: "local_project".into(),
            audience: "named-receiver".into(),
        };
        let wire = export(&f.store, &owner, &scope, &selection).unwrap();
        Self {
            f,
            owner,
            scope,
            selection,
            wire,
        }
    }
    fn verified(&self) -> VerifiedBundle {
        verify(
            &self.wire,
            &ReceiverPolicy {
                bundle: ObjectDigest::of(&self.wire),
                scope: self.scope.clone(),
            },
        )
        .unwrap()
    }
}
fn receiver(host: &std::path::Path) -> (CollaborationStore, ControllerOwner) {
    let mut store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("separate-receiver").unwrap(),
        &[],
    )
    .unwrap();
    let owner = target::acquire_controller(&mut store, "restore", "appointed-receiver").unwrap();
    (store, owner)
}
fn wire(header: &Header, objects: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let header = canonical(header).unwrap();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(header.len() as u64).to_be_bytes());
    out.extend_from_slice(&header);
    for bytes in objects.values() {
        out.extend_from_slice(bytes);
    }
    out
}
fn verify_changed(
    header: &Header,
    objects: &BTreeMap<String, Vec<u8>>,
    scope: &Scope,
) -> Result<VerifiedBundle, RestoreError> {
    let bytes = wire(header, objects);
    verify(
        &bytes,
        &ReceiverPolicy {
            bundle: ObjectDigest::of(&bytes),
            scope: scope.clone(),
        },
    )
}
fn count(store: &CollaborationStore, table: &str) -> i64 {
    store
        .read_connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn complete_source_and_context_round_trip_is_historical_only() {
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    assert_eq!(imported.scope, f.scope);
    assert_eq!(imported.sources.len(), 2);
    assert_eq!(imported.records.len(), f.selection.records.len());
    assert_eq!(imported.limitations, bundle.limitations());
    for table in [
        "retained_snapshots",
        "retained_contributions",
        "capture_operations",
        "capture_receipts",
        "contribution_briefs",
        "context_cache",
        "context_indexed",
        "local_verification_evidence",
        "local_acceptance_receipts",
        "local_agent_sessions",
        "local_generations",
    ] {
        assert_eq!(count(&store, table), 0, "history cannot populate {table}");
    }
    let again = import(&mut store, &owner, &binding, &bundle).unwrap();
    assert_eq!(again.id, imported.id);
    assert_eq!(count(&store, "local_imports"), 1);
    assert_eq!(show(&store, &imported.id).unwrap().bundle, imported.bundle);
    for record in &imported.records {
        assert_eq!(
            read_bounded(&store, &record.object).unwrap(),
            read_bounded(&f.f.store, &record.object).unwrap()
        );
    }
}
#[test]
fn independent_bundle_vector_uses_the_closed_canonical_wire() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("bundle-vector.json")).unwrap();
    let bytes: Vec<u8> = vector["wire_hex"]
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect();
    assert_eq!(
        ObjectDigest::of(&bytes).hex(),
        vector["digest"].as_str().unwrap()
    );
    let scope: Scope = serde_json::from_value(vector["scope"].clone()).unwrap();
    let bundle = verify(
        &bytes,
        &ReceiverPolicy {
            bundle: ObjectDigest::of(&bytes),
            scope,
        },
    )
    .unwrap();
    assert_eq!(wire(&bundle.header, &bundle.objects), bytes);
    assert_eq!(bundle.header.sources[0], vector["source"].as_str().unwrap());
}
#[test]
fn receiver_digest_origin_visibility_and_audience_are_independently_pinned() {
    let f = Fixture::new();
    let mut policy = ReceiverPolicy {
        bundle: ObjectDigest::of(&f.wire),
        scope: f.scope.clone(),
    };
    policy.bundle = ObjectDigest::of(b"different");
    assert!(matches!(
        verify(&f.wire, &policy),
        Err(RestoreError::Binding)
    ));
    policy.bundle = ObjectDigest::of(&f.wire);
    for field in ["origin", "visibility", "audience"] {
        let mut scope = serde_json::to_value(&f.scope).unwrap();
        scope[field] = "other".into();
        policy.scope = serde_json::from_value(scope).unwrap();
        assert!(matches!(
            verify(&f.wire, &policy),
            Err(RestoreError::Binding)
        ));
    }
}
#[test]
fn changed_objects_missing_payloads_trailing_bytes_and_duplicates_refuse() {
    let f = Fixture::new();
    let bundle = f.verified();
    let mut changed = f.wire.clone();
    *changed.last_mut().unwrap() ^= 1;
    assert!(
        verify(
            &changed,
            &ReceiverPolicy {
                bundle: ObjectDigest::of(&changed),
                scope: f.scope.clone()
            }
        )
        .is_err()
    );
    changed = f.wire[..f.wire.len() - 1].to_vec();
    assert!(
        verify(
            &changed,
            &ReceiverPolicy {
                bundle: ObjectDigest::of(&changed),
                scope: f.scope.clone()
            }
        )
        .is_err()
    );
    changed = f.wire.clone();
    changed.push(0);
    assert!(
        verify(
            &changed,
            &ReceiverPolicy {
                bundle: ObjectDigest::of(&changed),
                scope: f.scope.clone()
            }
        )
        .is_err()
    );
    let mut header = bundle.header.clone();
    header.sources.push(header.sources[0].clone());
    assert!(verify_changed(&header, &bundle.objects, &f.scope).is_err());
    let mut header = bundle.header.clone();
    header.objects.push(header.objects[0].clone());
    assert!(verify_changed(&header, &bundle.objects, &f.scope).is_err());
    let mut header = bundle.header.clone();
    header.records.push(header.records[0].clone());
    assert!(verify_changed(&header, &bundle.objects, &f.scope).is_err());
}
#[test]
fn source_and_record_reference_graph_must_be_complete() {
    let f = Fixture::new();
    let bundle = f.verified();
    for schema in [
        archive::CONTRIBUTION_LINEAGE_SCHEMA_NAME,
        capture::CAPTURE_RECEIPT_SCHEMA_NAME,
        brief::BRIEF_SCHEMA_NAME,
    ] {
        let mut header = bundle.header.clone();
        header.records.retain(|r| r.schema != schema);
        assert!(
            verify_changed(&header, &bundle.objects, &f.scope).is_err(),
            "missing referenced {schema}"
        );
    }
    let mut header = bundle.header.clone();
    header.sources.remove(0);
    assert!(verify_changed(&header, &bundle.objects, &f.scope).is_err());
    let mut objects = bundle.objects.clone();
    let mut header = bundle.header.clone();
    let bytes = b"unselected host credentials".to_vec();
    let digest = ObjectDigest::of(&bytes).hex();
    objects.insert(digest.clone(), bytes.clone());
    header.objects.push(ObjectRef {
        digest,
        bytes: bytes.len() as u64,
    });
    header.objects.sort_by(|a, b| a.digest.cmp(&b.digest));
    assert!(matches!(
        verify_changed(&header, &objects, &f.scope),
        Err(RestoreError::Invalid(_))
    ));
}
#[test]
fn unsupported_native_records_never_become_a_restore_policy() {
    let f = Fixture::new();
    let bundle = f.verified();
    let mut header = bundle.header.clone();
    header.records[0].schema = "aethyme.acceptance/provisional-v0".into();
    assert!(matches!(
        verify_changed(&header, &bundle.objects, &f.scope),
        Err(RestoreError::Unsupported(_))
    ));
    let record = Record::decode(
        br#"{"schema":"aethyme.generation-intent/provisional-v0","spec":{"command":"do not run"}}"#,
        &[&aethyme_contracts::experimental_v0::RecordSchema {
            name: "aethyme.generation-intent/provisional-v0",
            fields: &[aethyme_contracts::experimental_v0::FieldSpec {
                name: "spec",
                required: true,
                kind: aethyme_contracts::experimental_v0::FieldKind::Opaque,
                capability: None,
            }],
            capabilities: &[],
        }],
    )
    .unwrap();
    let object = archive::put_object(&f.f.store, &record.canonical_bytes()).unwrap();
    let mut selection = f.selection.clone();
    selection.records.push(RecordRef {
        id: record.id(),
        object,
    });
    assert!(matches!(
        export(&f.f.store, &f.owner, &f.scope, &selection),
        Err(RestoreError::Unsupported(_))
    ));
}
#[test]
fn advertised_object_and_selection_budgets_are_enforced_before_copy() {
    let f = Fixture::new();
    let bundle = f.verified();
    let mut header = bundle.header.clone();
    header.objects[0].bytes = MAX_OBJECT as u64 + 1;
    assert!(verify_changed(&header, &bundle.objects, &f.scope).is_err());
    let mut selection = f.selection.clone();
    selection.sources = vec![selection.sources[0].clone(); MAX_SOURCES + 1];
    assert!(export(&f.f.store, &f.owner, &f.scope, &selection).is_err());
    let mut huge = f.wire.clone();
    huge.resize(MAX_WIRE + 1, 0);
    assert!(
        verify(
            &huge,
            &ReceiverPolicy {
                bundle: ObjectDigest::of(&huge),
                scope: f.scope.clone()
            }
        )
        .is_err()
    );
}
#[test]
fn stale_receiving_binding_and_revoked_exporter_cannot_copy_bytes() {
    let mut f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let other = tempfile::tempdir().unwrap();
    let (mut other_store, other_owner) = receiver(other.path());
    assert!(
        matches!(
            import(&mut other_store, &other_owner, &binding, &bundle),
            Err(RestoreError::Binding)
        ),
        "same project/epoch differs through fresh epoch, not host labels"
    );
    // Revoke before invocation; neither historical binding nor imported digest revives it.
    crate::local_authority::freeze(&mut store, owner.authority_epoch(), "test restore fencing")
        .unwrap();
    assert!(import(&mut store, &owner, &binding, &bundle).is_err());
    assert_eq!(count(&store, "local_imports"), 0);
    crate::local_authority::freeze(
        &mut f.f.store,
        f.owner.authority_epoch(),
        "test export fencing",
    )
    .unwrap();
    assert!(export(&f.f.store, &f.owner, &f.scope, &f.selection).is_err());
}
#[test]
fn interruption_after_cas_publication_keeps_no_partially_bound_import() {
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let result = manager::import_with(&mut store, &owner, &binding, &bundle, &mut || {
        Err(RestoreError::Unknown)
    });
    assert!(matches!(result, Err(RestoreError::Unknown)));
    assert_eq!(count(&store, "local_imports"), 0);
    assert_eq!(count(&store, "local_imported_records"), 0);
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    assert_eq!(count(&store, "local_imports"), 1);
    assert_eq!(show(&store, &imported.id).unwrap().bundle, bundle.digest);
}
#[test]
fn durable_historical_object_pins_survive_real_gc() {
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    let orphan = archive::put_object(&store, b"unreferenced restore bytes").unwrap();
    for prefix in std::fs::read_dir(archive::objects_dir(&store)).unwrap() {
        for file in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(file.unwrap().path())
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                .unwrap();
        }
    }
    let options = gc::GcOptions::default();
    let plan = gc::plan(&mut store, &options).unwrap();
    assert!(!plan.reclaimable.is_empty());
    let report = gc::apply(&mut store, &plan.digest, &options).unwrap();
    assert!(!report.reclaimed.is_empty());
    assert!(read_bounded(&store, &orphan).is_err());
    assert_eq!(show(&store, &imported.id).unwrap().bundle, bundle.digest);
}
#[test]
fn corrupted_historical_header_or_receipt_never_becomes_complete_source() {
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    let source = imported.sources[0].clone();
    let object = archive::object_path(&store, &ObjectDigest::of_snapshot(&source));
    let original = std::fs::read(&object).unwrap();
    std::fs::write(&object, b"tampered manifest").unwrap();
    assert!(show(&store, &imported.id).is_err());
    assert!(rehearse(&mut store, &owner, &imported.id, &source, "bad").is_err());
    std::fs::write(object, original).unwrap();
    store
        .connection()
        .execute(
            "UPDATE local_imports SET body=x'7b7d' WHERE id=?1",
            [imported.id.as_str()],
        )
        .unwrap();
    assert!(show(&store, &imported.id).is_err());
}
#[test]
fn prepared_rehearsal_is_exact_exclusive_and_rechecked() {
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    let source = f.f.a.result.snapshot_id.clone();
    let root = rehearse(&mut store, &owner, &imported.id, &source, "rehearsal").unwrap();
    assert_eq!(std::fs::read(root.join("one.txt")).unwrap(), b"A\n");
    assert_eq!(
        rehearse(&mut store, &owner, &imported.id, &source, "rehearsal").unwrap(),
        root
    );
    std::fs::write(root.join("one.txt"), "late change").unwrap();
    assert!(rehearse(&mut store, &owner, &imported.id, &source, "rehearsal").is_err());
}
#[test]
fn interrupted_rehearsal_is_unknown_and_does_not_replay() {
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    let source = f.f.a.result.snapshot_id.clone();
    assert!(matches!(
        manager::rehearse_with(
            &mut store,
            &owner,
            &imported.id,
            &source,
            "crash",
            &mut |_| Err(RestoreError::Unknown)
        ),
        Err(RestoreError::Unknown)
    ));
    assert!(matches!(
        rehearse(&mut store, &owner, &imported.id, &source, "crash"),
        Err(RestoreError::Unknown)
    ));
    assert!(rehearse(&mut store, &owner, &imported.id, &source, "fresh-operation").is_ok());
}
#[cfg(unix)]
#[test]
fn swapped_rehearsal_ancestor_refuses_without_a_prepared_seal() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let bundle = f.verified();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    let source = f.f.a.result.snapshot_id.clone();
    let result = manager::rehearse_with(
        &mut store,
        &owner,
        &imported.id,
        &source,
        "swap",
        &mut |root| {
            let parent = root.parent().unwrap();
            let moved = parent.with_extension("preserved");
            std::fs::rename(parent, &moved)?;
            symlink(&moved, parent)?;
            Ok(())
        },
    );
    assert!(result.is_err());
    let prepared: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_import_rehearsals WHERE state='prepared'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prepared, 0);
}
#[test]
fn independent_source_build_survives_contributor_storage_destruction() {
    let mut f = Fixture::new();
    std::fs::write(
        f.f._repo.path().join("main.rs"),
        "fn main() { println!(\"restored Playground\"); }\n",
    )
    .unwrap();
    for args in [
        vec!["add", "main.rs"],
        vec!["commit", "-qm", "ordinary Playground Rust source"],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(f.f._repo.path())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(f.f._repo.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit =
        archive::CommitOid::parse(std::str::from_utf8(&commit.stdout).unwrap().trim()).unwrap();
    let retained = archive::retain_snapshot(&mut f.f.store, f.f._repo.path(), &commit).unwrap();
    let bytes = export(
        &f.f.store,
        &f.owner,
        &f.scope,
        &Selection {
            sources: vec![retained.snapshot_id.clone()],
            records: vec![],
        },
    )
    .unwrap();
    let bundle = verify(
        &bytes,
        &ReceiverPolicy {
            bundle: ObjectDigest::of(&bytes),
            scope: f.scope.clone(),
        },
    )
    .unwrap();
    let host = tempfile::tempdir().unwrap();
    let (mut store, owner) = receiver(host.path());
    let binding = bind_receiver(&store, &owner, &bundle).unwrap();
    let imported = import(&mut store, &owner, &binding, &bundle).unwrap();
    std::fs::remove_dir_all(f.f._repo.path()).unwrap();
    std::fs::remove_dir_all(f.f._host.path()).unwrap();
    let root = rehearse(
        &mut store,
        &owner,
        &imported.id,
        &retained.snapshot_id,
        "trusted-build",
    )
    .unwrap();
    let output = host.path().join("independent-build");
    let build = std::process::Command::new("rustc")
        .arg(root.join("main.rs"))
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let result = std::process::Command::new(output).output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"restored Playground\n");
    assert_eq!(show(&store, &imported.id).unwrap().bundle, bundle.digest);
}
