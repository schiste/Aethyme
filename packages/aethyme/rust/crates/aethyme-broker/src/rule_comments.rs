//! A configurable comment a review rule posts on a pull request (#596).
//!
//! A rule names a template from `[review.comments.<name>]` and a key:
//!
//! ```toml
//! [[review.trigger.rule]]
//! name = "large-pr-heads-up"
//! min_tier = "large"
//! comment = { template = "large-pr", key = "size-heads-up" }
//!
//! [review.comments.large-pr]
//! body = "This pull request is **{{tier}}** ({{files_changed}} files)."
//! ```
//!
//! **Only broker-measured values are interpolated.** A template may name the
//! variables in [`RULE_COMMENT_VARIABLES`] and nothing else, and none of them
//! carries text the pull request controls as prose: no title, body, branch or
//! commit message. The one value that can echo repository content -- the
//! classification's `reasons`, which name paths -- is rendered as plain text,
//! with Markdown and mentions neutralised. The template itself is the
//! operator's, and a mention written there is deliberate: that is how a bot is
//! summoned.
//!
//! **One comment per key and pull request**, found by a hidden marker
//! ([`marker`]) on its first line and edited in place. An unchanged rendering
//! is a no-op, so a tick that sees nothing new costs no write and notifies
//! nobody.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{ChangeClassification, OwnedComment, ReviewTriggerPolicy};

/// Every variable a comment template may name.
pub const RULE_COMMENT_VARIABLES: &[&str] = &[
    "tier",
    "risk",
    "files_changed",
    "lines_added",
    "lines_deleted",
    "churn",
    "signals",
    "reasons",
    "rule",
    "pr_number",
    "head",
    "head_short",
    "base",
];

const MARKER_PREFIX: &str = "<!-- aethyme:comment:";
const MARKER_SUFFIX: &str = " -->";

/// What a rule does with its comment once it stops matching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnUnmatch {
    /// Leave the comment as it was last rendered.
    #[default]
    Keep,
    /// Delete it.
    Delete,
}

/// A rule's `comment = { template, key, on_unmatch }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleComment {
    /// A `[review.comments.<name>]` entry.
    pub template: String,
    /// Identifies the comment on the pull request; one comment per key.
    pub key: String,
    #[serde(default)]
    pub on_unmatch: OnUnmatch,
}

/// One `[review.comments.<name>]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentTemplate {
    pub body: String,
}

/// The hidden first line that identifies a rule comment.
pub fn marker(key: &str) -> String {
    format!("{MARKER_PREFIX}{key}{MARKER_SUFFIX}")
}

/// The key of a rule comment, read from its first line.
///
/// Only a comment that *starts* with the marker counts, so a reply quoting
/// one (`> <!-- aethyme:comment:… -->`) is never mistaken for it.
pub fn rule_comment_key(body: &str) -> Option<&str> {
    let first = body.trim_start().lines().next()?;
    let key = first
        .trim_end()
        .strip_prefix(MARKER_PREFIX)?
        .strip_suffix(MARKER_SUFFIX)?;
    valid_key(key).then_some(key)
}

/// A key is a short slug, so it can never close the HTML comment it sits in.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.as_bytes()[0].is_ascii_alphanumeric()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The variables a template names, or why it cannot be rendered.
pub fn template_variables(body: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err("a `{{` is never closed by `}}`".into());
        };
        let name = after[..end].trim();
        if !RULE_COMMENT_VARIABLES.contains(&name) {
            return Err(format!(
                "unknown variable `{{{{{name}}}}}`; a template may use only {}",
                RULE_COMMENT_VARIABLES.join(", ")
            ));
        }
        names.insert(name.to_string());
        rest = &after[end + 2..];
    }
    Ok(names)
}

/// Render a value as inert text inside a comment.
///
/// Markdown control characters are escaped, line breaks become spaces, and
/// `@` gets a zero-width joiner so a path or reason can never mention anyone.
pub fn escape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '!' | '|' | '<'
            | '>' | '~' | '{' | '}' => {
                out.push('\\');
                out.push(ch);
            }
            '@' => out.push_str("@\u{200d}"),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// What a rendered comment may name about its pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleCommentContext {
    pub pull_request: i64,
    pub head: String,
    pub base: String,
}

fn variable_value(
    name: &str,
    change: &ChangeClassification,
    rule: &str,
    context: &RuleCommentContext,
) -> String {
    match name {
        "tier" => change.tier.as_str().to_string(),
        "risk" => change.risk.clone(),
        "files_changed" => change.size.files_changed.to_string(),
        "lines_added" => change.size.lines_added.to_string(),
        "lines_deleted" => change.size.lines_deleted.to_string(),
        "churn" => change.size.churn.to_string(),
        "signals" => {
            let set = change.signals.set_names();
            if set.is_empty() {
                "none".to_string()
            } else {
                set.join(", ")
            }
        }
        "reasons" => change.reasons.join("; "),
        "rule" => rule.to_string(),
        "pr_number" => context.pull_request.to_string(),
        "head" => context.head.clone(),
        "head_short" => context.head.chars().take(12).collect(),
        "base" => context.base.clone(),
        _ => String::new(),
    }
}

/// The comment body for one rule: marker line, then the rendered template.
///
/// Variables were checked at load; one that is somehow unknown renders empty
/// rather than leaving template syntax on the pull request.
pub fn render_rule_comment(
    key: &str,
    template: &str,
    change: &ChangeClassification,
    rule: &str,
    context: &RuleCommentContext,
) -> String {
    let mut out = String::new();
    out.push_str(&marker(key));
    out.push('\n');
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        out.push_str(&escape_value(&variable_value(name, change, rule, context)));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// A comment a matching rule wants on the pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedComment {
    pub key: String,
    pub rule: String,
    pub body: String,
}

/// A comment whose rule stopped matching and that should go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredComment {
    pub key: String,
    pub rule: String,
}

/// The comments the rules want, and the ones they want gone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RuleCommentWants {
    pub post: Vec<PlannedComment>,
    pub retire: Vec<RetiredComment>,
    /// A rule that matched but whose comment could not be rendered.
    pub unrendered: Vec<RetiredComment>,
}

/// What the matching rules want posted, in file order.
///
/// Nothing when the trigger is disabled. A rule that matches but has no
/// measured change to render from is listed in `unrendered`, never posted with
/// empty values.
pub fn rule_comment_wants(
    policy: &ReviewTriggerPolicy,
    facts: &crate::ChangeFacts,
    context: &RuleCommentContext,
) -> RuleCommentWants {
    let mut wants = RuleCommentWants::default();
    if !policy.enabled {
        return wants;
    }
    let mut seen = BTreeSet::new();
    for (index, rule) in policy.rule.iter().enumerate() {
        let Some(comment) = &rule.comment else {
            continue;
        };
        if !seen.insert(comment.key.clone()) {
            continue;
        }
        let name = rule.label(index);
        if !rule.matches(facts) {
            if comment.on_unmatch == OnUnmatch::Delete {
                wants.retire.push(RetiredComment {
                    key: comment.key.clone(),
                    rule: name,
                });
            }
            continue;
        }
        let (Some(change), Some(template)) = (
            facts.change.as_ref(),
            policy.comment_templates.get(&comment.template),
        ) else {
            wants.unrendered.push(RetiredComment {
                key: comment.key.clone(),
                rule: name,
            });
            continue;
        };
        wants.post.push(PlannedComment {
            key: comment.key.clone(),
            body: render_rule_comment(&comment.key, &template.body, change, &name, context),
            rule: name,
        });
    }
    wants
}

/// One write a rule comment needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RuleCommentAction {
    Create {
        key: String,
        rule: String,
        body: String,
    },
    Update {
        key: String,
        rule: String,
        comment_id: i64,
        body: String,
    },
    Delete {
        key: String,
        rule: String,
        comment_id: i64,
    },
}

impl RuleCommentAction {
    /// Arguments for `aethyme broker advanced gh --repo <repository> -- <these>`.
    ///
    /// Each is one of the exact forms the gh ref guard (#566) allowlists as a
    /// write that cannot touch a branch, so none needs an acknowledgement.
    pub fn gh_args(&self, repository: &str, pull_request: i64) -> Vec<String> {
        match self {
            Self::Create { body, .. } => vec![
                "pr".into(),
                "comment".into(),
                pull_request.to_string(),
                "--body".into(),
                body.clone(),
            ],
            Self::Update {
                comment_id, body, ..
            } => vec![
                "api".into(),
                "-X".into(),
                "PATCH".into(),
                format!("repos/{repository}/issues/comments/{comment_id}"),
                "-f".into(),
                format!("body={body}"),
            ],
            Self::Delete { comment_id, .. } => vec![
                "api".into(),
                "-X".into(),
                "DELETE".into(),
                format!("repos/{repository}/issues/comments/{comment_id}"),
            ],
        }
    }

    /// The `--reason` for the coordinated write, naming the rule and key, so
    /// the operation journal records which rule posted what.
    pub fn reason(&self, pull_request: i64) -> String {
        let (verb, key, rule) = match self {
            Self::Create { key, rule, .. } => ("post", key, rule),
            Self::Update { key, rule, .. } => ("update", key, rule),
            Self::Delete { key, rule, .. } => ("delete", key, rule),
        };
        format!(
            "{verb} review rule comment `{key}` on pull request #{pull_request} (rule `{rule}`)"
        )
    }
}

/// One rule comment's decision, as `review plan` and `review run` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleCommentDecision {
    pub key: String,
    pub rule: String,
    /// `create`, `update`, `none`, `delete` or `suppressed`.
    pub decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// The rule comment writes for one pull request, and the decision behind each.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RuleCommentPlan {
    pub actions: Vec<RuleCommentAction>,
    pub decisions: Vec<RuleCommentDecision>,
}

/// Compare what the rules want with the comments already there.
///
/// `existing` is `None` when the pull request's comments could not be read, or
/// could not be attributed to the broker's own identity. Nothing is written
/// then: a create could duplicate a comment that is already there, and an edit
/// could land on somebody else's. `suppressed` names a reason the pull request
/// gets no rule comment at all.
pub fn plan_rule_comments(
    wants: &RuleCommentWants,
    existing: Option<&BTreeMap<String, OwnedComment>>,
    suppressed: Option<&str>,
) -> RuleCommentPlan {
    let mut plan = RuleCommentPlan::default();
    for unrendered in &wants.unrendered {
        plan.decisions.push(RuleCommentDecision {
            key: unrendered.key.clone(),
            rule: unrendered.rule.clone(),
            decision: "suppressed",
            why: Some("the change was not measured, so there is nothing to render".into()),
            body: None,
        });
    }
    let blocked = suppressed.map(str::to_string).or_else(|| {
        existing.is_none().then(|| {
            "the pull request's comments could not be read as the broker's own; a create could \
             duplicate one and an edit could land on somebody else's"
                .to_string()
        })
    });
    if let Some(why) = blocked {
        for planned in &wants.post {
            plan.decisions.push(RuleCommentDecision {
                key: planned.key.clone(),
                rule: planned.rule.clone(),
                decision: "suppressed",
                why: Some(why.clone()),
                body: Some(planned.body.clone()),
            });
        }
        for retired in &wants.retire {
            plan.decisions.push(RuleCommentDecision {
                key: retired.key.clone(),
                rule: retired.rule.clone(),
                decision: "suppressed",
                why: Some(why.clone()),
                body: None,
            });
        }
        return plan;
    }
    let existing = existing.expect("checked above");
    for planned in &wants.post {
        let (decision, action) = match existing.get(&planned.key) {
            Some(current) if current.body.trim() == planned.body.trim() => ("none", None),
            Some(current) => (
                "update",
                Some(RuleCommentAction::Update {
                    key: planned.key.clone(),
                    rule: planned.rule.clone(),
                    comment_id: current.id,
                    body: planned.body.clone(),
                }),
            ),
            None => (
                "create",
                Some(RuleCommentAction::Create {
                    key: planned.key.clone(),
                    rule: planned.rule.clone(),
                    body: planned.body.clone(),
                }),
            ),
        };
        plan.decisions.push(RuleCommentDecision {
            key: planned.key.clone(),
            rule: planned.rule.clone(),
            decision,
            why: None,
            body: Some(planned.body.clone()),
        });
        plan.actions.extend(action);
    }
    for retired in &wants.retire {
        let Some(current) = existing.get(&retired.key) else {
            continue;
        };
        plan.decisions.push(RuleCommentDecision {
            key: retired.key.clone(),
            rule: retired.rule.clone(),
            decision: "delete",
            why: Some("the rule no longer matches and says `on_unmatch = \"delete\"`".into()),
            body: None,
        });
        plan.actions.push(RuleCommentAction::Delete {
            key: retired.key.clone(),
            rule: retired.rule.clone(),
            comment_id: current.id,
        });
    }
    plan
}

/// The broker's own rule comments on a pull request, by key.
///
/// Takes `(id, author, body)`; only comments whose author is `viewer` -- the
/// identity `gh` acts as -- count. Anyone can paste a marker into a comment,
/// and the broker must never adopt, edit or delete one it did not write. The
/// first comment per key wins.
pub fn own_rule_comments<'a>(
    comments: impl IntoIterator<Item = (i64, &'a str, &'a str)>,
    viewer: &str,
) -> BTreeMap<String, OwnedComment> {
    let mut found = BTreeMap::new();
    for (id, author, body) in comments {
        if author != viewer {
            continue;
        }
        if let Some(key) = rule_comment_key(body) {
            found.entry(key.to_string()).or_insert(OwnedComment {
                id,
                body: body.to_string(),
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change() -> ChangeClassification {
        let policy = crate::ChangeClassificationPolicy {
            sensitive_paths: vec!["src/auth/**".into()],
            ..Default::default()
        };
        crate::classify_change(
            &policy,
            &crate::ChangeInputs {
                files: vec![crate::ChangedFile {
                    path: "src/auth/@everyone_[x](y).rs".into(),
                    old_path: None,
                    added: Some(900),
                    deleted: Some(20),
                }],
                files_complete: true,
                provenance_known: true,
                ..Default::default()
            },
        )
    }

    fn context() -> RuleCommentContext {
        RuleCommentContext {
            pull_request: 7,
            head: "0123456789abcdef0123".into(),
            base: "main".into(),
        }
    }

    #[test]
    fn templates_may_name_only_measured_variables() {
        assert_eq!(
            template_variables("{{tier}} and {{ churn }}").unwrap(),
            BTreeSet::from(["churn".to_string(), "tier".to_string()])
        );
        let error = template_variables("{{title}}").unwrap_err();
        assert!(error.contains("unknown variable `{{title}}`"), "{error}");
        assert!(template_variables("{{tier").is_err());
    }

    #[test]
    fn rendering_fills_variables_and_neutralises_repository_text() {
        let body = render_rule_comment(
            "size-heads-up",
            "**{{tier}}**: {{files_changed}} files, {{churn}} lines @codex review\n{{reasons}}",
            &change(),
            "big",
            &context(),
        );
        assert!(body.starts_with("<!-- aethyme:comment:size-heads-up -->\n"));
        assert!(body.contains("**large**: 1 files, 920 lines @codex review"));
        // `reasons` is rendered inert too.
        assert!(body.contains("pr\\_size"), "{body}");
    }

    #[test]
    fn head_renders_the_full_classified_head() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let context = RuleCommentContext {
            head: head.into(),
            ..context()
        };
        let body = render_rule_comment(
            "override",
            "/review-override {{head}} trivial ({{head_short}})",
            &change(),
            "trivial",
            &context,
        );
        assert!(
            body.contains(&format!("/review-override {head} trivial (0123456789ab)")),
            "{body}"
        );
        assert!(template_variables("{{head}}").is_ok());
    }

    #[test]
    fn values_cannot_mention_link_or_break_lines() {
        let escaped = escape_value("@everyone [click](https://evil)\n<!-- x -->");
        assert!(!escaped.contains("@everyone"), "{escaped}");
        assert!(!escaped.contains("[click](https://evil)"), "{escaped}");
        assert!(!escaped.contains('\n'), "{escaped}");
        assert!(!escaped.contains("<!--"), "{escaped}");
    }

    #[test]
    fn a_key_is_read_only_from_a_comment_that_starts_with_the_marker() {
        assert_eq!(
            rule_comment_key("<!-- aethyme:comment:size-heads-up -->\nbody"),
            Some("size-heads-up")
        );
        assert_eq!(
            rule_comment_key("> <!-- aethyme:comment:size-heads-up -->\nquoted"),
            None
        );
        assert_eq!(rule_comment_key("<!-- aethyme:comment:a --> x -->"), None);
        assert!(!valid_key("Upper"));
        assert!(!valid_key("a -->"));
    }

    #[test]
    fn only_the_brokers_own_marked_comments_are_adopted() {
        let ours = format!("{}\nmine", marker("k"));
        let theirs = format!("{}\nspoofed", marker("k"));
        let found = own_rule_comments(
            [
                (1, "intruder", theirs.as_str()),
                (2, "aethyme-bot", ours.as_str()),
            ],
            "aethyme-bot",
        );
        assert_eq!(found["k"].id, 2);
    }

    fn wants() -> RuleCommentWants {
        RuleCommentWants {
            post: vec![PlannedComment {
                key: "k".into(),
                rule: "r".into(),
                body: format!("{}\nhello", marker("k")),
            }],
            retire: Vec::new(),
            unrendered: Vec::new(),
        }
    }

    #[test]
    fn create_update_and_unchanged_are_decided_against_the_existing_comment() {
        let none = BTreeMap::new();
        let plan = plan_rule_comments(&wants(), Some(&none), None);
        assert!(matches!(
            plan.actions[..],
            [RuleCommentAction::Create { .. }]
        ));

        let same = BTreeMap::from([(
            "k".to_string(),
            OwnedComment {
                id: 9,
                body: format!("{}\nhello\n", marker("k")),
            },
        )]);
        let plan = plan_rule_comments(&wants(), Some(&same), None);
        assert!(plan.actions.is_empty());
        assert_eq!(plan.decisions[0].decision, "none");

        let stale = BTreeMap::from([(
            "k".to_string(),
            OwnedComment {
                id: 9,
                body: format!("{}\nold", marker("k")),
            },
        )]);
        let plan = plan_rule_comments(&wants(), Some(&stale), None);
        assert!(matches!(
            plan.actions[..],
            [RuleCommentAction::Update { comment_id: 9, .. }]
        ));
    }

    #[test]
    fn unreadable_comments_or_a_suppression_write_nothing() {
        let plan = plan_rule_comments(&wants(), None, None);
        assert!(plan.actions.is_empty());
        assert_eq!(plan.decisions[0].decision, "suppressed");
        let none = BTreeMap::new();
        let plan = plan_rule_comments(&wants(), Some(&none), Some("fork"));
        assert!(plan.actions.is_empty());
        assert_eq!(plan.decisions[0].why.as_deref(), Some("fork"));
    }

    #[test]
    fn the_writes_use_the_guard_allowlisted_forms() {
        let update = RuleCommentAction::Update {
            key: "k".into(),
            rule: "r".into(),
            comment_id: 9,
            body: "b".into(),
        };
        assert_eq!(
            update.gh_args("acme/product", 7),
            [
                "api",
                "-X",
                "PATCH",
                "repos/acme/product/issues/comments/9",
                "-f",
                "body=b"
            ]
        );
        assert!(matches!(
            crate::gh_ref_guard::assess(&update.gh_args("acme/product", 7), "acme/product"),
            crate::gh_ref_guard::Verdict::SafeWrite
        ));
        let delete = RuleCommentAction::Delete {
            key: "k".into(),
            rule: "r".into(),
            comment_id: 9,
        };
        assert!(matches!(
            crate::gh_ref_guard::assess(&delete.gh_args("acme/product", 7), "acme/product"),
            crate::gh_ref_guard::Verdict::SafeWrite
        ));
        let create = RuleCommentAction::Create {
            key: "k".into(),
            rule: "r".into(),
            body: "b".into(),
        };
        assert!(matches!(
            crate::gh_ref_guard::assess(&create.gh_args("acme/product", 7), "acme/product"),
            crate::gh_ref_guard::Verdict::NoRefWrite
        ));
    }
}
