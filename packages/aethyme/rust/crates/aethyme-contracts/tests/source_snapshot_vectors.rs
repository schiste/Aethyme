//! SourceSnapshotId v0 against golden vectors produced by an independent
//! reference implementation (`fixtures/experimental-v0/source_snapshot.json`).
//! A non-Rust implementation must pass the same file.

use aethyme_contracts::experimental_v0::source_snapshot::PathRejection;
use aethyme_contracts::experimental_v0::{
    EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotError, SourceSnapshotId,
};
use data_encoding::HEXLOWER;
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    valid: Vec<ValidCase>,
    invalid: Vec<InvalidCase>,
    unsupported_modes: Vec<String>,
    malformed_ids: Vec<String>,
}

#[derive(Deserialize)]
struct ValidCase {
    name: String,
    entries: Vec<EntryCase>,
    manifest_hex: String,
    id: String,
}

#[derive(Deserialize)]
struct InvalidCase {
    name: String,
    entries: Vec<EntryCase>,
    error: String,
}

#[derive(Deserialize, Clone)]
struct EntryCase {
    path_hex: String,
    mode: String,
    content_hex: String,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!(
        "../fixtures/experimental-v0/source_snapshot.json"
    ))
    .expect("golden vectors parse")
}

fn hex(text: &str) -> Vec<u8> {
    HEXLOWER.decode(text.as_bytes()).expect("vector hex")
}

fn entries(cases: &[EntryCase]) -> Vec<SourceEntry> {
    cases
        .iter()
        .map(|e| {
            let kind = EntryKind::from_git_mode(&e.mode).expect("vector mode");
            SourceEntry::from_content(hex(&e.path_hex), kind, &hex(&e.content_hex))
        })
        .collect()
}

/// A stable code for each refusal, matching the vector file's `error` field.
fn error_code(error: &SourceSnapshotError) -> String {
    match error {
        SourceSnapshotError::InvalidPath { reason, .. } => format!(
            "invalid_path:{}",
            match reason {
                PathRejection::Empty => "empty",
                PathRejection::TooLong => "too_long",
                PathRejection::ContainsNul => "contains_nul",
                PathRejection::Absolute => "absolute",
                PathRejection::TrailingSlash => "trailing_slash",
                PathRejection::EmptyComponent => "empty_component",
                PathRejection::RelativeComponent => "relative_component",
                PathRejection::GitComponent => "git_component",
            }
        ),
        SourceSnapshotError::DuplicatePath { .. } => "duplicate_path".into(),
        SourceSnapshotError::FileDirectoryConflict { .. } => "file_directory_conflict".into(),
        SourceSnapshotError::CaseFoldCollision { .. } => "case_fold_collision".into(),
        SourceSnapshotError::UnsupportedMode { .. } => "unsupported_mode".into(),
        SourceSnapshotError::MalformedId { .. } => "malformed_id".into(),
    }
}

#[test]
fn valid_vectors_match_manifest_and_id_byte_for_byte() {
    for case in vectors().valid {
        let snapshot = SourceSnapshot::new(entries(&case.entries))
            .unwrap_or_else(|e| panic!("{}: unexpected refusal: {e}", case.name));
        assert_eq!(
            HEXLOWER.encode(&snapshot.manifest_bytes()),
            case.manifest_hex,
            "{}: manifest",
            case.name
        );
        assert_eq!(snapshot.id().as_str(), case.id, "{}: id", case.name);
    }
}

#[test]
fn input_order_never_changes_the_id() {
    for case in vectors().valid {
        let forward = entries(&case.entries);
        let mut reversed = forward.clone();
        reversed.reverse();
        let mut rotated = forward.clone();
        rotated.rotate_left(forward.len().min(1));
        let id = SourceSnapshot::new(forward).unwrap().id();
        assert_eq!(
            id,
            SourceSnapshot::new(reversed).unwrap().id(),
            "{}",
            case.name
        );
        assert_eq!(
            id,
            SourceSnapshot::new(rotated).unwrap().id(),
            "{}",
            case.name
        );
    }
}

/// Line endings, mode, Unicode composition and raw path bytes are all part of
/// identity: no two distinct vectors may collide.
#[test]
fn distinct_snapshots_never_share_an_id() {
    let ids: Vec<String> = vectors().valid.into_iter().map(|c| c.id).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn invalid_vectors_are_refused_with_the_expected_reason() {
    for case in vectors().invalid {
        let error = SourceSnapshot::new(entries(&case.entries))
            .err()
            .unwrap_or_else(|| panic!("{}: accepted an invalid snapshot", case.name));
        assert_eq!(error_code(&error), case.error, "{}", case.name);
    }
}

#[test]
fn unsupported_modes_are_refused_not_skipped() {
    for mode in vectors().unsupported_modes {
        let error = EntryKind::from_git_mode(&mode).expect_err(&mode);
        assert_eq!(error_code(&error), "unsupported_mode", "{mode:?}");
    }
}

#[test]
fn ids_round_trip_and_only_the_canonical_form_parses() {
    for case in vectors().valid {
        assert_eq!(SourceSnapshotId::parse(&case.id).unwrap().as_str(), case.id);
    }
    for encoded in vectors().malformed_ids {
        let error = SourceSnapshotId::parse(&encoded).expect_err(&encoded);
        assert_eq!(error_code(&error), "malformed_id", "{encoded:?}");
    }
}

#[test]
fn a_precomputed_content_digest_gives_the_same_entry() {
    let content = b"streamed content\n";
    let digest: [u8; 32] = {
        use sha2::Digest;
        sha2::Sha256::digest(content).into()
    };
    assert_eq!(
        SourceEntry::from_content("a.txt", EntryKind::Regular, content),
        SourceEntry::from_content_digest("a.txt", EntryKind::Regular, digest)
    );
}
