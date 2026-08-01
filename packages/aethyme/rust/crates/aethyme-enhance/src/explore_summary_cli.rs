//! `aethyme explore-summary --from <answer-json>` — the compact decision
//! surface deployed skill templates print after one bounded Explore call.
//!
//! Retirement plan Phase 5.5. Until 2026-08-01 `skills/aethyme/SKILL.md`,
//! `skills/aethyme/AGENTS.md`, and `skills/aethyme/references/explore.md`
//! told agents to run a `"$AETHYME_ROOT/.venv/bin/python"` heredoc over the
//! saved answer-json. Those templates deploy into *user* repositories, so
//! the product path depended on the Aethyme venv — which Phase 6 deletes.
//! This command is that projection, natively.
//!
//! Reader-command idiom mirrors `aethyme verify-targets --from` (engine
//! crate): both read the SAME saved answer-json, so one explore call still
//! feeds both surfaces — no double cost, no temp-file change.
//!
//! ## Why this lives in `aethyme-enhance`, not `aethyme-engine`
//!
//! The whole contract here is CPython-exact JSON emission (insertion order,
//! `ensure_ascii`, `indent=2`, `repr`-shaped floats), which is what
//! [`crate::pyjson`] provides. The engine deliberately does not depend on
//! enhance (see `aethyme-cli`'s module docs), and `aethyme-quality` already
//! sets the precedent of depending on enhance purely for `pyjson`. The
//! command is also enhance-adjacent in purpose: it exists to serve the
//! templates this crate deploys.
//!
//! ## Contract decision: no `schema_version`
//!
//! Unlike `verify-targets`, this output carries NO `schema_version` key.
//! Byte-parity with the retired heredoc keeps the skill's documented
//! "inspect only these fields" contract exactly true, and this surface is
//! read by an agent's eyes rather than parsed as a data contract, so a
//! per-invocation version key is pure token cost. Adding one later is an
//! additive change with its own contract decision.
//!
//! ## The projection being reproduced
//!
//! ```python
//! import json, sys; d = json.load(open(sys.argv[1], encoding="utf-8"))
//! targets = []; lanes = d.get("subsystems", [])[:3]
//! subsystems = [{k: lane.get(k) for k in ("rank", "id", "label", "role", "confidence", "token_subsystems", "missing_coverage_warnings")} for lane in lanes]
//! for lane in lanes:
//!     for target in lane.get("top_verification_targets", [])[:2]:
//!         if isinstance(target, dict):
//!             row = dict(target); row.setdefault("subsystem", lane.get("role") or lane.get("id"))
//!             targets.append(row)
//! print(json.dumps({...}, indent=2))
//! ```
//!
//! Preserved exactly: top-level key order; missing top-level fields emit
//! `null` (they are `.get()`, not omitted); every subsystem entry carries
//! all seven keys in tuple order with `null` for absent ones; 3 lanes, 2
//! targets per lane, 6 targets overall; each target keeps its source key
//! order with `subsystem` appended only when absent (`setdefault`) and
//! valued `lane.role or lane.id` (Python truthiness — an empty-string role
//! falls through to id); non-dict target entries are skipped;
//! `verification_steps` truncates to 3 and defaults to `[]`; `observability`
//! is always an object holding exactly `readiness`.
//!
//! Divergences, all in shapes where CPython *raises* rather than printing
//! (so they cannot break byte-parity): a non-array `subsystems` /
//! `top_verification_targets` / `verification_steps`, a non-object
//! `observability`, and a non-object lane degrade to empty/null here instead
//! of `TypeError`/`AttributeError`. A non-object document is a usage error.

use std::fs;
use std::io::Read;

use crate::pyjson::{self, Value};

/// Keys copied out of each subsystem lane, in Python tuple order.
const SUBSYSTEM_KEYS: [&str; 7] = [
    "rank",
    "id",
    "label",
    "role",
    "confidence",
    "token_subsystems",
    "missing_coverage_warnings",
];

const MAX_LANES: usize = 3;
const MAX_TARGETS_PER_LANE: usize = 2;
const MAX_TARGETS: usize = 6;
const MAX_VERIFICATION_STEPS: usize = 3;

pub enum ExploreSummaryCliOutcome {
    Done,
    BadUsage(String),
    Failed(String),
}

enum ExploreSummaryError {
    BadUsage(String),
    Failed(String),
}

pub fn run(args: &[String]) -> ExploreSummaryCliOutcome {
    match run_inner(args) {
        Ok(()) => ExploreSummaryCliOutcome::Done,
        Err(ExploreSummaryError::BadUsage(message)) => ExploreSummaryCliOutcome::BadUsage(message),
        Err(ExploreSummaryError::Failed(message)) => ExploreSummaryCliOutcome::Failed(message),
    }
}

fn run_inner(args: &[String]) -> Result<(), ExploreSummaryError> {
    let from = read_option(args, "--from").map_err(ExploreSummaryError::BadUsage)?;
    let raw = read_input(&from)?;
    let document = pyjson::loads(&raw)
        .map_err(|error| ExploreSummaryError::BadUsage(format!("parse --from JSON: {error}")))?;
    if !document.is_object() {
        return Err(ExploreSummaryError::BadUsage(
            "explore-summary: --from JSON must be an object (explore --format answer-json output)"
                .to_string(),
        ));
    }
    // `print(...)` — dumps plus the trailing newline.
    println!("{}", pyjson::dumps_indent2(&project(&document)));
    Ok(())
}

/// The compact projection. Pure so the parity tests can drive it directly.
pub fn project(document: &Value) -> Value {
    let lanes: Vec<&Value> = document
        .get("subsystems")
        .and_then(Value::as_array)
        .map(|items| items.iter().take(MAX_LANES).collect())
        .unwrap_or_default();

    let subsystems = Value::Array(
        lanes
            .iter()
            .map(|lane| {
                Value::Object(
                    SUBSYSTEM_KEYS
                        .iter()
                        .map(|key| {
                            (
                                (*key).to_string(),
                                lane.get(key).cloned().unwrap_or(Value::Null),
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
    );

    let mut targets = Vec::new();
    for lane in &lanes {
        let lane_targets = lane
            .get("top_verification_targets")
            .and_then(Value::as_array)
            .map(|items| items.as_slice())
            .unwrap_or(&[]);
        for target in lane_targets.iter().take(MAX_TARGETS_PER_LANE) {
            // `if isinstance(target, dict)` — anything else is skipped.
            if !target.is_object() {
                continue;
            }
            // `dict(target)` keeps the source key order; `setdefault`
            // appends `subsystem` at the end only when it is absent.
            let mut row = target.clone();
            if row.get("subsystem").is_none() {
                row.set("subsystem", lane_subsystem_label(lane));
            }
            targets.push(row);
        }
    }
    targets.truncate(MAX_TARGETS);

    let verification_steps = document
        .get("verification_steps")
        .and_then(Value::as_array)
        .map(|items| items.iter().take(MAX_VERIFICATION_STEPS).cloned().collect())
        .unwrap_or_default();

    let readiness = document
        .get("observability")
        .and_then(|observability| observability.get("readiness"))
        .cloned()
        .unwrap_or(Value::Null);

    Value::Object(vec![
        (
            "safe_to_use_as_answer".to_string(),
            document
                .get("safe_to_use_as_answer")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "trust_policy".to_string(),
            document.get("trust_policy").cloned().unwrap_or(Value::Null),
        ),
        ("subsystems".to_string(), subsystems),
        (
            "top_verification_targets".to_string(),
            Value::Array(targets),
        ),
        (
            "verification_steps".to_string(),
            Value::Array(verification_steps),
        ),
        (
            "observability".to_string(),
            Value::Object(vec![("readiness".to_string(), readiness)]),
        ),
    ])
}

/// `lane.get("role") or lane.get("id")` — Python truthiness, so an empty
/// string (or `0`, `[]`, `null`, missing) role falls through to the id.
fn lane_subsystem_label(lane: &Value) -> Value {
    match lane.get("role") {
        Some(role) if role.truthy() => role.clone(),
        _ => lane.get("id").cloned().unwrap_or(Value::Null),
    }
}

fn read_input(from: &str) -> Result<String, ExploreSummaryError> {
    if from == "-" {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| ExploreSummaryError::Failed(format!("read stdin: {error}")))?;
        return Ok(input);
    }
    fs::read_to_string(from)
        .map_err(|error| ExploreSummaryError::Failed(format!("read --from {from}: {error}")))
}

fn read_option(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing required option: {flag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(raw: &str) -> String {
        let document = pyjson::loads(raw).expect("fixture parses");
        format!("{}\n", pyjson::dumps_indent2(&project(&document)))
    }

    #[test]
    fn projects_all_fields_present() {
        let raw = r#"{
          "safe_to_use_as_answer": true,
          "trust_policy": "verify-first",
          "extra_noise": {"dropped": 1},
          "subsystems": [
            {
              "rank": 1,
              "id": "auth",
              "label": "Auth",
              "role": "credential validator",
              "confidence": 0.75,
              "token_subsystems": ["auth"],
              "missing_coverage_warnings": [],
              "top_verification_targets": [
                {"kind": "function", "target": "validate", "path": "src/auth.py"}
              ]
            }
          ],
          "verification_steps": ["read src/auth.py"],
          "observability": {"readiness": "ready", "other": "dropped"}
        }"#;
        assert_eq!(
            render(raw),
            r#"{
  "safe_to_use_as_answer": true,
  "trust_policy": "verify-first",
  "subsystems": [
    {
      "rank": 1,
      "id": "auth",
      "label": "Auth",
      "role": "credential validator",
      "confidence": 0.75,
      "token_subsystems": [
        "auth"
      ],
      "missing_coverage_warnings": []
    }
  ],
  "top_verification_targets": [
    {
      "kind": "function",
      "target": "validate",
      "path": "src/auth.py",
      "subsystem": "credential validator"
    }
  ],
  "verification_steps": [
    "read src/auth.py"
  ],
  "observability": {
    "readiness": "ready"
  }
}
"#
        );
    }

    #[test]
    fn missing_top_level_fields_become_null_not_omitted() {
        assert_eq!(
            render("{}"),
            r#"{
  "safe_to_use_as_answer": null,
  "trust_policy": null,
  "subsystems": [],
  "top_verification_targets": [],
  "verification_steps": [],
  "observability": {
    "readiness": null
  }
}
"#
        );
    }

    #[test]
    fn lane_keys_are_always_all_seven_in_tuple_order() {
        let out = render(r#"{"subsystems": [{"id": "only-id", "label": "L"}]}"#);
        assert!(
            out.contains(
                r#"    {
      "rank": null,
      "id": "only-id",
      "label": "L",
      "role": null,
      "confidence": null,
      "token_subsystems": null,
      "missing_coverage_warnings": null
    }"#
            ),
            "{out}"
        );
    }

    #[test]
    fn empty_subsystems_array_renders_inline() {
        assert!(render(r#"{"subsystems": []}"#).contains("\"subsystems\": [],"));
    }

    #[test]
    fn keeps_only_first_three_lanes() {
        let lane = |id: &str| {
            format!(r#"{{"id": "{id}", "top_verification_targets": [{{"path": "{id}.py"}}]}}"#)
        };
        let raw = format!(
            r#"{{"subsystems": [{}, {}, {}, {}]}}"#,
            lane("a"),
            lane("b"),
            lane("c"),
            lane("d")
        );
        let out = render(&raw);
        for id in ["a", "b", "c"] {
            assert!(out.contains(&format!("\"{id}.py\"")), "{out}");
        }
        assert!(!out.contains("\"d.py\""), "{out}");
        assert!(!out.contains("\"id\": \"d\""), "{out}");
    }

    #[test]
    fn keeps_only_first_two_targets_per_lane() {
        let raw = r#"{"subsystems": [{"id": "a", "top_verification_targets": [
            {"path": "t1"}, {"path": "t2"}, {"path": "t3"}, {"path": "t4"}, {"path": "t5"}
        ]}]}"#;
        let out = render(raw);
        assert!(out.contains("\"t1\"") && out.contains("\"t2\""), "{out}");
        assert!(!out.contains("\"t3\""), "{out}");
    }

    #[test]
    fn truncates_overall_targets_at_six() {
        // 3 lanes x 2 kept targets = 6; a fourth lane cannot contribute
        // anyway, so drive the cap with a lane whose targets are kept.
        let lane = |id: &str| {
            format!(
                r#"{{"id": "{id}", "top_verification_targets": [{{"path": "{id}1"}}, {{"path": "{id}2"}}]}}"#
            )
        };
        let raw = format!(
            r#"{{"subsystems": [{}, {}, {}]}}"#,
            lane("a"),
            lane("b"),
            lane("c")
        );
        let out = render(&raw);
        assert_eq!(out.matches("\"path\":").count(), 6, "{out}");
    }

    #[test]
    fn lane_with_zero_targets_contributes_none() {
        let out = render(r#"{"subsystems": [{"id": "a", "top_verification_targets": []}]}"#);
        assert!(out.contains("\"top_verification_targets\": [],"), "{out}");
    }

    #[test]
    fn existing_subsystem_key_is_left_alone_and_keeps_its_position() {
        let out = render(
            r#"{"subsystems": [{"id": "a", "role": "lane role", "top_verification_targets": [
                {"subsystem": "pinned", "path": "t1"}
            ]}]}"#,
        );
        assert!(
            out.contains(
                r#"    {
      "subsystem": "pinned",
      "path": "t1"
    }"#
            ),
            "{out}"
        );
        assert!(!out.contains("lane role\"\n    }"), "{out}");
    }

    #[test]
    fn non_dict_target_entries_are_skipped() {
        let out = render(
            r#"{"subsystems": [{"id": "a", "top_verification_targets": ["bare-string", {"path": "t2"}]}]}"#,
        );
        assert!(!out.contains("bare-string"), "{out}");
        // The string still consumed one of the two per-lane slots.
        assert!(out.contains("\"t2\""), "{out}");
    }

    #[test]
    fn empty_string_role_falls_through_to_id() {
        let out = render(
            r#"{"subsystems": [{"id": "fallback-id", "role": "", "top_verification_targets": [{"path": "t"}]}]}"#,
        );
        assert!(out.contains("\"subsystem\": \"fallback-id\""), "{out}");
    }

    #[test]
    fn null_role_falls_through_to_id_and_missing_id_is_null() {
        let out = render(
            r#"{"subsystems": [{"role": null, "top_verification_targets": [{"path": "t"}]}]}"#,
        );
        assert!(out.contains("\"subsystem\": null"), "{out}");
    }

    #[test]
    fn verification_steps_truncate_to_three() {
        let out = render(r#"{"verification_steps": ["a", "b", "c", "d"]}"#);
        assert!(out.contains("\"c\""), "{out}");
        assert!(!out.contains("\"d\""), "{out}");
    }

    #[test]
    fn observability_is_always_an_object_with_only_readiness() {
        assert!(render(r#"{"observability": {"other": 1}}"#).contains(
            r#"  "observability": {
    "readiness": null
  }"#
        ));
    }

    #[test]
    fn non_ascii_is_escaped_like_cpython_ensure_ascii() {
        let out = render(
            r#"{"trust_policy": "vérifier", "subsystems": [{"id": "café", "label": "日本語", "top_verification_targets": [{"path": "src/naïve/🚀.py"}]}]}"#,
        );
        assert!(
            out.contains(concat!(r#""trust_policy": "v"#, "\\u00e9rifier\"")),
            "{out}"
        );
        assert!(out.contains(concat!(r#""id": "caf"#, "\\u00e9\"")), "{out}");
        assert!(
            out.contains(concat!(r#""label": ""#, "\\u65e5\\u672c\\u8a9e\"")),
            "{out}"
        );
        // Astral chars become surrogate pairs, like CPython.
        assert!(
            out.contains(concat!(
                r#""path": "src/na"#,
                "\\u00efve/\\ud83d\\ude80.py\""
            )),
            "{out}"
        );
        // The lane id (non-ASCII) also flows into the added subsystem key.
        assert!(
            out.contains(concat!(r#""subsystem": "caf"#, "\\u00e9\"")),
            "{out}"
        );
        assert!(out.is_ascii(), "output must be pure ASCII: {out}");
    }

    #[test]
    fn non_array_containers_degrade_instead_of_raising() {
        // CPython raises here; we emit the empty/null shape.
        let out = render(
            r#"{"subsystems": {"not": "a list"}, "verification_steps": "nope", "observability": null}"#,
        );
        assert!(out.contains("\"subsystems\": [],"), "{out}");
        assert!(out.contains("\"verification_steps\": [],"), "{out}");
        assert!(out.contains("\"readiness\": null"), "{out}");
    }

    #[test]
    fn missing_from_option_is_a_usage_error() {
        match run(&["--repo".to_string(), ".".to_string()]) {
            ExploreSummaryCliOutcome::BadUsage(message) => {
                assert!(message.contains("--from"), "{message}");
            }
            _ => panic!("expected BadUsage"),
        }
    }

    #[test]
    fn unreadable_from_path_fails() {
        match run(&[
            "--from".to_string(),
            "/nonexistent/aethyme-explore-summary.json".to_string(),
        ]) {
            ExploreSummaryCliOutcome::Failed(message) => {
                assert!(message.contains("read --from"), "{message}");
            }
            _ => panic!("expected Failed"),
        }
    }
}
