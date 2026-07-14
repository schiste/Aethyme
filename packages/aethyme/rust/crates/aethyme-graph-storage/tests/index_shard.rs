//! Integration tests for per-module NDJSON index shards.

use aethyme_graph_schema::{NodeId, NodeKind};
use aethyme_graph_storage::{
    IndexShardDecodeError, SymbolRecord, dedupe_and_canonicalize, read_index_shard_bytes,
    write_index_shard_bytes,
};

fn record(module: &str, symbol: &str, kind: NodeKind, file: &str) -> SymbolRecord {
    let id = NodeId::new(kind, "aethyme", file, symbol).unwrap();
    SymbolRecord {
        module: module.into(),
        symbol: symbol.into(),
        kind,
        node_id: id,
        file: file.into(),
    }
}

#[test]
fn write_then_read_round_trips() {
    let records = vec![
        record(
            "src.cli",
            "explore_command",
            NodeKind::Function,
            "src/cli.py",
        ),
        record("src.cli", "Helper", NodeKind::Class, "src/cli.py"),
        record("src.cli", "run", NodeKind::Function, "src/cli.py"),
    ];
    let bytes = write_index_shard_bytes(&records).unwrap();
    let back = read_index_shard_bytes(&bytes).unwrap();
    // Read order should match the canonical sort order, not the
    // input order.
    assert_eq!(back.len(), records.len());
    let mut expected = records.clone();
    expected.sort_by(|a, b| {
        a.module
            .cmp(&b.module)
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.kind.name().cmp(b.kind.name()))
    });
    assert_eq!(back, expected);
}

#[test]
fn write_sorts_records_canonically() {
    // Two different input orders should produce identical bytes.
    let r1 = record("src.cli", "alpha", NodeKind::Function, "src/cli.py");
    let r2 = record("src.cli", "beta", NodeKind::Function, "src/cli.py");
    let r3 = record("src.util", "alpha", NodeKind::Function, "src/util.py");

    let bytes_a = write_index_shard_bytes(&[r1.clone(), r2.clone(), r3.clone()]).unwrap();
    let bytes_b = write_index_shard_bytes(&[r3.clone(), r1.clone(), r2.clone()]).unwrap();
    let bytes_c = write_index_shard_bytes(&[r2.clone(), r3.clone(), r1.clone()]).unwrap();
    assert_eq!(bytes_a, bytes_b);
    assert_eq!(bytes_a, bytes_c);
}

#[test]
fn ndjson_format_one_record_per_line_trailing_newline() {
    let records = vec![
        record("src.cli", "alpha", NodeKind::Function, "src/cli.py"),
        record("src.cli", "beta", NodeKind::Function, "src/cli.py"),
    ];
    let bytes = write_index_shard_bytes(&records).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let lines: Vec<&str> = text.split('\n').collect();
    // Trailing newline produces a final empty element after split.
    assert_eq!(lines.len(), 3); // 2 records + 1 empty after trailing \n
    assert_eq!(lines[2], "");
    // Each non-empty line is a complete JSON object.
    for line in &lines[..2] {
        assert!(line.starts_with('{'));
        assert!(line.ends_with('}'));
        let parsed: SymbolRecord = serde_json::from_str(line).unwrap();
        assert!(records.contains(&parsed));
    }
}

#[test]
fn read_tolerates_blank_lines() {
    // git's merge=union may produce blank lines if one side ended
    // without a trailing newline. Reader must tolerate them
    // gracefully.
    let r = record("src.cli", "alpha", NodeKind::Function, "src/cli.py");
    let line = serde_json::to_string(&r).unwrap();
    let messy = format!("{line}\n\n\n{line}\n\n");
    let parsed = read_index_shard_bytes(messy.as_bytes()).unwrap();
    assert_eq!(parsed.len(), 2);
}

#[test]
fn read_reports_line_number_on_malformed_json() {
    let r = record("src.cli", "alpha", NodeKind::Function, "src/cli.py");
    let good = serde_json::to_string(&r).unwrap();
    let messy = format!("{good}\nnot json\n");
    let err = read_index_shard_bytes(messy.as_bytes()).unwrap_err();
    match err {
        IndexShardDecodeError::Json { line, .. } => assert_eq!(line, 2),
        other => panic!("expected Json error with line=2, got {other:?}"),
    }
}

#[test]
fn dedupe_canonicalizes_and_removes_duplicates() {
    let r1 = record("src.cli", "alpha", NodeKind::Function, "src/cli.py");
    let r2 = record("src.cli", "beta", NodeKind::Function, "src/cli.py");

    // Simulate a post-merge state with duplicates and unsorted order.
    let messy = vec![r2.clone(), r1.clone(), r1.clone(), r2.clone(), r1.clone()];
    let cleaned = dedupe_and_canonicalize(messy);
    assert_eq!(cleaned.len(), 2);
    // Sorted (alpha < beta lexicographically)
    assert_eq!(cleaned[0].symbol.as_ref(), "alpha");
    assert_eq!(cleaned[1].symbol.as_ref(), "beta");
}

#[test]
fn determinism_across_constructions() {
    // 100 builds of the same input must produce byte-identical
    // output.
    let records = vec![
        record(
            "src.cli",
            "explore_command",
            NodeKind::Function,
            "src/cli.py",
        ),
        record("src.cli", "Helper", NodeKind::Class, "src/cli.py"),
        record("src.util", "alpha", NodeKind::Function, "src/util.py"),
    ];
    let first = write_index_shard_bytes(&records).unwrap();
    for _ in 0..100 {
        assert_eq!(write_index_shard_bytes(&records).unwrap(), first);
    }
}

#[test]
fn empty_shard_round_trips() {
    let bytes = write_index_shard_bytes(&[]).unwrap();
    assert!(bytes.is_empty());
    let parsed = read_index_shard_bytes(&bytes).unwrap();
    assert!(parsed.is_empty());
}

#[test]
fn sort_key_includes_kind() {
    // Same module, same symbol, different kinds — sort by kind name
    // distinguishes them.
    let fn_record = record("m", "x", NodeKind::Function, "src/x.py");
    let class_record = record("m", "x", NodeKind::Class, "src/x.py");
    let bytes = write_index_shard_bytes(&[fn_record.clone(), class_record.clone()]).unwrap();
    let parsed = read_index_shard_bytes(&bytes).unwrap();
    // "class" < "function" lexicographically, so Class comes first.
    assert_eq!(parsed[0].kind, NodeKind::Class);
    assert_eq!(parsed[1].kind, NodeKind::Function);
}
