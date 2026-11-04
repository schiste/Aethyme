//! The broker's command-line and operator layer.
//!
//! Everything here sits above `aethyme_broker`'s core: the `aethyme broker`
//! command tree, storage reporting, the local exchange, handoff and restore
//! surfaces, diagnostic reports and the services the CLI installs. It is a
//! separate crate so the core's unit-test binary, the slowest compile in the
//! workspace, does not build this code. See
//! `docs/guides/build-performance.md`.

// The modules here were written against one crate. Re-exporting the core at
// this crate's root keeps their `crate::...` paths and the suites' imports
// working, so a module can move between the crates without being rewritten.
#[doc(hidden)]
pub use aethyme_broker::*;

pub mod agent_hook;
mod artifact_claim;
mod chau7_mcp;
pub mod collaboration_cli;
#[allow(dead_code)]
pub mod experimental_symbol_mapping;
pub mod cli;
mod cli_output;
mod gate_doctor;
mod git_maintenance_lock;
mod graph_impact_envelope;
mod homebrew;
mod issue_form;
mod issue_set;
mod issue_set_apply;
mod issue_set_github;
pub mod local_exchange;
pub mod local_generation;
pub mod local_handoff;
pub mod local_restore;
mod merge_chain;
pub mod plugin_cli;
mod pr_monitoring;
pub mod session_guidance;
pub use pr_monitoring::{
    activate as activate_pr_monitoring, active_sessions as pr_monitoring_sessions,
    deactivate as deactivate_pr_monitoring, is_active as pr_monitoring_is_active,
};
#[cfg(any(target_os = "macos", all(test, unix)))]
mod gc_sweep_service;
#[cfg(unix)]
mod pr_watch_service;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod pr_watch_service_install;
mod report;
mod report_filing;
#[cfg(unix)]
mod scheduled_service;
mod watch_tick_budget;
pub use gate_doctor::{
    GateDiagnostic, GateDiagnosticConfidence, GateDiagnosticId, GateDiagnosticSeverity,
    GateDoctorError, GateDoctorGate, GateDoctorReport, GateProbeMutations, GateProbeOutcome,
    GateProbeReport, GateProbeWorktree, inspect_gate_quality, probe_gate_quality,
    static_gate_diagnostics,
};
pub use graph_impact_envelope::{UnbindableImpactReport, impact_analysis_envelope};
pub use homebrew::render_homebrew_formula;
pub use issue_form::{
    ISSUE_FORM_RENDER_SCHEMA_VERSION, ISSUE_REVIEW_ARTIFACT_SCHEMA_VERSION, IssueFormFieldKind,
    IssueFormFieldStatus, IssueFormRenderResult, IssueFormRenderedField, IssueFormWriteResult,
    render_issue_form, write_issue_form_render_atomic,
};
pub use report::{
    InvalidReportEntry, PreparedReport, REPORT_DOCUMENT_SCHEMA_VERSION, REPORT_FILINGS_FILENAME,
    REPORT_FILINGS_SCHEMA_VERSION, REPORT_INVENTORY_SCHEMA_VERSION, REPORT_MAX_BYTES,
    REPORT_RECENT_EVENT_LIMIT, REPORT_RECENT_GATE_LIMIT, REPORT_RECENT_OPERATION_LIMIT,
    REPORT_SNAPSHOT_SCHEMA_VERSION, ReportBuild, ReportCaptureError, ReportCaptureResult,
    ReportDocument, ReportEventType, ReportFilingState, ReportGateCacheSource,
    ReportGateObservation, ReportGateProvenance, ReportGraphIntegrity, ReportInspection,
    ReportKind, ReportLastFailure, ReportList, ReportOperation, ReportPlatform, ReportSession,
    ReportSnapshot, ReportSnapshotBuilder, ReportSummary, list_reports, prepare_report,
    show_report, write_report_atomic,
};
pub use report_filing::{
    REPORT_FILE_SCHEMA_VERSION, ReportFileError, ReportFileResult, ReportFileState,
    file_reviewed_report,
};
