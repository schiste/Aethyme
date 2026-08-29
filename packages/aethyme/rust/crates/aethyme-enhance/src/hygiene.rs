//! Typed commit-message hygiene — byte-parity port of
//! `src/indexing/commit_hygiene.py` (default template + linter).
//!
//! The lint result is built as an ordered [`Value`] so
//! `json.dumps(result, indent=2)` byte shapes survive the port. Note the
//! Python original emits *different key orders* for the empty-message
//! result (`required_sections` before `recognized_sections`) and the
//! normal result (`recognized_sections` before `required_sections`);
//! both quirks are preserved.

use crate::pyjson::Value;

const SUBSTANTIVE_SECTIONS: &[&str] = &["Problem", "Decision", "Rationale", "Validation"];
const SUBJECT_ONLY_SECTIONS: &[&str] = &[];
pub const OPTIONAL_SECTIONS: [&str; 4] =
    ["Alternatives considered", "Risks", "Follow-up", "Memory"];

/// Commit-type-specific requirements shared by template generation,
/// linting, and the generated agent-guidance contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitPolicy {
    pub commit_type: &'static str,
    pub body_required: bool,
    pub required_sections: &'static [&'static str],
}

pub const COMMIT_POLICIES: [CommitPolicy; 9] = [
    CommitPolicy {
        commit_type: "fix",
        body_required: true,
        required_sections: SUBSTANTIVE_SECTIONS,
    },
    CommitPolicy {
        commit_type: "feat",
        body_required: true,
        required_sections: SUBSTANTIVE_SECTIONS,
    },
    CommitPolicy {
        commit_type: "refactor",
        body_required: true,
        required_sections: SUBSTANTIVE_SECTIONS,
    },
    CommitPolicy {
        commit_type: "perf",
        body_required: true,
        required_sections: SUBSTANTIVE_SECTIONS,
    },
    CommitPolicy {
        commit_type: "test",
        body_required: false,
        required_sections: SUBJECT_ONLY_SECTIONS,
    },
    CommitPolicy {
        commit_type: "docs",
        body_required: false,
        required_sections: SUBJECT_ONLY_SECTIONS,
    },
    CommitPolicy {
        commit_type: "build",
        body_required: false,
        required_sections: SUBJECT_ONLY_SECTIONS,
    },
    CommitPolicy {
        commit_type: "chore",
        body_required: false,
        required_sections: SUBJECT_ONLY_SECTIONS,
    },
    CommitPolicy {
        commit_type: "revert",
        body_required: false,
        required_sections: SUBJECT_ONLY_SECTIONS,
    },
];

pub fn commit_policy(commit_type: &str) -> Option<&'static CommitPolicy> {
    COMMIT_POLICIES
        .iter()
        .find(|policy| policy.commit_type == commit_type)
}

fn known_sections() -> impl Iterator<Item = &'static str> {
    SUBSTANTIVE_SECTIONS
        .iter()
        .copied()
        .chain(OPTIONAL_SECTIONS)
}

/// Python `str.isspace()`-compatible whitespace test. `char::is_whitespace`
/// covers the Unicode White_Space property (incl. U+0085); Python's
/// `isspace` additionally treats the ASCII separator controls as space.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_isspace)
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// The typed commit message template used by Aethyme commit hygiene.
pub fn default_template(commit_type: &str, scope: &str) -> String {
    let policy = commit_policy(commit_type).unwrap_or_else(|| {
        commit_policy("fix").expect("the built-in fix commit policy must exist")
    });
    let mut lines = vec![format!("{}({scope}): short summary", policy.commit_type)];
    if !policy.body_required {
        return format!("{}\n", lines[0]);
    }
    for section in policy.required_sections {
        lines.push(String::new());
        lines.push(format!("{section}:"));
        lines.push(if *section == "Validation" {
            "- ...".to_string()
        } else {
            "...".to_string()
        });
    }
    lines.extend(
        [
            "",
            "Alternatives considered:",
            "- ...",
            "",
            "Risks:",
            "- ...",
            "",
            "Memory:",
            "...",
        ]
        .map(str::to_string),
    );
    format!("{}\n", lines.join("\n"))
}

struct ParsedSubject {
    commit_type: String,
    scope: Option<String>,
    summary: String,
}

/// Lint a commit message against the typed Aethyme hygiene contract.
/// Returns the exact Python result dict as an ordered [`Value`].
pub fn lint_commit_message(message: &str) -> Value {
    // Python: message.strip("\n") — newlines only.
    let stripped = message.trim_matches('\n');
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if py_strip(stripped).is_empty() {
        return obj(vec![
            ("ok", Value::Bool(false)),
            (
                "errors",
                Value::Array(vec![Value::str("Commit message is empty.")]),
            ),
            ("warnings", Value::Array(vec![])),
            ("subject", Value::Null),
            ("sections", Value::object()),
            ("required_sections", Value::Array(vec![])),
            ("recognized_sections", Value::Array(vec![])),
            ("body_required", Value::Bool(false)),
            ("memory_candidates", Value::Array(vec![])),
        ]);
    }

    let lines = crate::util::py_splitlines(stripped);
    let subject_line = py_strip(lines.first().copied().unwrap_or(""));
    let subject = parse_subject(subject_line, &mut errors);
    let sections = parse_sections(&lines[1.min(lines.len())..]);

    let subject_len = subject_line.chars().count();
    if subject_len > 72 {
        warnings.push(format!(
            "Subject is {subject_len} characters; prefer 72 or fewer."
        ));
    }

    if let Some(parsed) = &subject {
        if parsed.scope.is_none() {
            warnings.push(
                "Subject has no scope; prefer `type(scope): summary` for durable routing."
                    .to_string(),
            );
        }
    }

    let mut required_sections: &[&str] = &[];
    let mut body_required = false;
    if let Some(parsed) = &subject {
        let policy = commit_policy(&parsed.commit_type)
            .expect("parsed subjects always carry an allowed commit type");
        body_required = policy.body_required;
        required_sections = policy.required_sections;
        if body_required {
            let any_content = sections.iter().any(|(_, text)| !py_strip(text).is_empty());
            if !any_content {
                errors.push(format!(
                    "Structured body is required for substantive commits: {}.",
                    required_sections.join(", ")
                ));
            }
        }
    }

    for section_name in required_sections {
        match sections.iter().find(|(name, _)| name == section_name) {
            None => {
                errors.push(format!("Missing required section: {section_name}."));
            }
            Some((_, text)) => {
                if py_strip(text).is_empty() {
                    errors.push(format!("Section `{section_name}` must not be empty."));
                }
            }
        }
    }

    let memory_candidates = memory_candidates(&sections);
    obj(vec![
        ("ok", Value::Bool(errors.is_empty())),
        (
            "errors",
            Value::Array(errors.into_iter().map(Value::Str).collect()),
        ),
        (
            "warnings",
            Value::Array(warnings.into_iter().map(Value::Str).collect()),
        ),
        (
            "subject",
            match &subject {
                None => Value::Null,
                Some(parsed) => obj(vec![
                    ("type", Value::str(parsed.commit_type.clone())),
                    (
                        "scope",
                        match &parsed.scope {
                            Some(scope) => Value::str(scope.clone()),
                            None => Value::Null,
                        },
                    ),
                    ("summary", Value::str(parsed.summary.clone())),
                ]),
            },
        ),
        (
            "sections",
            Value::Object(
                sections
                    .iter()
                    .map(|(name, text)| (name.clone(), Value::str(text.clone())))
                    .collect(),
            ),
        ),
        (
            "recognized_sections",
            Value::Array(
                sections
                    .iter()
                    .map(|(name, _)| Value::str(name.clone()))
                    .collect(),
            ),
        ),
        (
            "required_sections",
            Value::Array(
                required_sections
                    .iter()
                    .map(|name| Value::str(*name))
                    .collect(),
            ),
        ),
        ("body_required", Value::Bool(body_required)),
        ("memory_candidates", Value::Array(memory_candidates)),
    ])
}

/// `SUBJECT_PATTERN.match(...)` without a regex dependency. The pattern
/// `^([a-z]+)(?:\(([^)]+)\))?: (.+)$` reduces to: maximal ASCII-lowercase
/// prefix (backtracking it can never help — the boundary char is
/// lowercase, never `(` or `:`), an optional `(scope)` whose scope runs
/// to the FIRST `)` (the class excludes `)` so greedy cannot cross it),
/// then a literal `": "` and a non-empty single-line remainder.
fn parse_subject(subject_line: &str, errors: &mut Vec<String>) -> Option<ParsedSubject> {
    let no_match = |errors: &mut Vec<String>| {
        errors.push("Subject must match `type(scope): summary` or `type: summary`.".to_string());
        None
    };

    let type_end = subject_line
        .char_indices()
        .find(|(_, c)| !c.is_ascii_lowercase())
        .map(|(i, _)| i)
        .unwrap_or(subject_line.len());
    if type_end == 0 {
        return no_match(errors);
    }
    let commit_type = &subject_line[..type_end];
    let rest = &subject_line[type_end..];

    let (scope, remainder) = if let Some(after_paren) = rest.strip_prefix('(') {
        match after_paren.find(')') {
            Some(close) if close > 0 => {
                let scope = &after_paren[..close];
                let after = &after_paren[close + 1..];
                if let Some(summary) = after.strip_prefix(": ") {
                    (Some(scope), summary)
                } else {
                    // Optional-group backtrack: `: ` directly after the type
                    // would have to start at `(` — impossible.
                    return no_match(errors);
                }
            }
            _ => return no_match(errors),
        }
    } else if let Some(summary) = rest.strip_prefix(": ") {
        (None, summary)
    } else {
        return no_match(errors);
    };

    // `.+$`: at least one char, no newline (subject lines are single-line).
    if remainder.is_empty() {
        return no_match(errors);
    }

    if commit_policy(commit_type).is_none() {
        errors.push(format!(
            "Unsupported commit type `{commit_type}`. Allowed types: {}.",
            COMMIT_POLICIES
                .iter()
                .map(|policy| policy.commit_type)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return None;
    }
    let summary = py_strip(remainder);
    if summary.is_empty() {
        errors.push("Subject summary must not be empty.".to_string());
        return None;
    }
    Some(ParsedSubject {
        commit_type: commit_type.to_string(),
        scope: scope.map(|s| py_strip(s).to_string()),
        summary: summary.to_string(),
    })
}

/// `_parse_sections`: ordered by first appearance; duplicate headers keep
/// appending to the earlier bucket; content joined then stripped.
fn parse_sections(body_lines: &[&str]) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_section: Option<usize> = None;
    for raw_line in body_lines {
        let line = py_rstrip(raw_line);
        if let Some((section, initial_content)) = parse_section_header(line) {
            let index = match sections.iter().position(|(name, _)| name == section) {
                Some(index) => index,
                None => {
                    sections.push((section.to_string(), Vec::new()));
                    sections.len() - 1
                }
            };
            current_section = Some(index);
            if !initial_content.is_empty() {
                sections[index].1.push(initial_content.to_string());
            }
            continue;
        }
        let Some(index) = current_section else {
            continue;
        };
        sections[index].1.push(line.to_string());
    }

    sections
        .into_iter()
        .map(|(name, content_lines)| {
            let text = py_strip(&content_lines.join("\n")).to_string();
            (name, text)
        })
        .collect()
}

fn parse_section_header<'a>(line: &'a str) -> Option<(&'static str, &'a str)> {
    known_sections().find_map(|section| {
        let remainder = line.strip_prefix(section)?.strip_prefix(':')?;
        if remainder.is_empty() {
            return Some((section, ""));
        }
        remainder
            .chars()
            .next()
            .filter(|character| py_isspace(*character))
            .map(|_| (section, py_strip(remainder)))
    })
}

fn memory_candidates(sections: &[(String, String)]) -> Vec<Value> {
    let section_text = |name: &str| -> Option<&str> {
        sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, text)| text.as_str())
            .filter(|text| !text.is_empty())
    };
    let mut candidates = Vec::new();
    for (candidate_type, source_section) in [
        ("decision", "Decision"),
        ("memory-note", "Memory"),
        ("gotcha", "Risks"),
        ("validation-rule", "Validation"),
    ] {
        if let Some(text) = section_text(source_section) {
            candidates.push(obj(vec![
                ("type", Value::str(candidate_type)),
                ("source_section", Value::str(source_section)),
                ("summary", Value::str(first_line(text))),
            ]));
        }
    }
    candidates
}

fn first_line(text: &str) -> String {
    for line in crate::util::py_splitlines(text) {
        let stripped = py_strip(line);
        if !stripped.is_empty() {
            let without_prefix = stripped.strip_prefix("- ").unwrap_or(stripped);
            return py_strip(without_prefix).to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson;

    #[test]
    fn policy_table_covers_every_allowed_type_and_preserves_requirements() {
        assert_eq!(
            COMMIT_POLICIES
                .iter()
                .map(|policy| policy.commit_type)
                .collect::<Vec<_>>(),
            vec!["fix", "feat", "refactor", "perf", "test", "docs", "build", "chore", "revert",]
        );
        for policy in &COMMIT_POLICIES[..4] {
            assert!(policy.body_required);
            assert_eq!(policy.required_sections, SUBSTANTIVE_SECTIONS);
        }
        for policy in &COMMIT_POLICIES[4..] {
            assert!(!policy.body_required);
            assert_eq!(policy.required_sections, SUBJECT_ONLY_SECTIONS);
        }
    }

    #[test]
    fn default_template_includes_required_sections_for_fix() {
        let template = default_template("fix", "watchlist");
        assert!(template.starts_with("fix(watchlist): short summary\n"));
        assert!(template.contains("Problem:\n...\n"));
        assert!(template.contains("Decision:\n...\n"));
        assert!(template.contains("Rationale:\n...\n"));
        assert!(template.contains("Validation:\n- ...\n"));
        assert!(template.contains("Memory:\n...\n"));
    }

    #[test]
    fn default_templates_are_subject_only_for_non_substantive_types() {
        for policy in COMMIT_POLICIES
            .iter()
            .filter(|policy| !policy.body_required)
        {
            assert_eq!(
                default_template(policy.commit_type, "guide"),
                format!("{}(guide): short summary\n", policy.commit_type)
            );
        }
        // Unknown type normalizes to fix.
        assert!(default_template("nope", "s").starts_with("fix(s): short summary\n"));
    }

    #[test]
    fn lint_accepts_structured_fix() {
        let message = "fix(watchlist): mark only viewed revision as seen\n\nProblem:\nViewing a diff marked every revision as seen.\n\nDecision:\nUse the viewed revision id for seen-marking.\n\nRationale:\nSeen state is revision-scoped.\n\nValidation:\n- Added regression coverage.\n- Ran watchlist tests.\n\nMemory:\nWatchlist seen-marking must remain revision-scoped.\n";
        let result = lint_commit_message(message);
        assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
        let subject = result.get("subject").unwrap();
        assert_eq!(subject.get("type").and_then(Value::as_str), Some("fix"));
        assert_eq!(
            subject.get("scope").and_then(Value::as_str),
            Some("watchlist")
        );
        assert_eq!(
            subject.get("summary").and_then(Value::as_str),
            Some("mark only viewed revision as seen")
        );
        let recognized = result.get("recognized_sections").unwrap();
        assert!(recognized
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("Decision")));
        let candidates = result.get("memory_candidates").unwrap().as_array().unwrap();
        assert!(candidates
            .iter()
            .any(|c| c.get("type").and_then(Value::as_str) == Some("decision")));
    }

    #[test]
    fn parse_sections_accepts_standalone_headers() {
        assert_eq!(
            parse_sections(&["Problem:", "Standalone content."]),
            vec![("Problem".to_string(), "Standalone content.".to_string())]
        );
    }

    #[test]
    fn parse_sections_accepts_inline_and_multiline_content() {
        assert_eq!(
            parse_sections(&[
                "Problem: Initial content.",
                "Continuation.",
                "Decision: Chosen approach.",
            ]),
            vec![
                (
                    "Problem".to_string(),
                    "Initial content.\nContinuation.".to_string(),
                ),
                ("Decision".to_string(), "Chosen approach.".to_string()),
            ]
        );
    }

    #[test]
    fn parse_sections_append_duplicate_headers_deterministically() {
        assert_eq!(
            parse_sections(&["Problem: First.", "Problem:", "Second.", "Problem: Third.",]),
            vec![("Problem".to_string(), "First.\nSecond.\nThird.".to_string(),)]
        );
    }

    #[test]
    fn parse_sections_accepts_empty_inline_headers() {
        assert_eq!(
            parse_sections(&["Problem:   "]),
            vec![("Problem".to_string(), String::new())]
        );
    }

    #[test]
    fn parse_sections_accepts_unicode_inline_content_and_whitespace() {
        assert_eq!(
            parse_sections(&["Problem:\u{2003}Déjà vu — 問題"]),
            vec![("Problem".to_string(), "Déjà vu — 問題".to_string())]
        );
    }

    #[test]
    fn parse_sections_leave_unknown_headers_as_content() {
        assert_eq!(
            parse_sections(&[
                "Unknown: ignored before a known section",
                "Problem: Known.",
                "Unknown: retained inside the section",
            ]),
            vec![(
                "Problem".to_string(),
                "Known.\nUnknown: retained inside the section".to_string(),
            )]
        );
    }

    #[test]
    fn parse_sections_do_not_recognize_headers_mid_line_or_without_spacing() {
        assert_eq!(
            parse_sections(&[
                "The Problem: is described here.",
                "Problem:text without separator whitespace",
                "Decision: Valid header.",
                "The Problem: remains ordinary prose.",
            ]),
            vec![(
                "Decision".to_string(),
                "Valid header.\nThe Problem: remains ordinary prose.".to_string(),
            )]
        );
    }

    #[test]
    fn lint_rejects_missing_required_section() {
        let message = "feat(repo-memory): add commit hygiene tool\n\nProblem:\nAgents need a consistent commit format.\n\nDecision:\nAdd a linter and template generator.\n\nValidation:\n- Added tests.\n";
        let result = lint_commit_message(message);
        assert_eq!(result.get("ok"), Some(&Value::Bool(false)));
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert!(errors
            .iter()
            .any(|e| e.as_str() == Some("Missing required section: Rationale.")));
    }

    #[test]
    fn lint_empty_message_matches_python_key_order() {
        let result = lint_commit_message("\n\n");
        let json = pyjson::dumps_indent2(&result);
        // The empty-result branch orders required_sections BEFORE
        // recognized_sections (opposite of the normal branch).
        let required = json.find("\"required_sections\"").unwrap();
        let recognized = json.find("\"recognized_sections\"").unwrap();
        assert!(required < recognized);
        assert!(json.contains("\"Commit message is empty.\""));
    }

    #[test]
    fn lint_normal_result_key_order_and_warnings() {
        let long_summary = "x".repeat(80);
        let message = format!("docs: {long_summary}");
        let result = lint_commit_message(&message);
        let json = pyjson::dumps_indent2(&result);
        let recognized = json.find("\"recognized_sections\"").unwrap();
        let required = json.find("\"required_sections\"").unwrap();
        assert!(recognized < required);
        let warnings = result.get("warnings").unwrap().as_array().unwrap();
        assert_eq!(
            warnings[0].as_str(),
            Some("Subject is 86 characters; prefer 72 or fewer.")
        );
        assert_eq!(
            warnings[1].as_str(),
            Some("Subject has no scope; prefer `type(scope): summary` for durable routing.")
        );
        assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
        assert!(result.get("errors").unwrap().as_array().unwrap().is_empty());
        assert!(result
            .get("required_sections")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn subject_only_messages_pass_for_every_non_substantive_type() {
        for policy in COMMIT_POLICIES
            .iter()
            .filter(|policy| !policy.body_required)
        {
            let result = lint_commit_message(&format!("{}(scope): summary", policy.commit_type));
            assert_eq!(
                result.get("ok"),
                Some(&Value::Bool(true)),
                "{} should allow a subject-only message",
                policy.commit_type
            );
            assert!(result
                .get("required_sections")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn subject_only_messages_fail_for_every_substantive_type() {
        for policy in COMMIT_POLICIES.iter().filter(|policy| policy.body_required) {
            let result = lint_commit_message(&format!("{}(scope): summary", policy.commit_type));
            assert_eq!(result.get("ok"), Some(&Value::Bool(false)));
            assert_eq!(
                result
                    .get("required_sections")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                4
            );
        }
    }

    #[test]
    fn subject_edge_cases_match_regex_semantics() {
        // Unsupported type is a distinct error.
        let result = lint_commit_message("wip: things");
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert!(errors.iter().any(|e| {
            e.as_str()
                == Some(
                    "Unsupported commit type `wip`. Allowed types: fix, feat, refactor, perf, test, docs, build, chore, revert.",
                )
        }));
        // Empty parens do not parse.
        let result = lint_commit_message("fix(): x");
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert!(errors.iter().any(|e| {
            e.as_str() == Some("Subject must match `type(scope): summary` or `type: summary`.")
        }));
        // Scope may contain an open paren (regex `[^)]+` allows it).
        let result = lint_commit_message(
            "fix(a(b): c\n\nProblem:\np\n\nDecision:\nd\n\nRationale:\nr\n\nValidation:\n- v\n",
        );
        assert_eq!(
            result
                .get("subject")
                .unwrap()
                .get("scope")
                .and_then(Value::as_str),
            Some("a(b")
        );
        // Whitespace-only summary: the subject line is stripped first, so
        // "fix:   " becomes "fix:" and fails the shape match (same as
        // Python — the summary-empty branch is unreachable post-strip).
        let result = lint_commit_message("fix:   ");
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert!(errors.iter().any(|e| {
            e.as_str() == Some("Subject must match `type(scope): summary` or `type: summary`.")
        }));
    }

    #[test]
    fn substantive_commit_with_no_body_gets_structured_body_error() {
        let result = lint_commit_message("feat(x): add thing");
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert_eq!(
            errors[0].as_str(),
            Some(
                "Structured body is required for substantive commits: Problem, Decision, Rationale, Validation."
            )
        );
        // Followed by the four missing-section errors, in order.
        assert_eq!(errors.len(), 5);
        assert_eq!(
            errors[1].as_str(),
            Some("Missing required section: Problem.")
        );
        assert_eq!(
            errors[4].as_str(),
            Some("Missing required section: Validation.")
        );
    }
}
