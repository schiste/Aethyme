//! The baseline AQ0 adapter: present a [`GraphImpactReport`] through the
//! shared analysis envelope (#655, plan §5.7, §6.20) without claiming more
//! than the report establishes.
//!
//! The mapping is deliberately conservative:
//!
//! - **Subjects.** The report analyses a list of changed paths against one
//!   committed revision. The base is that revision (a legacy Git subject: the
//!   engine never computed raw-byte source identity), and the candidate is the
//!   changed-path set, not a candidate snapshot. A report whose revision is
//!   not a full object id cannot be bound and is refused.
//! - **Profile.** A named legacy profile, `aethyme-engine/<version>/graph-impact-<mode>`.
//!   Producer configuration is not pinned, so results are comparable only
//!   with the same engine version and mode.
//! - **Dimensions.** `complete` maps to exact, complete within profile and
//!   within limits. `partial` maps to exact but partial coverage, even when
//!   only traversal was cut short, because the report does not separate the
//!   two. Truncation is read from `limits.truncated` only: the report's
//!   `coverage.truncated` is also true for coverage gaps, so it is a gap, not
//!   a truncation. `stale` and `unavailable` leave coverage and limits
//!   unknown.
//! - **Confidence.** The report's `high`/`medium`/`low` label is kept as
//!   `heuristic_confidence` and never feeds coverage or freshness.
//! - **Locators.** The repository root path and free-text explanations are
//!   left out: they are locators and may expose local paths, not identity.

use aethyme_contracts::experimental_v0::analysis::{
    AnalysisEnvelope, Coverage, Freshness, Limits, Operation, Outcome, ProfileRef, Subject,
};
use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};

use crate::graph_impact::{GraphImpactContractStatus, GraphImpactReport};

/// Why a report cannot be presented through the envelope.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnbindableImpactReport {
    #[error("impact report revision {revision:?} is not a full Git object id")]
    Revision { revision: String },
    #[error("impact report diff digest {digest:?} is not 64 lowercase hex digits")]
    DiffDigest { digest: String },
}

/// Present `report` as an `explain_impact` analysis envelope.
pub fn impact_analysis_envelope(
    report: &GraphImpactReport,
) -> Result<AnalysisEnvelope, UnbindableImpactReport> {
    let revision = &report.repository.revision;
    if !is_lower_hex(revision, &[40, 64]) {
        return Err(UnbindableImpactReport::Revision {
            revision: revision.clone(),
        });
    }
    let digest = &report.request.diff_digest;
    if !is_lower_hex(digest, &[64]) {
        return Err(UnbindableImpactReport::DiffDigest {
            digest: digest.clone(),
        });
    }

    let truncated = if report.limits.truncated {
        Limits::Truncated
    } else {
        Limits::WithinLimits
    };
    let (outcome, freshness, coverage, limits, reason) = match report.status {
        GraphImpactContractStatus::Complete => (
            Outcome::Available,
            Freshness::Exact,
            Coverage::CompleteWithinProfile,
            truncated,
            None,
        ),
        GraphImpactContractStatus::Partial => (
            Outcome::Available,
            Freshness::Exact,
            Coverage::Partial,
            truncated,
            None,
        ),
        GraphImpactContractStatus::Stale => (
            Outcome::Available,
            Freshness::Stale,
            Coverage::Unknown,
            Limits::Unknown,
            Some("graph_not_bound_to_revision"),
        ),
        GraphImpactContractStatus::Unavailable => (
            Outcome::Unavailable,
            Freshness::Unknown,
            Coverage::Unknown,
            Limits::Unknown,
            Some("graph_unavailable"),
        ),
    };

    let mut gaps: Vec<String> = report
        .coverage
        .missing_edge_kinds
        .iter()
        .map(|kind| format!("missing_edge_kind:{kind}"))
        .collect();
    if report.coverage.truncated && !report.limits.truncated {
        gaps.push("coverage_gap".into());
    }
    if report.limits.truncated {
        gaps.push("traversal_limit".into());
    }
    gaps.sort();
    gaps.dedup();

    // The report withholds impact paths when they cannot be trusted; carry
    // a result only where it exposed them.
    let result = (outcome == Outcome::Available && freshness == Freshness::Exact).then(|| {
        let set = &report.impact;
        object([
            ("direct", strings(&set.direct)),
            ("transitive", strings(&set.transitive)),
            ("callers", strings(&set.callers)),
            ("importers", strings(&set.importers)),
            ("tests", strings(&set.tests)),
            ("configs", strings(&set.configs)),
            ("manifests", strings(&set.manifests)),
        ])
    });

    let provenance = &report.provenance;
    let mut provenance_members = vec![
        ("engine_version", text(&provenance.engine_version)),
        ("request_digest", text(&provenance.request_digest)),
        ("result_digest", text(&provenance.result_digest)),
    ];
    if let Some(graph_revision) = &provenance.graph_revision {
        provenance_members.push(("graph_revision", text(graph_revision)));
    }
    let limits_detail = &report.limits;

    Ok(AnalysisEnvelope {
        operation: Operation::ExplainImpact,
        subject: Subject::ChangedPaths(digest.clone()),
        base_subject: Some(Subject::LegacyGitRevision(revision.clone())),
        profile: ProfileRef::Legacy(format!(
            "aethyme-engine/{}/graph-impact-{}",
            provenance.engine_version,
            report.request.mode.as_str()
        )),
        outcome,
        freshness,
        coverage,
        limits,
        reason: reason.map(str::to_string),
        gaps,
        heuristic_confidence: Some(report.confidence.as_str().to_string()),
        provenance: Some(object(provenance_members)),
        limit_detail: Some(object([
            ("budget", integer(limits_detail.budget)),
            ("max_depth", integer(limits_detail.max_depth)),
            ("max_nodes", integer(limits_detail.max_nodes)),
            ("max_results", integer(limits_detail.max_results)),
        ])),
        result,
    })
}

fn is_lower_hex(text: &str, lengths: &[usize]) -> bool {
    lengths.contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn strings(values: &[String]) -> Value {
    Value::Array(values.iter().map(|value| text(value)).collect())
}

fn integer(value: usize) -> Value {
    // Budgets are bounded by GRAPH_IMPACT_MAX_BUDGET, far inside the
    // profile's safe integer range.
    Value::Integer(i64::try_from(value).expect("impact limits are small"))
}

fn object(members: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let members = members
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    Value::Object(Object::new(members).expect("distinct keys"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_impact::{
        GraphImpactConfidence, GraphImpactCoverage, GraphImpactLimits, GraphImpactMode,
        GraphImpactProvenance, GraphImpactRepository, GraphImpactRequestSummary,
        GraphImpactRiskHints, GraphImpactSet,
    };
    use aethyme_contracts::experimental_v0::analysis::Status;

    const REVISION: &str = "5e8daf712e26c63f1d4082e9a617f556cf995de2";

    fn report(status: GraphImpactContractStatus) -> GraphImpactReport {
        GraphImpactReport {
            schema_version: 1,
            repository: GraphImpactRepository {
                root: "/Users/someone/private/checkout".into(),
                revision: REVISION.into(),
            },
            request: GraphImpactRequestSummary {
                changed_files: 1,
                diff_digest: "c3".repeat(32),
                mode: GraphImpactMode::Calls,
            },
            status,
            confidence: GraphImpactConfidence::High,
            coverage: GraphImpactCoverage {
                mode: "complete".into(),
                languages: vec!["rust".into()],
                parsed_files: 1,
                excluded_files: 0,
                unsupported_files: 0,
                missing_edge_kinds: Vec::new(),
                truncated: false,
            },
            impact: GraphImpactSet::default(),
            risk_hints: GraphImpactRiskHints {
                security_surface: false,
                runtime_surface: false,
                workspace_surface: false,
                global_config_surface: false,
            },
            limits: GraphImpactLimits {
                budget: 8,
                max_nodes: 8,
                max_depth: 4,
                max_results: 8,
                truncated: false,
            },
            provenance: GraphImpactProvenance {
                graph_revision: Some(REVISION.into()),
                engine_version: "0.8.26".into(),
                request_digest: "r".into(),
                result_digest: "s".into(),
            },
            explanations: vec!["evaluated /Users/someone/private/checkout".into()],
        }
    }

    fn envelope(report: &GraphImpactReport) -> AnalysisEnvelope {
        let envelope = impact_analysis_envelope(report).unwrap();
        // Every envelope must survive the shared record encoding unchanged.
        let (bytes, _) = envelope.to_record();
        assert_eq!(AnalysisEnvelope::from_record(&bytes).unwrap(), envelope);
        envelope
    }

    #[test]
    fn only_a_complete_report_makes_an_empty_result_evidence_of_no_impact() {
        let complete = envelope(&report(GraphImpactContractStatus::Complete));
        assert_eq!(complete.status(), Status::Complete);
        assert!(complete.absence_is_evidence());

        for status in [
            GraphImpactContractStatus::Partial,
            GraphImpactContractStatus::Stale,
            GraphImpactContractStatus::Unavailable,
        ] {
            let degraded = envelope(&report(status));
            assert!(!degraded.absence_is_evidence(), "{status:?}");
        }
    }

    #[test]
    fn each_legacy_status_maps_to_its_own_envelope_status() {
        let mut truncated = report(GraphImpactContractStatus::Partial);
        truncated.limits.truncated = true;
        truncated.coverage.truncated = true;
        let cases = [
            (report(GraphImpactContractStatus::Partial), Status::Partial),
            (truncated, Status::Truncated),
            (report(GraphImpactContractStatus::Stale), Status::Stale),
            (
                report(GraphImpactContractStatus::Unavailable),
                Status::Unavailable,
            ),
        ];
        for (report, expected) in cases {
            assert_eq!(envelope(&report).status(), expected, "{:?}", report.status);
        }
    }

    /// The report sets `coverage.truncated` for coverage gaps too; that is a
    /// gap, not a cut-short traversal.
    #[test]
    fn a_coverage_gap_is_not_read_as_truncation() {
        let mut gapped = report(GraphImpactContractStatus::Partial);
        gapped.coverage.truncated = true;
        gapped.coverage.missing_edge_kinds = vec!["Calls".into()];
        let envelope = envelope(&gapped);
        assert_eq!(envelope.limits, Limits::WithinLimits);
        assert_eq!(envelope.coverage, Coverage::Partial);
        assert_eq!(envelope.gaps, ["coverage_gap", "missing_edge_kind:Calls"]);
    }

    #[test]
    fn high_confidence_never_upgrades_a_partial_report() {
        let mut partial = report(GraphImpactContractStatus::Partial);
        partial.confidence = GraphImpactConfidence::High;
        let envelope = envelope(&partial);
        assert_eq!(envelope.heuristic_confidence.as_deref(), Some("high"));
        assert_eq!(envelope.status(), Status::Partial);
    }

    #[test]
    fn subjects_are_the_revision_and_the_changed_paths_never_a_snapshot() {
        let envelope = envelope(&report(GraphImpactContractStatus::Complete));
        assert_eq!(
            envelope.base_subject,
            Some(Subject::LegacyGitRevision(REVISION.into()))
        );
        assert_eq!(envelope.subject, Subject::ChangedPaths("c3".repeat(32)));
        assert_eq!(
            envelope.profile,
            ProfileRef::Legacy("aethyme-engine/0.8.26/graph-impact-calls".into())
        );
    }

    #[test]
    fn stale_and_unavailable_reports_carry_no_result() {
        for status in [
            GraphImpactContractStatus::Stale,
            GraphImpactContractStatus::Unavailable,
        ] {
            assert!(envelope(&report(status)).result.is_none(), "{status:?}");
        }
    }

    #[test]
    fn local_paths_and_explanations_stay_out_of_the_envelope() {
        let (bytes, _) = envelope(&report(GraphImpactContractStatus::Complete)).to_record();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("/Users/someone"), "{text}");
    }

    #[test]
    fn short_revisions_and_unbindable_digests_are_refused() {
        let mut short = report(GraphImpactContractStatus::Complete);
        short.repository.revision = "abc".into();
        assert!(matches!(
            impact_analysis_envelope(&short),
            Err(UnbindableImpactReport::Revision { .. })
        ));
        let mut digest = report(GraphImpactContractStatus::Complete);
        digest.request.diff_digest = "XYZ".into();
        assert!(matches!(
            impact_analysis_envelope(&digest),
            Err(UnbindableImpactReport::DiffDigest { .. })
        ));
    }
}
