//! `aethyme-graph-query` — read-side CLI exercising the
//! `FragmentStore` API against a repo's `.aethyme/graph/` tree.
//!
//! Phase 4.3 deliverable: makes the graph queryable from the
//! shell without going through aethyme-engine. Future phases
//! will wire equivalent queries into the engine's intent layer.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::FragmentStore;

#[derive(Parser, Debug)]
#[command(
    name = "aethyme-graph-query",
    about = "Query the committed Aethyme graph for symbols, modules, and counts.",
)]
struct Cli {
    /// Absolute path to the repo root. The store reads from
    /// `<repo-root>/.aethyme/graph/`.
    #[arg(long, value_name = "PATH")]
    repo_root: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Look up symbols by name.
    FindSymbol {
        /// Symbol name (matched exactly).
        #[arg(long)]
        name: String,

        /// Restrict to a specific module (omit to scan all).
        #[arg(long)]
        module: Option<String>,

        /// Restrict to a specific kind. Accepts the canonical
        /// snake_case kind name (e.g. "function", "class").
        #[arg(long)]
        kind: Option<String>,
    },
    /// List all modules that have an index shard on disk.
    ListModules,
    /// List all source paths that have a fragment on disk.
    ListFiles,
    /// Distinct languages observed across all File nodes.
    ListLanguages,
    /// Aggregate node counts by kind across the whole store.
    Counts,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aethyme-graph-query: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let store = FragmentStore::open(cli.repo_root.clone())?;

    match cli.command {
        Command::FindSymbol {
            name,
            module,
            kind,
        } => {
            let kind = parse_kind(kind.as_deref())?;
            let hits =
                store.find_symbols(module.as_deref(), &name, kind)?;
            if hits.is_empty() {
                println!("aethyme-graph-query: no matches for name={name:?}");
            } else {
                println!("aethyme-graph-query: {} match(es):", hits.len());
                for h in &hits {
                    println!(
                        "  {} {}::{} in {} ({})",
                        h.kind.name(),
                        h.module,
                        h.symbol,
                        h.file,
                        h.node_id.as_str(),
                    );
                }
            }
        }
        Command::ListModules => {
            let modules = store.list_modules()?;
            for m in modules {
                println!("{m}");
            }
        }
        Command::ListFiles => {
            let files = store.list_indexed_source_paths()?;
            for f in files {
                println!("{f}");
            }
        }
        Command::ListLanguages => {
            let langs = store.list_languages()?;
            for l in langs {
                println!("{l}");
            }
        }
        Command::Counts => {
            let counts = store.count_nodes_by_kind()?;
            for (kind, count) in counts {
                println!("{} = {}", kind.name(), count);
            }
        }
    }

    Ok(())
}

fn parse_kind(s: Option<&str>) -> Result<Option<NodeKind>, String> {
    match s {
        None => Ok(None),
        Some(s) => NodeKind::from_name(s)
            .map(Some)
            .map_err(|e| format!("--kind: {e}")),
    }
}
