//! Public selection of exact immutable contribution revisions.
use super::*;
use crate::collaboration_archive::ObjectDigest;
use crate::collaboration_context::{RevisionPin, retrieve_revisions};
use aethyme_contracts::experimental_v0::ContributionId;

const MAX_PINS: usize = 256;
const PIN_FORMAT: &str = "CONTRIBUTION_ID@REVISION_RECORD_ID@sha256:HEX";

pub(super) enum BriefSelector {
    Capture(RecordId),
    Operation(OperationId),
}

pub(super) fn brief_selector<'a>(rest: &[&'a str]) -> Result<(BriefSelector, &'a str), Failure> {
    no_extra(rest, &["--contribution", "--operation"], 1)?;
    for flag in ["--contribution", "--operation"] {
        if rest.iter().filter(|word| **word == flag).count() > 1 {
            return Err(Failure::usage(format!("duplicate {flag}")));
        }
    }
    let contribution = flag_value(rest, "--contribution")?;
    let operation = flag_value(rest, "--operation")?;
    let selector = match (contribution, operation) {
        (Some(id), None) => BriefSelector::Capture(RecordId::parse(id).map_err(|error| {
            Failure::refused(
                "invalid_record_id",
                error.to_string(),
                "pass the capture receipt's contribution ID",
            )
        })?),
        (None, Some(id)) => BriefSelector::Operation(OperationId::parse(id).map_err(|error| {
            Failure::refused(
                "invalid_operation_id",
                error.to_string(),
                "pass the exact capture operation ID",
            )
        })?),
        _ => {
            return Err(Failure::usage(
                "select exactly one of --contribution <record-id> or --operation <id>",
            ));
        }
    };
    let mut index = 0;
    while index < rest.len() {
        if matches!(rest[index], "--contribution" | "--operation") {
            index += 2;
        } else {
            return Ok((selector, rest[index]));
        }
    }
    Err(Failure::usage("the decision file is required"))
}

pub(super) fn pins(rest: &[&str]) -> Result<Vec<RevisionPin>, Failure> {
    let mut pins = Vec::new();
    for (index, word) in rest.iter().enumerate() {
        if *word != "--revision-pin" {
            continue;
        }
        if pins.len() == MAX_PINS {
            return Err(Failure::usage(format!(
                "at most {MAX_PINS} --revision-pin values are allowed"
            )));
        }
        let value = rest
            .get(index + 1)
            .ok_or_else(|| Failure::usage(format!("--revision-pin needs {PIN_FORMAT}")))?;
        pins.push(parse_pin(value)?);
    }
    Ok(pins)
}

fn parse_pin(value: &str) -> Result<RevisionPin, Failure> {
    let invalid = || {
        Failure::refused(
            "invalid_revision_pin",
            "invalid exact revision pin",
            format!("pass {PIN_FORMAT} with canonical IDs and lowercase hex"),
        )
    };
    let mut parts = value.split('@');
    let contribution = parts.next().ok_or_else(invalid)?;
    let revision = parts.next().ok_or_else(invalid)?;
    let digest = parts
        .next()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or_else(invalid)?;
    if parts.next().is_some()
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid());
    }
    let contribution_id = ContributionId::parse(contribution).map_err(|_| invalid())?;
    let revision_record_id = RecordId::parse(revision).map_err(|_| invalid())?;
    let bytes: [u8; 32] = data_encoding::HEXLOWER
        .decode(digest.as_bytes())
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())?;
    Ok(RevisionPin {
        contribution_id,
        revision_record_id,
        object_digest: ObjectDigest::from_bytes(bytes),
    })
}

pub(super) fn retrieve(
    store: &CollaborationStore,
    query: &ContextQuery,
    pins: &[RevisionPin],
) -> Result<Output, Failure> {
    let answer = retrieve_revisions(store, query, pins, &LocalProject)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let record: Value = serde_json::from_slice(&answer.record)
        .map_err(|error| Failure::domain("state", format!("context record: {error}")))?;
    let value = json!({
        "schema": CONTEXT_SCHEMA,
        "served": "fresh",
        "stored": false,
        "cache_key": answer.cache_key,
        "context_id": answer.id.as_str(),
        "absence_is_evidence": false,
        "context": record,
    });
    Ok((value, context_text(&record, "fresh", false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> String {
        format!(
            "contrib:{}@sha256:{}@sha256:{}",
            "a".repeat(26),
            "1".repeat(64),
            "2".repeat(64)
        )
    }

    #[test]
    fn identity_integrity_and_io_errors_remain_failures_at_the_cli_boundary() {
        use crate::collaboration_identity_store::IdentityStoreError;
        let corrupt = IdentityStoreError::Corrupt("invalid immutable record".into());
        assert_eq!(
            Failure::domain(corrupt.code(), corrupt.to_string()).exit,
            exit_status::FAILED
        );
        let sqlite = IdentityStoreError::Sqlite(rusqlite::Error::InvalidQuery);
        let context = crate::collaboration_context::ContextError::ImmutableRevision(sqlite);
        assert_eq!(
            Failure::domain(context.code(), context.to_string()).exit,
            exit_status::FAILED
        );
        let unknown = IdentityStoreError::UnknownCaptureOperation("absent-operation".into());
        assert_eq!(
            Failure::domain(unknown.code(), unknown.to_string()).exit,
            exit_status::REFUSED
        );
    }

    #[test]
    fn brief_selector_requires_one_exact_selector_and_one_file() {
        let operation = ["--operation", "specific-op", "decision.json"];
        let (selector, file) = brief_selector(&operation).unwrap();
        assert!(
            matches!(selector, BriefSelector::Operation(ref id) if id.as_str() == "specific-op")
        );
        assert_eq!(file, "decision.json");
        let record = format!("sha256:{}", "1".repeat(64));
        assert!(matches!(
            brief_selector(&["decision.json", "--contribution", &record])
                .unwrap()
                .0,
            BriefSelector::Capture(_)
        ));
        for invalid in [
            vec!["decision.json"],
            vec!["--operation", "specific-op"],
            vec!["--operation", "specific-op", "decision.json", "extra.json"],
            vec![
                "--operation",
                "specific-op",
                "--contribution",
                &record,
                "decision.json",
            ],
            vec![
                "--operation",
                "specific-op",
                "--operation",
                "other-op",
                "decision.json",
            ],
        ] {
            assert!(brief_selector(&invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn operation_brief_handler_selects_one_of_two_identical_capture_lineages() {
        use crate::collaboration_cli::project_test_support::BoundProjectFixture;
        let mut fixture = BoundProjectFixture::new().unwrap();
        let base = fixture.bootstrap_commit.clone();
        let result = fixture
            .commit_file("a.txt", b"identical capture bytes\n")
            .unwrap();
        let first = fixture
            .capture_operation("brief-first", &base, &result)
            .unwrap();
        let second = fixture
            .capture_operation("brief-second", &base, &result)
            .unwrap();
        let original = Brief {
            intent: "First immutable rationale".into(),
            ..Brief::default()
        };
        let revised = Brief {
            intent: "Second operation rationale".into(),
            ..Brief::default()
        };
        let first_selector = BriefSelector::Operation(first.operation_id.clone());
        let (first_result, _) =
            attach_selected_brief(&mut fixture.store, first_selector, &original).unwrap();
        let second_selector = BriefSelector::Operation(second.operation_id.clone());
        let (second_result, _) =
            attach_selected_brief(&mut fixture.store, second_selector, &revised).unwrap();
        assert_eq!(first_result["contribution"], second_result["contribution"]);
        assert_ne!(
            first_result["contribution_revision"],
            second_result["contribution_revision"]
        );
        assert_eq!(first_result["operation"], first.operation_id.as_str());
        assert_eq!(second_result["operation"], second.operation_id.as_str());
        let capture = RecordId::parse(first_result["contribution"].as_str().unwrap()).unwrap();
        let ambiguous = attach_selected_brief(
            &mut fixture.store,
            BriefSelector::Capture(capture),
            &revised,
        )
        .unwrap_err();
        assert!(ambiguous.message.contains("--operation"));
        let (retried, _) = attach_selected_brief(
            &mut fixture.store,
            BriefSelector::Operation(first.operation_id),
            &original,
        )
        .unwrap();
        assert_eq!(retried, first_result);
    }

    #[test]
    fn exact_pin_parser_requires_all_three_canonical_identities() {
        let parsed = parse_pin(&valid()).unwrap();
        assert_eq!(
            parsed.contribution_id.as_str(),
            format!("contrib:{}", "a".repeat(26))
        );
        assert_eq!(parsed.object_digest.hex(), "2".repeat(64));
        for malformed in [
            valid().replace("contrib:", ""),
            valid().replace(&"a".repeat(26), &"a".repeat(25)),
            valid().replace(&"2".repeat(64), &"A".repeat(64)),
            format!("{}@extra", valid()),
            "contrib:a@sha256:1@sha256:2".into(),
        ] {
            assert_eq!(
                parse_pin(&malformed).unwrap_err().code,
                "invalid_revision_pin"
            );
        }
    }

    #[test]
    fn exact_pin_parser_bounds_repeated_values_and_requires_an_argument() {
        assert!(pins(&["--revision-pin"]).is_err());
        let value = valid();
        let accepted: Vec<_> = (0..MAX_PINS)
            .flat_map(|_| ["--revision-pin", value.as_str()])
            .collect();
        assert_eq!(pins(&accepted).unwrap().len(), MAX_PINS);
        let mut rejected = accepted;
        rejected.extend(["--revision-pin", value.as_str()]);
        assert!(pins(&rejected).is_err());
    }
}
