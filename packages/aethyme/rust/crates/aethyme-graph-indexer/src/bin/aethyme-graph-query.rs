//! `aethyme-graph-query` — read-side CLI exercising the
//! `FragmentStore` API against a repo's `.aethyme/graph/` tree.
//!
//! Phase 4.3 deliverable: makes the graph queryable from the
//! shell without going through aethyme-engine. Future phases
//! will wire equivalent queries into the engine's intent layer.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Arg, value_parser};

use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::FragmentStore;

#[derive(Debug)]
struct Cli {
    repo_root: PathBuf,
    command: Command,
}

#[derive(Debug)]
enum Command {
    FindSymbol {
        name: String,
        module: Option<String>,
        kind: Option<String>,
    },
    ListModules,
    ListFiles,
    ListLanguages,
    Counts,
}

/// Built with clap's builder API, not its derive macros, so the workspace
/// compiles one `syn` (see `aethyme-graph-index`). A missing subcommand is an
/// error, and no arguments at all prints help, as the derive form did.
fn command() -> clap::Command {
    clap::Command::new("aethyme-graph-query")
        .about("Query the committed Aethyme graph for symbols, modules, and counts.")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new("repo_root")
                .long("repo-root")
                .value_name("PATH")
                .required(true)
                .value_parser(value_parser!(PathBuf))
                .help("Absolute path to the repo root. The store reads from `<repo-root>/.aethyme/graph/`"),
        )
        .subcommand(
            clap::Command::new("find-symbol")
                .about("Look up symbols by name")
                .arg(
                    Arg::new("name")
                        .long("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Symbol name (matched exactly)"),
                )
                .arg(
                    Arg::new("module")
                        .long("module")
                        .value_name("MODULE")
                        .help("Restrict to a specific module (omit to scan all)"),
                )
                .arg(Arg::new("kind").long("kind")
                        .value_name("KIND").help(
                    "Restrict to a specific kind. Accepts the canonical snake_case kind name (e.g. \"function\", \"class\")",
                )),
        )
        .subcommand(
            clap::Command::new("list-modules")
                .about("List all modules that have an index shard on disk"),
        )
        .subcommand(
            clap::Command::new("list-files")
                .about("List all source paths that have a fragment on disk"),
        )
        .subcommand(
            clap::Command::new("list-languages")
                .about("Distinct languages observed across all File nodes"),
        )
        .subcommand(
            clap::Command::new("counts")
                .about("Aggregate node counts by kind across the whole store"),
        )
}

impl Cli {
    fn parse() -> Self {
        let mut matches = command().get_matches();
        let repo_root = matches.remove_one("repo_root").expect("required");
        let command = match matches.remove_subcommand() {
            Some((name, mut sub)) => match name.as_str() {
                "find-symbol" => Command::FindSymbol {
                    name: sub.remove_one("name").expect("required"),
                    module: sub.remove_one("module"),
                    kind: sub.remove_one("kind"),
                },
                "list-modules" => Command::ListModules,
                "list-files" => Command::ListFiles,
                "list-languages" => Command::ListLanguages,
                "counts" => Command::Counts,
                other => unreachable!("clap accepted unknown subcommand {other}"),
            },
            None => unreachable!("clap requires a subcommand"),
        };
        Self { repo_root, command }
    }
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
        Command::FindSymbol { name, module, kind } => {
            let kind = parse_kind(kind.as_deref())?;
            let hits = store.find_symbols(module.as_deref(), &name, kind)?;
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
