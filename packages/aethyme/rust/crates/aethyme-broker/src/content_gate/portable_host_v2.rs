//! Independent, bounded checks of the supported host-adapter observations.
//! Observations remain assertions by the host, not proof of provenance or freshness.
use super::{Item, parse_unique};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MEDIA: &str = "application/vnd.aexeo.content-adapter+json";
const LIMIT: usize = 100_000;

fn fields(value: &Value, required: &[&str], optional: &[&str]) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("expected host object")?;
    if required.iter().any(|key| !object.contains_key(*key))
        || object
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err("unsupported host fields");
    }
    Ok(())
}

fn valid_gate(gate: &Value) -> Result<&str, &'static str> {
    let kind = gate["kind"].as_str().ok_or("invalid host gate kind")?;
    let extra: &[&str] = match kind {
        "word_count" | "paragraph_count" => &["min", "max"],
        "character_count" => &["min", "max", "ignore_whitespace"],
        "paragraph_length" => &["max_words"],
        "paragraph_character_length" | "heading_length" => &["max_chars"],
        "required_headings" => &["headings"],
        "heading_hierarchy" => &["initial_level"],
        _ => return Err("host adapter portable verification does not support this gate"),
    };
    let mut required = vec!["kind", "id", "severity", "blocking"];
    required.extend_from_slice(extra);
    fields(gate, &required, &[])?;
    if gate["id"].as_str().is_none_or(str::is_empty)
        || !matches!(gate["severity"].as_str(), Some("warning" | "error"))
        || !gate["blocking"].is_boolean()
    {
        return Err("invalid host gate fields");
    }
    match kind {
        "word_count" | "character_count" | "paragraph_count" => {
            let bound = |value: &Value| -> Result<Option<u64>, &'static str> {
                if value.is_null() {
                    Ok(None)
                } else {
                    value.as_u64().map(Some).ok_or("invalid host gate bound")
                }
            };
            let min = bound(&gate["min"])?;
            let max = bound(&gate["max"])?;
            if (min.is_none() && max.is_none())
                || matches!((min, max), (Some(a), Some(b)) if a > b)
                || (kind == "character_count" && !gate["ignore_whitespace"].is_boolean())
            {
                return Err("invalid host gate bounds");
            }
        }
        "paragraph_length" | "paragraph_character_length" | "heading_length" => {
            let key = if kind == "paragraph_length" {
                "max_words"
            } else {
                "max_chars"
            };
            if gate[key].as_u64().is_none_or(|v| v == 0) {
                return Err("invalid host gate limit");
            }
        }
        "required_headings" => {
            let headings = gate["headings"]
                .as_array()
                .ok_or("invalid required headings")?;
            if headings.is_empty()
                || headings
                    .iter()
                    .any(|value| value.as_str().is_none_or(|s| s.trim().is_empty()))
            {
                return Err("invalid required headings");
            }
        }
        "heading_hierarchy" => {
            let initial = &gate["initial_level"];
            if !initial.is_null() && initial.as_u64().is_none_or(|v| !(1..=6).contains(&v)) {
                return Err("invalid initial heading level");
            }
        }
        _ => unreachable!(),
    }
    Ok(kind)
}

/// Verify every supported checked gate against the exact bound host projection.
pub(super) fn verify(
    policy: &Value,
    verdict: &Value,
    after: &[Item],
    checks: &BTreeMap<&str, &Value>,
) -> Result<bool, &'static str> {
    let host: Vec<_> = after
        .iter()
        .filter(|item| item.media_type == MEDIA)
        .collect();
    if host.is_empty() {
        return Ok(false);
    }
    let gates = policy["gates"].as_array().ok_or("invalid resolved gates")?;
    for gate in gates {
        valid_gate(gate)?;
    }
    // Index positions once so batches do not rescan every finding for every item.
    let mut unscoped_location = false;
    let mut located_entities = BTreeSet::new();
    for finding in verdict["findings"].as_array().ok_or("invalid findings")? {
        if finding
            .get("location")
            .is_some_and(|location| !location.is_null())
        {
            match finding.get("entity_id") {
                None | Some(Value::Null) => unscoped_location = true,
                Some(entity) => {
                    if let Some(entity) = entity.as_str() {
                        located_entities.insert(entity);
                    }
                }
            }
        }
    }
    let needs_words = gates.iter().any(|gate| gate["kind"] == "word_count");
    let needs_paragraph_words = gates.iter().any(|gate| gate["kind"] == "paragraph_length");
    let needs_paragraph_chars = gates
        .iter()
        .any(|gate| gate["kind"] == "paragraph_character_length");
    for item in host {
        let projection = parse_unique(item.source.as_bytes())?;
        fields(
            &projection,
            &["schema", "text"],
            &["block_ends", "paragraphs", "headings", "heading_levels"],
        )?;
        if projection["schema"] != "aexeo.content-adapter/v1" {
            return Err("unsupported host projection");
        }
        let text = projection["text"].as_str().ok_or("host text unavailable")?;
        let mut count = 0usize;
        let mut start = 0usize;
        if let Some(value) = projection.get("block_ends") {
            let ends = value.as_array().ok_or("invalid block boundaries")?;
            if ends.len() > LIMIT {
                return Err("host observation limit exceeded");
            }
            for (index, value) in ends.iter().enumerate() {
                let end = value
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or("invalid block boundary")?;
                if end > text.len() || !text.is_char_boundary(end) || (index > 0 && end <= start) {
                    return Err("invalid block boundary");
                }
                if needs_words {
                    count += text[start..end].split_whitespace().count();
                }
                start = end;
            }
        }
        if needs_words {
            count += text[start..].split_whitespace().count();
        }
        let paragraphs = projection
            .get("paragraphs")
            .map(|v| v.as_array().ok_or("invalid host paragraphs"))
            .transpose()?;
        let mut longest_words = 0usize;
        let mut longest_chars = 0usize;
        if let Some(paragraphs) = paragraphs {
            if paragraphs.len() > LIMIT {
                return Err("host observation limit exceeded");
            }
            let mut previous = 0usize;
            for paragraph in paragraphs {
                fields(paragraph, &["start", "end"], &["list_item"])?;
                let start = paragraph["start"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or("invalid host paragraph range")?;
                let end = paragraph["end"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or("invalid host paragraph range")?;
                if start < previous
                    || end <= start
                    || end > text.len()
                    || !text.is_char_boundary(start)
                    || !text.is_char_boundary(end)
                    || paragraph
                        .get("list_item")
                        .is_some_and(|v| !v.is_null() && !v.is_boolean())
                {
                    return Err("invalid host paragraph range");
                }
                let paragraph_text = &text[start..end];
                if needs_paragraph_words {
                    longest_words = longest_words.max(paragraph_text.split_whitespace().count());
                }
                if needs_paragraph_chars {
                    longest_chars = longest_chars.max(paragraph_text.chars().count());
                }
                previous = end;
            }
        }
        let headings = projection
            .get("headings")
            .map(|value| -> Result<Vec<&str>, &'static str> {
                let values = value.as_array().ok_or("invalid host headings")?;
                if values.len() > LIMIT {
                    return Err("host observation limit exceeded");
                }
                values
                    .iter()
                    .map(|v| v.as_str().ok_or("invalid host heading"))
                    .collect()
            })
            .transpose()?;
        let heading_set = headings
            .as_ref()
            .map(|values| values.iter().copied().collect::<BTreeSet<_>>());
        let levels = projection
            .get("heading_levels")
            .map(|value| -> Result<Vec<u64>, &'static str> {
                let values = value.as_array().ok_or("invalid host heading levels")?;
                if headings.as_ref().is_none_or(|h| h.len() != values.len()) {
                    return Err("invalid host heading levels");
                }
                values
                    .iter()
                    .map(|v| {
                        v.as_u64()
                            .filter(|n| (1..=6).contains(n))
                            .ok_or("invalid host heading level")
                    })
                    .collect()
            })
            .transpose()?;
        if unscoped_location || located_entities.contains(item.entity_id.as_str()) {
            return Err("host finding cannot claim a source position");
        }
        let mut character_counts = [None, None];
        for gate in gates {
            let kind = gate["kind"].as_str().ok_or("invalid host gate kind")?;
            let id = gate["id"].as_str().ok_or("invalid host gate id")?;
            let check_id = format!("gate:{}:{}:{id}", item.entity_id.len(), item.entity_id);
            let status = checks
                .get(check_id.as_str())
                .and_then(|check| check["status"].as_str())
                .ok_or("host check missing")?;
            if status == "not_checked" {
                continue;
            }
            let failed = match kind {
                "word_count" | "character_count" | "paragraph_count" => {
                    let amount = match kind {
                        "word_count" => count,
                        "character_count" => {
                            let ignore = gate["ignore_whitespace"] == true;
                            *character_counts[usize::from(ignore)].get_or_insert_with(|| {
                                text.chars()
                                    .filter(|c| !ignore || !c.is_whitespace())
                                    .count()
                            })
                        }
                        _ => paragraphs.ok_or("host paragraphs unavailable")?.len(),
                    } as u64;
                    gate["min"].as_u64().is_some_and(|v| amount < v)
                        || gate["max"].as_u64().is_some_and(|v| amount > v)
                }
                "paragraph_length" | "paragraph_character_length" => {
                    paragraphs.ok_or("host paragraphs unavailable")?;
                    let (length, key) = if kind == "paragraph_length" {
                        (longest_words, "max_words")
                    } else {
                        (longest_chars, "max_chars")
                    };
                    (length as u64) > gate[key].as_u64().ok_or("invalid host gate limit")?
                }
                "required_headings" => {
                    let observed = heading_set.as_ref().ok_or("host headings unavailable")?;
                    gate["headings"]
                        .as_array()
                        .ok_or("invalid required headings")?
                        .iter()
                        .any(|heading| !observed.contains(heading.as_str().unwrap_or_default()))
                }
                "heading_length" => headings
                    .as_ref()
                    .ok_or("host headings unavailable")?
                    .iter()
                    .any(|heading| {
                        heading.trim().chars().count() as u64
                            > gate["max_chars"].as_u64().unwrap_or(0)
                    }),
                "heading_hierarchy" => {
                    headings.as_ref().ok_or("host headings unavailable")?;
                    let mut previous = gate["initial_level"].as_u64();
                    levels
                        .as_ref()
                        .ok_or("host heading levels unavailable")?
                        .iter()
                        .any(|level| {
                            let skipped = previous.is_some_and(|prior| *level > prior + 1);
                            previous = Some(*level);
                            skipped
                        })
                }
                _ => return Err("unsupported host gate"),
            };
            if status != if failed { "failed" } else { "passed" } {
                return Err("host status differs from observations");
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(projection: Value) -> Item {
        Item {
            entity_id: "article".into(),
            public_path: "/article".into(),
            media_type: MEDIA.into(),
            revision: None,
            sha256: "a".repeat(64),
            source: projection.to_string(),
            links: None,
        }
    }
    fn policy() -> Value {
        json!({"gates": [
            {"kind":"word_count", "id":"words", "severity":"warning", "blocking":true, "min":2, "max":2},
            {"kind":"paragraph_count", "id":"paragraphs", "severity":"warning", "blocking":false, "min":1, "max":1},
            {"kind":"paragraph_length", "id":"length", "severity":"warning", "blocking":false, "max_words":2},
            {"kind":"required_headings", "id":"heading", "severity":"warning", "blocking":false, "headings":["Titre"]},
            {"kind":"heading_hierarchy", "id":"hierarchy", "severity":"warning", "blocking":false, "initial_level":1}
        ]})
    }
    fn projection() -> Value {
        json!({"schema":"aexeo.content-adapter/v1", "text":"é foo", "paragraphs":[{"start":0,"end":6}], "headings":["Titre"], "heading_levels":[2]})
    }
    fn run(
        policy: &Value,
        projection: Value,
        status: &str,
        findings: Value,
    ) -> Result<bool, &'static str> {
        let check = json!({"status":status});
        let mut checks = BTreeMap::new();
        for id in ["words", "paragraphs", "length", "heading", "hierarchy"] {
            checks.insert(
                match id {
                    "words" => "gate:7:article:words",
                    "paragraphs" => "gate:7:article:paragraphs",
                    "length" => "gate:7:article:length",
                    "heading" => "gate:7:article:heading",
                    _ => "gate:7:article:hierarchy",
                },
                &check,
            );
        }
        verify(
            policy,
            &json!({"findings":findings}),
            &[item(projection)],
            &checks,
        )
    }
    #[test]
    fn checks_composed_host_gates_and_rejects_forged_status() {
        assert_eq!(run(&policy(), projection(), "passed", json!([])), Ok(true));
        assert!(run(&policy(), projection(), "failed", json!([])).is_err());
        let mut p = projection();
        p["paragraphs"][0]["end"] = json!(1); // inside a UTF-8 character
        assert!(run(&policy(), p, "passed", json!([])).is_err());
    }
    #[test]
    fn missing_observations_are_not_empty_and_untrusted_locations_are_rejected() {
        let mut p = projection();
        p.as_object_mut().unwrap().remove("paragraphs");
        assert!(run(&policy(), p.clone(), "passed", json!([])).is_err());
        assert_eq!(run(&policy(), p, "not_checked", json!([])), Ok(true));
        assert!(
            run(
                &policy(),
                projection(),
                "passed",
                json!([{"entity_id":"article", "location":{"line":1}}])
            )
            .is_err()
        );
    }
    #[test]
    fn host_location_index_respects_entity_scope() {
        for finding in [
            json!({"entity_id":"article", "location":{"line":1}}),
            json!({"entity_id":null, "location":{"line":1}}),
            json!({"location":{"line":1}}),
        ] {
            assert!(run(&policy(), projection(), "passed", json!([finding])).is_err());
        }
        for finding in [
            json!({"entity_id":"other", "location":{"line":1}}),
            json!({"entity_id":"article", "location":null}),
        ] {
            assert_eq!(
                run(&policy(), projection(), "passed", json!([finding])),
                Ok(true)
            );
        }
    }
    #[test]
    fn each_composed_status_is_checked_independently() {
        let mut p = policy();
        p["gates"].as_array_mut().unwrap().extend([
            json!({"kind":"character_count", "id":"chars", "severity":"warning", "blocking":false, "min":4, "max":4, "ignore_whitespace":true}),
            json!({"kind":"paragraph_character_length", "id":"paragraph_chars", "severity":"warning", "blocking":false, "max_chars":5}),
            json!({"kind":"heading_length", "id":"heading_chars", "severity":"warning", "blocking":false, "max_chars":5}),
        ]);
        let names = [
            "words",
            "paragraphs",
            "length",
            "heading",
            "hierarchy",
            "chars",
            "paragraph_chars",
            "heading_chars",
        ];
        for wrong in 0..names.len() {
            let values: Vec<_> = names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    (
                        format!("gate:7:article:{name}"),
                        json!({"status": if index == wrong {"failed"} else {"passed"}}),
                    )
                })
                .collect();
            let checks: BTreeMap<_, _> = values
                .iter()
                .map(|(id, check)| (id.as_str(), check))
                .collect();
            assert!(
                verify(&p, &json!({"findings":[]}), &[item(projection())], &checks).is_err(),
                "forged {}",
                names[wrong]
            );
        }
    }

    #[test]
    fn malformed_gate_is_rejected_even_when_not_checked() {
        let mut p = policy();
        p["gates"][2]["max_words"] = json!(0);
        assert!(run(&p, projection(), "not_checked", json!([])).is_err());
        p["gates"][2]["kind"] = json!("custom");
        assert!(run(&p, projection(), "not_checked", json!([])).is_err());
    }
}
