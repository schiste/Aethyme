//! Declared session scope and the collisions it exposes.
//!
//! Leases are recomputed from each session's diff, so they describe work that
//! has already happened: by the time two sessions overlap on a path, both have
//! edited it. A scope is declared before the edits exist, and names the target
//! rather than the file. That catches the case a path comparison cannot see --
//! two sessions reshaping one interface from different files, textually
//! disjoint until both land.
//!
//! Detection is a pure function of the declared pairs. Nothing here consults a
//! model, so the same two scopes always produce the same verdict.

use std::collections::HashSet;

use crate::types::{ScopeKind, ScopeOperation, SessionScope};

/// How much two sessions sharing a target should worry the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeConflictSeverity {
    /// Both sides only add; the target survives in a form each expects.
    Low,
    /// At least one side did not state its intent, so the pair cannot be
    /// judged. Reported rather than assumed harmless.
    Medium,
    /// One side rewrites or removes what the other is building on.
    High,
}

impl ScopeConflictSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Two live sessions naming the same target.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScopeOverlap {
    /// Lower session id first — the pair is unordered.
    pub session_a: i64,
    pub session_b: i64,
    pub kind: ScopeKind,
    pub value: String,
    pub operation_a: ScopeOperation,
    pub operation_b: ScopeOperation,
    pub severity: ScopeConflictSeverity,
    /// Why this pair was classified as it was, in the operator's terms.
    pub explanation: String,
    /// The cheapest next move, stated so the pair is actionable rather than
    /// merely reported.
    pub suggestion: String,
}

/// Severity for one ordered pair of intents.
///
/// The pair decides, not the target: two sessions extending one interface is
/// ordinary work, while a rewrite underneath an extension silently invalidates
/// it. Refusing every shared target would make the signal useless.
pub fn classify(a: ScopeOperation, b: ScopeOperation) -> ScopeConflictSeverity {
    use ScopeOperation::*;
    match (a, b) {
        (Remove, _) | (_, Remove) => ScopeConflictSeverity::High,
        (Replace, Replace) => ScopeConflictSeverity::High,
        (Replace, Extend) | (Extend, Replace) => ScopeConflictSeverity::High,
        (Unknown, _) | (_, Unknown) => ScopeConflictSeverity::Medium,
        (Extend, Extend) => ScopeConflictSeverity::Low,
    }
}

fn explain(value: &str, a: ScopeOperation, b: ScopeOperation) -> (String, String) {
    use ScopeOperation::*;
    match (a, b) {
        (Remove, _) | (_, Remove) => (
            format!("one session removes `{value}` while the other still works on it"),
            "settle whether the target survives before either lands; a removal \
             invalidates the other session's work rather than conflicting with it"
                .to_string(),
        ),
        (Replace, Replace) => (
            format!("both sessions rewrite `{value}`"),
            "agree one owner for the rewrite; the second will be reworked against \
             whatever the first lands"
                .to_string(),
        ),
        (Replace, Extend) | (Extend, Replace) => (
            format!("one session rewrites `{value}` while the other extends it"),
            "land the rewrite first, or extract the shared shape both sides can \
             build on; the extension will otherwise attach to a form that is \
             going away"
                .to_string(),
        ),
        (Unknown, _) | (_, Unknown) => (
            format!("both sessions name `{value}`, and at least one did not state its intent"),
            "state the intent on both sides (extend, replace or remove) so the \
             pair can be judged"
                .to_string(),
        ),
        (Extend, Extend) => (
            format!("both sessions extend `{value}`"),
            "usually fine; check the additions do not assume each other's absence".to_string(),
        ),
    }
}

/// Every pair of live sessions naming one target, worst first.
///
/// Scopes already released, or belonging to one session, are not pairs.
pub fn detect_scope_overlaps(scopes: &[SessionScope]) -> Vec<ScopeOverlap> {
    let live: Vec<&SessionScope> = scopes
        .iter()
        .filter(|scope| scope.released_at.is_none())
        .collect();
    let mut seen = HashSet::new();
    let mut overlaps = Vec::new();
    for (index, a) in live.iter().enumerate() {
        for b in &live[index + 1..] {
            if a.session_id == b.session_id || a.kind != b.kind || a.value != b.value {
                continue;
            }
            // Order the pair by session id so the same collision is reported
            // identically however the rows were read.
            let (low, high, op_low, op_high) = if a.session_id < b.session_id {
                (a.session_id, b.session_id, a.operation, b.operation)
            } else {
                (b.session_id, a.session_id, b.operation, a.operation)
            };
            if !seen.insert((low, high, a.kind.as_str(), a.value.clone())) {
                continue;
            }
            let severity = classify(op_low, op_high);
            let (explanation, suggestion) = explain(&a.value, op_low, op_high);
            overlaps.push(ScopeOverlap {
                session_a: low,
                session_b: high,
                kind: a.kind,
                value: a.value.clone(),
                operation_a: op_low,
                operation_b: op_high,
                severity,
                explanation,
                suggestion,
            });
        }
    }
    // Worst first, then stable by target and pair.
    overlaps.sort_by(|x, y| {
        y.severity
            .cmp(&x.severity)
            .then_with(|| x.value.cmp(&y.value))
            .then_with(|| x.session_a.cmp(&y.session_a))
            .then_with(|| x.session_b.cmp(&y.session_b))
    });
    overlaps
}

/// Parse `kind:value[=operation]`, e.g. `symbol:PaymentService=replace`.
///
/// The operation is optional so a caller can name a target without claiming to
/// know yet what it will do to it; that records `unknown` and reports the pair
/// at medium rather than pretending it is safe.
pub fn parse_scope_argument(text: &str) -> Result<(ScopeKind, String, ScopeOperation), String> {
    let (kind_text, rest) = text.split_once(':').ok_or_else(|| {
        format!("scope `{text}` must be written kind:value[=operation], e.g. symbol:Name=replace")
    })?;
    let kind = ScopeKind::parse(kind_text)
        .map_err(|_| format!("unknown scope kind `{kind_text}`; supported kinds: symbol"))?;
    let (value, operation) = match rest.split_once('=') {
        Some((value, op_text)) => {
            let operation = ScopeOperation::parse(op_text).map_err(|_| {
                format!(
                    "unknown scope operation `{op_text}`; supported operations: \
                     extend, replace, remove"
                )
            })?;
            (value, operation)
        }
        None => (rest, ScopeOperation::Unknown),
    };
    if value.trim().is_empty() {
        return Err(format!("scope `{text}` names no target"));
    }
    Ok((kind, value.trim().to_string(), operation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ScopeSource;

    fn scope(session: i64, value: &str, operation: ScopeOperation) -> SessionScope {
        SessionScope {
            id: session * 100,
            session_id: session,
            kind: ScopeKind::Symbol,
            value: value.to_string(),
            operation,
            source: ScopeSource::Declared,
            created_at: 1,
            released_at: None,
        }
    }

    /// The pair decides, not the target. Flagging every shared symbol would
    /// make the signal worthless, and treating an unstated intent as safe would
    /// assert a verdict the evidence does not support.
    #[test]
    fn severity_is_decided_by_the_pair_of_intents() {
        use ScopeOperation::*;
        assert_eq!(classify(Extend, Extend), ScopeConflictSeverity::Low);
        assert_eq!(classify(Replace, Extend), ScopeConflictSeverity::High);
        assert_eq!(classify(Extend, Replace), ScopeConflictSeverity::High);
        assert_eq!(classify(Replace, Replace), ScopeConflictSeverity::High);
        assert_eq!(classify(Remove, Extend), ScopeConflictSeverity::High);
        assert_eq!(classify(Extend, Remove), ScopeConflictSeverity::High);
        assert_eq!(classify(Unknown, Extend), ScopeConflictSeverity::Medium);
        assert_eq!(classify(Unknown, Unknown), ScopeConflictSeverity::Medium);
        // A removal outranks an unstated intent: the target going away is
        // decidable whatever the other side meant to do.
        assert_eq!(classify(Remove, Unknown), ScopeConflictSeverity::High);
    }

    #[test]
    fn a_rewrite_under_an_extension_is_reported_with_a_way_out() {
        let overlaps = detect_scope_overlaps(&[
            scope(7, "PaymentService", ScopeOperation::Replace),
            scope(4, "PaymentService", ScopeOperation::Extend),
        ]);
        assert_eq!(overlaps.len(), 1);
        let found = &overlaps[0];
        // The pair is unordered, so the lower session is always first.
        assert_eq!((found.session_a, found.session_b), (4, 7));
        assert_eq!(found.operation_a, ScopeOperation::Extend);
        assert_eq!(found.operation_b, ScopeOperation::Replace);
        assert_eq!(found.severity, ScopeConflictSeverity::High);
        assert!(found.explanation.contains("PaymentService"), "{found:?}");
        assert!(
            !found.suggestion.is_empty(),
            "a reported collision must say what to do next: {found:?}"
        );
    }

    #[test]
    fn one_session_and_released_scopes_are_not_collisions() {
        // Two targets in one session is ordinary work, not a pair.
        let same_session = detect_scope_overlaps(&[
            scope(1, "Alpha", ScopeOperation::Replace),
            scope(1, "Alpha", ScopeOperation::Extend),
        ]);
        assert!(same_session.is_empty(), "{same_session:?}");

        // A released scope is history, not a claim.
        let mut released = scope(2, "Alpha", ScopeOperation::Replace);
        released.released_at = Some(9);
        let dropped = detect_scope_overlaps(&[scope(1, "Alpha", ScopeOperation::Extend), released]);
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn different_targets_never_pair_and_worst_is_reported_first() {
        let overlaps = detect_scope_overlaps(&[
            scope(1, "Quiet", ScopeOperation::Extend),
            scope(2, "Quiet", ScopeOperation::Extend),
            scope(1, "Loud", ScopeOperation::Replace),
            scope(2, "Loud", ScopeOperation::Remove),
            scope(3, "Alone", ScopeOperation::Replace),
        ]);
        assert_eq!(overlaps.len(), 2, "{overlaps:?}");
        assert_eq!(overlaps[0].value, "Loud");
        assert_eq!(overlaps[0].severity, ScopeConflictSeverity::High);
        assert_eq!(overlaps[1].value, "Quiet");
        assert_eq!(overlaps[1].severity, ScopeConflictSeverity::Low);
        assert!(
            overlaps.iter().all(|found| found.value != "Alone"),
            "a target only one session names is not a collision: {overlaps:?}"
        );
    }

    #[test]
    fn a_target_may_be_named_without_claiming_to_know_the_intent() {
        let (kind, value, operation) = parse_scope_argument("symbol:PaymentService").unwrap();
        assert_eq!(kind, ScopeKind::Symbol);
        assert_eq!(value, "PaymentService");
        assert_eq!(
            operation,
            ScopeOperation::Unknown,
            "an unstated intent must record as unknown rather than a guess"
        );

        let (_, _, stated) = parse_scope_argument("symbol:PaymentService=replace").unwrap();
        assert_eq!(stated, ScopeOperation::Replace);
    }

    #[test]
    fn a_malformed_scope_says_what_was_expected() {
        for (input, expected) in [
            ("PaymentService", "kind:value"),
            ("symbol:", "names no target"),
            ("ledger:Thing", "unknown scope kind"),
            ("symbol:Thing=rewrite", "unknown scope operation"),
        ] {
            let error = parse_scope_argument(input).unwrap_err();
            assert!(
                error.contains(expected),
                "`{input}` should explain `{expected}`, said: {error}"
            );
        }
    }
}
