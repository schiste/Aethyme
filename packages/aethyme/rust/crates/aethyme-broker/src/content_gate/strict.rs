//! Version-pinned schema and decision-input checks for Aexeo v1.
use super::{SubjectPrechecked, precheck_v1};
use std::{collections::BTreeMap, fmt, sync::OnceLock};

const CHANGE_SCHEMA: &str = include_str!("change.schema.json");
const VERDICT_SCHEMA: &str = include_str!("verdict.schema.json");
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
static CHANGE_VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
static VERDICT_VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();

fn validator(
    schema: &'static str,
    cell: &'static OnceLock<jsonschema::Validator>,
) -> &'static jsonschema::Validator {
    cell.get_or_init(|| {
        let value = serde_json::from_str(schema).expect("bundled schema must be valid JSON");
        jsonschema::options()
            .should_validate_formats(true)
            .build(&value)
            .expect("bundled schema must compile")
    })
}
/// Parse bounded JSON while rejecting duplicate keys at every depth.
pub fn parse_unique(bytes: &[u8]) -> Result<serde_json::Value, &'static str> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err("document exceeds size limit");
    }
    let UniqueJson(value) =
        serde_json::from_slice(bytes).map_err(|_| "invalid or duplicate JSON")?;
    Ok(value)
}
/// Exact, host-collected bytes of all other known decision inputs. An absent
/// collection or external result is different from an empty one. The host is
/// responsible for including any additional inputs and establishing their origin.
#[derive(Debug, PartialEq, Eq)]
pub struct DecisionInputs {
    pub change: Vec<u8>,
    pub policy: Vec<u8>,
    pub options: Vec<u8>,
    pub collection: Option<Vec<u8>>,
    pub external: Option<Vec<u8>>,
    pub additional: BTreeMap<String, Vec<u8>>,
}
/// Checks pinned schemas, duplicates, subject, candidate bytes and independently
/// captured inputs. Success is not a publication authorization: the host must
/// ensure the snapshots are complete, authentic and current at publication and
/// synchronize the final check with publication. It must observe the real process
/// result and use a fresh verdict, not accept untrusted supplied status or cache.
pub fn strict_precheck_v1(
    before: &DecisionInputs,
    current_inputs: &DecisionInputs,
    verdict: &[u8],
    exit_code: Option<i32>,
    timed_out: bool,
    current_content: &BTreeMap<String, Option<Vec<u8>>>,
) -> Result<SubjectPrechecked, &'static str> {
    if before != current_inputs {
        return Err("decision inputs changed");
    }
    let change = parse_unique(&before.change)?;
    let result = parse_unique(verdict)?;
    if !validator(CHANGE_SCHEMA, &CHANGE_VALIDATOR).is_valid(&change) {
        return Err("change schema invalid");
    }
    if !validator(VERDICT_SCHEMA, &VERDICT_VALIDATOR).is_valid(&result) {
        return Err("verdict schema invalid");
    }
    let checks = result["checks"].as_array().ok_or("invalid checks")?;
    let count = |status: &str| {
        checks
            .iter()
            .filter(|check| check["status"] == status)
            .count() as u64
    };
    let blocking = |status: &str| {
        checks
            .iter()
            .filter(|check| check["blocking"] == true && check["status"] == status)
            .count() as u64
    };
    let summary = &result["summary"];
    if summary["checks"] != checks.len() as u64
        || summary["passed"] != count("passed")
        || summary["failed"] != count("failed")
        || summary["not_checked"] != count("not_checked")
        || summary["blocking_failed"] != blocking("failed")
        || summary["blocking_not_checked"] != blocking("not_checked")
    {
        return Err("inconsistent verdict summary");
    }
    precheck_v1(
        &before.change,
        verdict,
        exit_code,
        timed_out,
        current_content,
    )
}
struct UniqueJson(serde_json::Value);

impl<'de> serde::Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{Error, MapAccess, SeqAccess, Visitor};
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueJson;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueJson(n.into()))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_none<E: Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Null))
            }
            fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueJson(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(UniqueJson(values.into()))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(A::Error::custom("duplicate JSON key"));
                    }
                    let UniqueJson(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(UniqueJson(values.into()))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    fn fixture() -> (DecisionInputs, Vec<u8>, BTreeMap<String, Option<Vec<u8>>>) {
        let digest = format!("{:x}", Sha256::digest(b"hello"));
        let change = json!({"schema":"content-gate.change/v1","change_id":"c","created_at":"2026-01-01T00:00:00Z","origin":{"kind":"manual"},"base":{"kind":"opaque","revision":"r"},"entries":[{"entity_id":"a","operation":"update","media_type":"text/plain","after":{"sha256":digest,"inline":"hello"}}]});
        let verdict = json!({"schema":"content-gate.verdict/v1","status":"pass","subject":{"change_id":"c","base":{"kind":"opaque","revision":"r"},"entries":[{"entity_id":"a","sha256":digest}]},"validator":{"name":"aexeo","version":"1"},"evaluated_at":"2026-01-01T00:00:00Z","summary":{"checks":1,"passed":1,"failed":0,"not_checked":0,"blocking_failed":0,"blocking_not_checked":0},"checks":[{"id":"x","status":"passed","blocking":true}],"findings":[]});
        (
            DecisionInputs {
                change: serde_json::to_vec(&change).unwrap(),
                policy: b"policy".to_vec(),
                options: b"options".to_vec(),
                collection: None,
                external: None,
                additional: BTreeMap::new(),
            },
            serde_json::to_vec(&verdict).unwrap(),
            BTreeMap::from([("a".into(), Some(b"hello".to_vec()))]),
        )
    }
    fn copy(d: &DecisionInputs) -> DecisionInputs {
        DecisionInputs {
            change: d.change.clone(),
            policy: d.policy.clone(),
            options: d.options.clone(),
            collection: d.collection.clone(),
            external: d.external.clone(),
            additional: d.additional.clone(),
        }
    }
    #[test]
    fn rejects_invalid_change_nested_duplicates_and_truncation() {
        let (before, verdict, content) = fixture();
        let mut invalid = copy(&before);
        let mut value: Value = serde_json::from_slice(&invalid.change).unwrap();
        value["created_at"] = json!("not-a-date");
        invalid.change = serde_json::to_vec(&value).unwrap();
        assert!(
            strict_precheck_v1(&invalid, &invalid, &verdict, Some(0), false, &content).is_err()
        );
        let nested = String::from_utf8(verdict.clone()).unwrap().replace(
            "\"change_id\":\"c\"",
            "\"change_id\":\"c\",\"change_id\":\"c\"",
        );
        assert!(
            strict_precheck_v1(
                &before,
                &before,
                nested.as_bytes(),
                Some(0),
                false,
                &content
            )
            .is_err()
        );
        assert!(
            strict_precheck_v1(
                &before,
                &before,
                &verdict[..verdict.len() - 1],
                Some(0),
                false,
                &content
            )
            .is_err()
        );
        assert!(strict_precheck_v1(&before, &before, &verdict, Some(0), true, &content).is_err());
        assert!(strict_precheck_v1(&before, &before, &verdict, Some(2), false, &content).is_err());
    }
    #[test]
    fn checks_documents_and_every_declared_input() {
        let (before, verdict, content) = fixture();
        let mut current = copy(&before);
        assert!(strict_precheck_v1(&before, &current, &verdict, Some(0), false, &content).is_ok());
        current.policy.push(1);
        assert!(strict_precheck_v1(&before, &current, &verdict, Some(0), false, &content).is_err());
        current = copy(&before);
        current.options.push(1);
        assert!(strict_precheck_v1(&before, &current, &verdict, Some(0), false, &content).is_err());
        current = copy(&before);
        current.collection = Some(vec![]);
        assert!(strict_precheck_v1(&before, &current, &verdict, Some(0), false, &content).is_err());
        current = copy(&before);
        current.external = Some(vec![]);
        assert!(strict_precheck_v1(&before, &current, &verdict, Some(0), false, &content).is_err());
        current = copy(&before);
        current.additional.insert("other".into(), vec![]);
        assert!(strict_precheck_v1(&before, &current, &verdict, Some(0), false, &content).is_err());
        assert!(strict_precheck_v1(&before, &before, b"{", Some(0), false, &content).is_err());
        let s =
            String::from_utf8(verdict.clone())
                .unwrap()
                .replacen("{", "{\"status\":\"pass\",", 1);
        assert!(
            strict_precheck_v1(&before, &before, s.as_bytes(), Some(0), false, &content).is_err()
        );
        let mut v: Value = serde_json::from_slice(&verdict).unwrap();
        v["summary"]["blocking_failed"] = json!(1);
        assert!(
            strict_precheck_v1(
                &before,
                &before,
                v.to_string().as_bytes(),
                Some(0),
                false,
                &content
            )
            .is_err()
        );
        v["summary"]["blocking_failed"] = json!(0);
        v["evaluated_at"] = json!("bad date");
        assert!(
            strict_precheck_v1(
                &before,
                &before,
                v.to_string().as_bytes(),
                Some(0),
                false,
                &content
            )
            .is_err()
        );
    }
}
