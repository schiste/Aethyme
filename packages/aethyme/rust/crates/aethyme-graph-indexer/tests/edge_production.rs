//! Every schema edge needs persisted producer evidence or an explicit gap.

#[path = "support/surface_fixture.rs"]
mod surface_fixture;

use std::collections::BTreeMap;
use std::path::Path;

use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk};
use aethyme_graph_schema::{ALL_EDGE_KINDS, EdgeKind};
use aethyme_graph_storage::read_fragment_bytes;

#[derive(Debug)]
enum Production {
    Witnessed,
    NotProduced { issue: u32, reason: &'static str },
}

// Deliberately exhaustive: a new schema variant must make an explicit producer
// decision. Witnessed means this fixture emits it, not complete language or
// semantic coverage. Unsupported declarations must be removed when a producer
// starts emitting the kind.
fn production(kind: EdgeKind) -> Production {
    use EdgeKind::*;
    match kind {
        Contains | Defines | Imports | Calls | Tests | Documents | References | Authorizes
        | Exposes | ForwardsTo | InstallsMiddleware | IssuesCredential | StoresCredential
        | UsesCredential | ValidatesCredential | RewritesHeader | TestedBy => Production::Witnessed,
        Implements => Production::NotProduced {
            issue: 213,
            reason: "trait and interface implementation relationships have no default producer",
        },
        Inherits => Production::NotProduced {
            issue: 213,
            reason: "class inheritance relationships have no default producer",
        },
        Reads | Writes => Production::NotProduced {
            issue: 213,
            reason: "statement-level dataflow is not produced by the default indexers",
        },
        Uses => Production::NotProduced {
            issue: 213,
            reason: "function-level binding-use relationships have no default producer",
        },
        Mocks => Production::NotProduced {
            issue: 213,
            reason: "mock-to-symbol resolution has no default producer",
        },
        Configures => Production::NotProduced {
            issue: 213,
            reason: "config value references do not establish runtime code configuration reads",
        },
        Decides | Deprecates => Production::NotProduced {
            issue: 213,
            reason: "document references do not establish decision or deprecation semantics",
        },
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(from, to).unwrap();
        }
    }
}

#[test]
fn every_edge_kind_has_a_persisted_witness_or_an_explicit_unproduced_issue() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("playground");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playground_docs");
    copy_tree(&fixture, &root);
    surface_fixture::write_surface_fixture(&root);
    std::fs::write(
        root.join("src/import_witness.py"),
        "from src.handlers import handle_request\n\ndef caller():\n    return handle_request()\n\nclass Client:\n    def run(self):\n        return handle_request()\n",
    )
    .unwrap();
    let context = IndexerContext::new("edge-production", &root, "0.1.0").unwrap();
    let summary = index_repo_to_disk(&context, &WalkOptions::default()).unwrap();

    // Read actual persisted fragments, not source constructor spellings or a
    // hand-authored capability report. Cross-check the coverage projection too.
    let mut observed = BTreeMap::<String, u64>::new();
    for path in &summary.fragments_written {
        let fragment = read_fragment_bytes(&std::fs::read(path).unwrap()).unwrap();
        for edge in fragment.edges() {
            *observed.entry(edge.kind().name().into()).or_default() += 1;
        }
    }
    assert_eq!(observed, summary.coverage.report.edge_counts_by_kind);
    for &kind in ALL_EDGE_KINDS {
        let count = observed.get(kind.name()).copied().unwrap_or(0);
        match production(kind) {
            Production::Witnessed => assert!(count > 0, "missing persisted witness for {kind:?}"),
            Production::NotProduced { issue, reason } => {
                assert!(issue > 0 && !reason.is_empty());
                assert_eq!(
                    count, 0,
                    "{kind:?} is now produced: retire gap #{issue}: {reason}"
                );
            }
        }
    }
}
