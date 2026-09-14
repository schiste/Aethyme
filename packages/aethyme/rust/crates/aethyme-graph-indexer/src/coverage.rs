//! Build the content-free coverage and code-unit projection while indexing.
//!
//! This module consumes the same walked files, parser results, fragments, and
//! source bytes as the normal index pass.  It never reparses a file and never
//! stores source text.  The resulting rows are suitable for a bounded,
//! revision-pinned downstream semantic pass.

use std::collections::BTreeMap;

use aethyme_graph_schema::{Node, NodeKind, NodeKindCategory, SourceRange};
use aethyme_graph_storage::{
    CoverageBytes, CoverageFileStatus, CoverageFiles, ExclusionReason, GraphCoverage, GraphUnit,
    UnitCoverageStatus, UnitPosition, sort_units,
};

use crate::context::IndexerContext;
use crate::filesystem::{FilesystemIndexResult, IndexedFile, SkipReason};
use crate::pipeline::BuiltFragment;

#[derive(Debug, Clone)]
pub(crate) struct FileCoverageObservation {
    pub path: String,
    pub language: String,
    pub parser: Option<String>,
    pub status: CoverageFileStatus,
    pub byte_size: u64,
    pub source_bytes_read: u64,
    pub exclusion_reason: Option<ExclusionReason>,
    pub unlocated_units: u64,
    pub units: Vec<GraphUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCoverage {
    pub report: GraphCoverage,
    pub units: Vec<GraphUnit>,
}

pub(crate) fn observe_file(
    indexed: &IndexedFile,
    built: &BuiltFragment,
    content: Option<&str>,
    parser: Option<&str>,
    status: CoverageFileStatus,
    exclusion_reason: Option<ExclusionReason>,
) -> FileCoverageObservation {
    let byte_size = match &indexed.top_node {
        Node::File(file) => file.byte_size(),
        Node::NonCodeFile(_) => 0,
        _ => 0,
    };
    let mut units = Vec::new();
    let mut unlocated_units = 0;
    if let Some(content) = content
        && let Some(unit_status) = unit_status(status)
        && matches!(&indexed.top_node, Node::File(_))
    {
        units.push(GraphUnit {
            path: indexed.source_path.to_string(),
            symbol_identity: None,
            kind: NodeKind::File.name().into(),
            language: indexed.language.to_string(),
            parser: parser.map(str::to_owned),
            start: UnitPosition {
                line: 1,
                byte_offset: 0,
            },
            end: UnitPosition {
                line: line_count(content),
                byte_offset: content.len() as u64,
            },
            content_digest: digest(&content.as_bytes()[..]),
            graph_node_ref: indexed.top_node.id().as_str().to_string(),
            coverage_status: unit_status,
        });
    }

    for node in built
        .fragment
        .nodes()
        .iter()
        .filter(|node| is_code_unit_kind(node.kind()))
    {
        let Some(content) = content else {
            continue;
        };
        let Some(range) = node.source_range() else {
            unlocated_units += 1;
            continue;
        };
        let Some((start, end)) = range_offsets(content, range) else {
            unlocated_units += 1;
            continue;
        };
        let Some(unit_status) = unit_status(status) else {
            continue;
        };
        let symbol_identity = node
            .name()
            .map(str::to_owned)
            .or_else(|| Some(format!("{}@{}", node.kind().name(), start,)));
        units.push(GraphUnit {
            path: indexed.source_path.to_string(),
            symbol_identity,
            kind: node.kind().name().into(),
            language: indexed.language.to_string(),
            parser: parser.map(str::to_owned),
            start: UnitPosition {
                line: range.start_line(),
                byte_offset: start as u64,
            },
            end: UnitPosition {
                line: range.end_line(),
                byte_offset: end as u64,
            },
            content_digest: digest(&content.as_bytes()[start..end]),
            graph_node_ref: node.id().as_str().to_string(),
            coverage_status: unit_status,
        });
    }

    sort_units(&mut units);
    FileCoverageObservation {
        path: indexed.source_path.to_string(),
        language: indexed.language.to_string(),
        parser: parser.map(str::to_owned),
        status,
        byte_size,
        source_bytes_read: content.map_or(0, |content| content.len()) as u64,
        exclusion_reason,
        unlocated_units,
        units,
    }
}

pub(crate) fn assemble(
    ctx: &IndexerContext,
    walk: &FilesystemIndexResult,
    observations: &[FileCoverageObservation],
    built_fragments: &[BuiltFragment],
) -> IndexCoverage {
    let mut report = GraphCoverage {
        schema_version: aethyme_graph_storage::GRAPH_COVERAGE_SCHEMA_VERSION,
        available: true,
        source_revision: ctx.source_revision().map(str::to_owned),
        indexed_revision: ctx.source_revision().map(str::to_owned),
        source_tree_sha256: ctx.source_tree_digest().map(str::to_owned),
        indexed_tree_sha256: ctx.source_tree_digest().map(str::to_owned),
        engine_version: ctx.engine_version().to_string(),
        coverage_mode: "partial".into(),
        safe_to_use: false,
        gaps: Vec::new(),
        files: CoverageFiles {
            discovered: (walk.files.len() + walk.skipped.len()) as u64,
            ..CoverageFiles::default()
        },
        bytes: CoverageBytes::default(),
        by_language: BTreeMap::new(),
        by_parser: BTreeMap::new(),
        exclusion_reasons: BTreeMap::new(),
        node_counts_by_kind: BTreeMap::new(),
        node_counts_by_category: BTreeMap::new(),
        edge_counts_by_kind: BTreeMap::new(),
        edge_counts_by_category: BTreeMap::new(),
        unit_count: 0,
    };
    let mut units = Vec::new();
    let mut unlocated_units = 0;

    for observation in observations {
        observe_file_counts(&mut report, observation, ctx.repo_root());
        unlocated_units += observation.unlocated_units;
        units.extend(observation.units.iter().cloned());
    }

    for skipped in &walk.skipped {
        let bytes = std::fs::metadata(ctx.repo_root().join(&*skipped.source_path))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        report.files.excluded += 1;
        report.bytes.excluded += bytes;
        report.bytes.discovered += bytes;
        let reason = skip_reason(skipped.reason);
        *report.exclusion_reasons.entry(reason).or_default() += 1;
    }

    for fragment in built_fragments {
        for node in fragment.fragment.nodes() {
            increment(&mut report.node_counts_by_kind, node.kind().name());
            increment(
                &mut report.node_counts_by_category,
                node.kind().category().name(),
            );
        }
        for edge in fragment.fragment.edges() {
            increment(&mut report.edge_counts_by_kind, edge.kind().name());
            increment(
                &mut report.edge_counts_by_category,
                edge.kind().category().name(),
            );
        }
    }

    sort_units(&mut units);
    report.unit_count = units.len() as u64;
    if ctx.source_revision().is_none() {
        report.gaps.push("source_revision_unbound".into());
    }
    if ctx.source_tree_digest().is_none() {
        report.gaps.push("source_tree_unbound".into());
    }
    if report.files.excluded > 0 {
        report.gaps.push("excluded_files".into());
    }
    if report.files.unsupported > 0 {
        report.gaps.push("unsupported_languages".into());
    }
    if report.files.partial > 0 {
        report.gaps.push("partial_parses".into());
    }
    if unlocated_units > 0 {
        report.gaps.push("unlocated_units".into());
        *report
            .exclusion_reasons
            .entry(ExclusionReason::SourceRangeUnavailable)
            .or_default() += unlocated_units;
    }
    report.coverage_mode = if report.gaps.is_empty() {
        "complete".into()
    } else {
        "partial".into()
    };
    report.safe_to_use = report.available && report.gaps.is_empty();
    IndexCoverage { report, units }
}

fn observe_file_counts(
    report: &mut GraphCoverage,
    observation: &FileCoverageObservation,
    repo_root: &std::path::Path,
) {
    let byte_size = if observation.status == CoverageFileStatus::NonCode {
        std::fs::metadata(repo_root.join(&observation.path))
            .map(|metadata| metadata.len())
            .unwrap_or(observation.byte_size)
    } else {
        observation.byte_size
    };
    report.files.eligible += 1;
    report.bytes.discovered += byte_size;
    report.bytes.eligible += byte_size;
    report.bytes.source_read += observation.source_bytes_read;
    match observation.status {
        CoverageFileStatus::Parsed => {
            report.files.parsed += 1;
            report.bytes.parsed += byte_size;
        }
        CoverageFileStatus::Partial => {
            report.files.partial += 1;
            report.bytes.partial += byte_size;
        }
        CoverageFileStatus::Unsupported => {
            report.files.unsupported += 1;
            report.bytes.unsupported += byte_size;
        }
        CoverageFileStatus::NonCode => {
            report.files.non_code += 1;
            report.bytes.non_code += byte_size;
        }
    }
    let language = report
        .by_language
        .entry(observation.language.clone())
        .or_default();
    language.observe(observation.status, byte_size);
    let parser_key = match observation.status {
        CoverageFileStatus::NonCode => "none".to_string(),
        CoverageFileStatus::Unsupported => "unavailable".to_string(),
        CoverageFileStatus::Parsed | CoverageFileStatus::Partial => observation
            .parser
            .clone()
            .unwrap_or_else(|| "unavailable".into()),
    };
    report
        .by_parser
        .entry(parser_key)
        .or_default()
        .observe(observation.status, byte_size);
    if let Some(reason) = observation.exclusion_reason {
        *report.exclusion_reasons.entry(reason).or_default() += 1;
    }
}

fn increment(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

fn unit_status(status: CoverageFileStatus) -> Option<UnitCoverageStatus> {
    match status {
        CoverageFileStatus::Parsed => Some(UnitCoverageStatus::Parsed),
        CoverageFileStatus::Partial => Some(UnitCoverageStatus::Partial),
        CoverageFileStatus::Unsupported => Some(UnitCoverageStatus::Unsupported),
        CoverageFileStatus::NonCode => None,
    }
}

fn is_code_unit_kind(kind: NodeKind) -> bool {
    matches!(
        kind.category(),
        NodeKindCategory::Callable
            | NodeKindCategory::TypeDefining
            | NodeKindCategory::SubSymbol
            | NodeKindCategory::SurfaceFlowNode
    )
}

fn range_offsets(content: &str, range: SourceRange) -> Option<(usize, usize)> {
    let mut line_starts = vec![0usize];
    for (offset, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(offset + 1);
        }
    }
    let start_index = range.start_line().checked_sub(1)? as usize;
    let end_index = range.end_line().checked_sub(1)? as usize;
    let start = *line_starts.get(start_index)?;
    let end = if let Some(next_line) = line_starts.get(end_index + 1) {
        *next_line
    } else {
        content.len()
    };
    (start <= end && end <= content.len()).then_some((start, end))
}

fn line_count(content: &str) -> u32 {
    content.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn skip_reason(reason: SkipReason) -> ExclusionReason {
    match reason {
        SkipReason::UnrecognizedExtension => ExclusionReason::UnrecognizedExtension,
        SkipReason::OverSizeLimit => ExclusionReason::OverSizeLimit,
        SkipReason::ReadError => ExclusionReason::ReadError,
    }
}
