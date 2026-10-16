//! Public selection of exact immutable contribution revisions.
use super::*;
use crate::collaboration_archive::ObjectDigest;
use crate::collaboration_context::{RevisionPin, retrieve_revisions};
use aethyme_contracts::experimental_v0::ContributionId;

const MAX_PINS: usize = 256;
const PIN_FORMAT: &str = "CONTRIBUTION_ID@REVISION_RECORD_ID@sha256:HEX";

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
