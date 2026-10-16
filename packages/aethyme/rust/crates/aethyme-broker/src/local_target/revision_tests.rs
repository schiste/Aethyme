use super::*;
use rusqlite::params;

#[test]
fn released_d10_history_does_not_require_reclaimed_archive_objects() {
    let mut fixture = tests::Fixture::new();
    let owner = acquire_controller(&mut fixture.store, "history-target", "issuer").unwrap();
    let spec = fixture.spec();
    let proposed = propose_revision(&mut fixture.store, &owner, spec).unwrap();

    // This test isolates the released-history loader contract. It installs a
    // canonical D10-shaped binding whose referenced objects are intentionally
    // absent; a released record may render stored dispositions but cannot
    // re-enter the active target API.
    let (target, body): (String, Vec<u8>) = fixture
        .store
        .read_connection()
        .query_row(
            "SELECT target, body FROM local_target_revisions WHERE revision = ?1",
            [proposed.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let descriptor_object = "f".repeat(64);
    let revision_object = "e".repeat(64);
    body["revision_bindings"] = serde_json::to_value(D10RevisionBindings {
        profile: D10_TARGET_PROFILE.into(),
        project_id: "unavailable-project-binding".into(),
        descriptor_record_id: format!("sha256:{}", "d".repeat(64)),
        descriptor_object_sha256: descriptor_object.clone(),
        revisions: vec![D10RevisionPin {
            contribution: "A".into(),
            logical_contribution_id: "unavailable-logical-revision".into(),
            revision_record_id: format!("sha256:{}", "c".repeat(64)),
            revision_object_sha256: revision_object.clone(),
        }],
    })
    .unwrap();
    let record = Record::decode(
        &serde_json::to_vec(&body).unwrap(),
        &[&TARGET_REVISION_SCHEMA],
    )
    .unwrap();
    let history_id = record.id();
    let history_body = record.canonical_bytes();
    let stored_dispositions: Vec<(String, String, Option<String>)> = fixture
        .store
        .read_connection()
        .prepare(
            "SELECT contribution, status, detail FROM local_target_dispositions WHERE revision = ?1 ORDER BY contribution",
        )
        .unwrap()
        .query_map([proposed.id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let tx = fixture
        .store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute(
        "DELETE FROM local_target_dispositions WHERE revision = ?1",
        [proposed.id.as_str()],
    )
    .unwrap();
    tx.execute(
        "DELETE FROM local_target_roots WHERE revision = ?1",
        [proposed.id.as_str()],
    )
    .unwrap();
    tx.execute(
        "DELETE FROM local_target_revisions WHERE revision = ?1",
        [proposed.id.as_str()],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO local_target_revisions (revision, target, body, released) VALUES (?1, ?2, ?3, 1)",
        params![history_id.as_str(), target, history_body],
    )
    .unwrap();
    for (contribution, status, detail) in stored_dispositions {
        tx.execute(
            "INSERT INTO local_target_dispositions (revision, contribution, status, detail) VALUES (?1, ?2, ?3, ?4)",
            params![history_id.as_str(), contribution, status, detail],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    assert!(matches!(
        load_revision(&fixture.store, &history_id),
        Err(TargetError::Released)
    ));
    assert!(
        !archive::has_object(
            &fixture.store,
            &archive::parse_digest(&descriptor_object).unwrap()
        )
        .unwrap()
    );
    assert!(
        !archive::has_object(
            &fixture.store,
            &archive::parse_digest(&revision_object).unwrap()
        )
        .unwrap()
    );
    let dispositions = dispositions(&fixture.store, &history_id).unwrap();
    assert_eq!(dispositions.len(), 2);
    assert!(matches!(
        select_revision(&mut fixture.store, &owner, &history_id),
        Err(TargetError::Released)
    ));

    let response = crate::collaboration_sensitive::response_use(&fixture.store).unwrap();
    let history =
        load_revision_history_under_response(&fixture.store, &history_id, &response).unwrap();
    assert_eq!(history.id, history_id);
    assert_eq!(history.target, target);
}
