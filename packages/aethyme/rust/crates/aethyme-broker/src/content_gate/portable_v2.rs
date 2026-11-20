//! Independent framing of Aexeo's draft candidate-binding profile.
//!
//! This computes a digest only. It does not parse or validate a verdict,
//! reconstruct a candidate, authenticate artifacts, or authorize publication.
//! Snapshot digests must be independently reconstructed by the host; values
//! copied from a previous verdict cannot establish currentness.
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"content-gate:decision-inputs:v2\0";
const REVISION_PROFILE: &[u8] = b"content-gate.candidate-revision/v1";
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Exact artifacts independently acquired from the current evaluation context.
/// `None` is distinct from present empty bytes for optional artifacts.
pub struct CandidateArtifacts<'a> {
    pub change: &'a [u8],
    pub resolved_policy: &'a [u8],
    pub host_options: &'a [u8],
    pub evaluator: &'a [u8],
    pub collection: Option<&'a [u8]>,
    pub external_evidence: Option<&'a [u8]>,
    pub base_revision_sha256: [u8; 32],
    pub candidate_revision_sha256: [u8; 32],
}

/// Frame the fixed, lexically ordered artifact set specified by the draft.
/// Equality of this digest is not evidence of a valid verdict or safe publish.
pub fn candidate_binding(a: &CandidateArtifacts<'_>) -> Result<[u8; 32], &'static str> {
    let artifacts: [(&str, Option<&[u8]>); 9] = [
        (
            "candidate_base_revision_sha256",
            Some(&a.base_revision_sha256),
        ),
        ("candidate_revision_profile", Some(REVISION_PROFILE)),
        (
            "candidate_revision_sha256",
            Some(&a.candidate_revision_sha256),
        ),
        ("change", Some(a.change)),
        ("collection", a.collection),
        ("evaluator", Some(a.evaluator)),
        ("external_evidence", a.external_evidence),
        ("host_options", Some(a.host_options)),
        ("resolved_policy", Some(a.resolved_policy)),
    ];
    let total = artifacts
        .iter()
        .try_fold(0usize, |n, (_, value)| {
            n.checked_add(value.map_or(0, |bytes| bytes.len()))
        })
        .ok_or("decision artifacts exceed size limit")?;
    if total > MAX_BYTES {
        return Err("decision artifacts exceed size limit");
    }
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    hash.update((artifacts.len() as u32).to_be_bytes());
    for (name, bytes) in artifacts {
        hash.update((name.len() as u32).to_be_bytes());
        hash.update(name.as_bytes());
        match bytes {
            None => hash.update([0]),
            Some(bytes) => {
                hash.update([1]);
                hash.update((bytes.len() as u64).to_be_bytes());
                hash.update(bytes);
            }
        }
    }
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(hex: &str) -> [u8; 32] {
        std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
    }
    #[test]
    fn matches_aexeo_cross_language_vector_and_binds_current_inputs() {
        // Frozen Aexeo vector plus independently computed snapshot digests from
        // its Python candidate overlay implementation.
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("portable-verdict-vector.json")).unwrap();
        let a = &vector["artifacts"];
        let mut input = CandidateArtifacts {
            change: a["change"].as_str().unwrap().as_bytes(),
            resolved_policy: a["resolved_policy"].as_str().unwrap().as_bytes(),
            host_options: a["host_options"].as_str().unwrap().as_bytes(),
            evaluator: a["evaluator"].as_str().unwrap().as_bytes(),
            collection: None,
            external_evidence: None,
            base_revision_sha256: decode(
                "69a706fb08665399c92496d0c4d7321e5d9857d469fc1ef0d8ee844526dd4ea7",
            ),
            candidate_revision_sha256: decode(
                "5b3bbd7e90693296330978fdd708f309cc3c52bac6a8f17f450567d472b1c8a2",
            ),
        };
        let expected = decode(vector["portable"]["binding_sha256"].as_str().unwrap());
        assert_eq!(candidate_binding(&input).unwrap(), expected);
        input.collection = Some(b"");
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        input.collection = None;
        input.candidate_revision_sha256[0] ^= 1;
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        input.candidate_revision_sha256[0] ^= 1;
        input.base_revision_sha256[0] ^= 1;
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        input.base_revision_sha256[0] ^= 1;
        input.resolved_policy = b"different";
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        input.resolved_policy = a["resolved_policy"].as_str().unwrap().as_bytes();
        input.external_evidence = Some(b"");
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        input.external_evidence = None;
        input.host_options = b"changed";
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        input.host_options = a["host_options"].as_str().unwrap().as_bytes();
        input.evaluator = b"changed";
        assert_ne!(candidate_binding(&input).unwrap(), expected);
        let oversized = vec![0; MAX_BYTES];
        input.change = &oversized;
        assert!(candidate_binding(&input).is_err());
    }
}
