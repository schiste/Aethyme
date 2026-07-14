//! `aethyme-graph-link` CLI — runs the Phase 4.5 linker pass over
//! an already-indexed repo, resolving `UnresolvedSymbol`
//! placeholders into concrete cross-file edges.
//!
//! `aethyme-graph-index` runs the linker by default at the end of
//! indexing; this standalone binary exists so the link step can be
//! re-run (e.g. after the linker logic itself is updated) without
//! re-indexing the repo. It is also useful in CI: index in one job,
//! link in another, query in a third.
//!
//! Usage:
//!
//! ```text
//! aethyme-graph-link --repo-root /path/to/repo
//! ```
//!
//! Exit codes:
//!   0 — success, summary printed to stdout
//!   1 — link failed; error on stderr
//!   2 — argument validation failed (clap-default)

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use aethyme_graph_indexer::{LinkSummary, link_repo_path};

#[derive(Parser, Debug)]
#[command(
    name = "aethyme-graph-link",
    about = "Resolve cross-file UnresolvedSymbol placeholders into concrete edges.",
    long_about = None,
)]
struct Cli {
    /// Repo root containing the `.aethyme/graph/` tree.
    #[arg(long, value_name = "PATH")]
    repo_root: PathBuf,

    /// Print JSON summary instead of human-readable text.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aethyme-graph-link: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let summary = link_repo_path(cli.repo_root)?;
    let elapsed = started.elapsed();

    if cli.json {
        emit_json_summary(&summary, elapsed.as_millis() as u64);
    } else {
        emit_text_summary(&summary, &elapsed);
    }
    Ok(())
}

fn emit_text_summary(summary: &LinkSummary, elapsed: &std::time::Duration) {
    println!(
        "aethyme-graph-link: linked {} fragments in {:.2}s",
        summary.fragments_visited,
        elapsed.as_secs_f64(),
    );
    println!(
        "  resolved {}/{} placeholders, rewrote {} edges, removed {} orphans",
        summary.placeholders_resolved,
        summary.placeholders_seen,
        summary.edges_rewritten,
        summary.orphans_removed,
    );
    if summary.fragments_rewritten > 0 {
        println!(
            "  {} fragments rewritten to disk",
            summary.fragments_rewritten
        );
    }
}

fn emit_json_summary(summary: &LinkSummary, elapsed_ms: u64) {
    // Same hand-rolled JSON discipline as aethyme-graph-index.
    print!("{{");
    print!("\"elapsed_ms\":{},", elapsed_ms);
    print!("\"fragments_visited\":{},", summary.fragments_visited);
    print!("\"fragments_rewritten\":{},", summary.fragments_rewritten);
    print!("\"placeholders_seen\":{},", summary.placeholders_seen);
    print!(
        "\"placeholders_resolved\":{},",
        summary.placeholders_resolved
    );
    print!("\"edges_rewritten\":{},", summary.edges_rewritten);
    print!("\"orphans_removed\":{}", summary.orphans_removed);
    print!("}}");
    println!();
}
