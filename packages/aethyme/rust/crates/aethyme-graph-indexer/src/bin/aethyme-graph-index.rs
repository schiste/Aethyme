//! `aethyme-graph-index` CLI — runs the indexer against a repo
//! and writes per-file fragments + per-module index shards under
//! `.aethyme/graph/`.
//!
//! Usage:
//!
//! ```text
//! aethyme-graph-index \
//!     --repo-root /path/to/repo \
//!     --repo-name my-repo \
//!     --engine-version 0.1.0
//! ```
//!
//! Exit codes:
//!   0 — success, summary printed to stdout
//!   1 — indexing failed; error printed to stderr
//!   2 — argument validation failed (clap-default)

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use aethyme_graph_indexer::{
    index_repo_to_disk, IndexerContext, IndexRepoSummary, WalkOptions,
};
use aethyme_graph_storage::bootstrap_repo;

/// Indexes a source repository into the Aethyme graph format.
///
/// Walks the repo, dispatches each code file to its registered
/// language indexer, and writes per-file binary fragments plus
/// per-module NDJSON index shards under `<repo-root>/.aethyme/
/// graph/`. The layout is the committed-artifact form locked in
/// Phase 2 (per `docs/architecture/graph-schema.md` §5).
#[derive(Parser, Debug)]
#[command(
    name = "aethyme-graph-index",
    about = "Index a source repo into per-file binary fragments + per-module NDJSON shards.",
    long_about = None,
)]
struct Cli {
    /// Absolute path to the repo root. Source files are
    /// discovered relative to this path.
    #[arg(long, value_name = "PATH")]
    repo_root: PathBuf,

    /// Stable repo namespace identifier. Used as the `<repo>`
    /// component of every NodeId. Must not contain `:`.
    #[arg(long, value_name = "NAME")]
    repo_name: String,

    /// Engine version string written to `.aethyme/engine-version`
    /// and stored on the context. CI uses this for the
    /// parser-version-drift check.
    #[arg(long, value_name = "VERSION")]
    engine_version: String,

    /// Skip the bootstrap step (creating `.aethyme/graph/`,
    /// `.gitattributes`, `engine-version`). Use for re-indexing
    /// existing repos where the bootstrap already ran.
    #[arg(long)]
    skip_bootstrap: bool,

    /// Skip files larger than this many bytes (default 5 MiB,
    /// matching the library's WalkOptions default).
    #[arg(long, value_name = "BYTES")]
    max_file_size: Option<u64>,

    /// Repo-relative directory to skip during the walk (repeatable).
    /// Extends the default ignore list (.git/, node_modules/, etc.).
    #[arg(long = "extra-ignore", value_name = "DIR")]
    extra_ignore: Vec<String>,

    /// Print JSON summary instead of human-readable text. Useful
    /// when piping into eval harnesses or scripts.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aethyme-graph-index: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    if !cli.skip_bootstrap {
        bootstrap_repo(&cli.repo_root, &cli.engine_version)?;
    }

    let ctx = IndexerContext::new(
        &cli.repo_name,
        cli.repo_root.clone(),
        &cli.engine_version,
    )?;

    let options = WalkOptions {
        extra_ignore_dirs: cli.extra_ignore.clone(),
        max_file_size_bytes: cli.max_file_size,
    };

    let started = Instant::now();
    let summary = index_repo_to_disk(&ctx, &options)?;
    let elapsed = started.elapsed();

    if cli.json {
        emit_json_summary(&summary, elapsed.as_millis() as u64);
    } else {
        emit_text_summary(&summary, &elapsed);
    }

    Ok(())
}

fn emit_text_summary(summary: &IndexRepoSummary, elapsed: &std::time::Duration) {
    println!(
        "aethyme-graph-index: indexed {} files ({} skipped) in {:.2}s",
        summary.total_files,
        summary.total_skipped,
        elapsed.as_secs_f64(),
    );
    println!(
        "  wrote {} fragments, {} index shards",
        summary.fragments_written.len(),
        summary.shards_written.len(),
    );
    if !summary.counts_by_kind.is_empty() {
        println!("  nodes by kind:");
        // BTreeMap iteration is sorted by kind; canonical-order
        // output matches the rest of the crate's determinism
        // discipline.
        for (kind, count) in &summary.counts_by_kind {
            println!("    {} = {}", kind.name(), count);
        }
    }
}

fn emit_json_summary(summary: &IndexRepoSummary, elapsed_ms: u64) {
    // Hand-rolled JSON to avoid an extra serde_json dep in the
    // binary's compile graph and to keep the output shape
    // explicitly under our control (it's a wire format for
    // downstream consumers).
    print!("{{");
    print!("\"total_files\":{},", summary.total_files);
    print!("\"total_skipped\":{},", summary.total_skipped);
    print!("\"fragments_written\":{},", summary.fragments_written.len());
    print!("\"shards_written\":{},", summary.shards_written.len());
    print!("\"elapsed_ms\":{},", elapsed_ms);
    print!("\"counts_by_kind\":{{");
    for (i, (kind, count)) in summary.counts_by_kind.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!("\"{}\":{}", kind.name(), count);
    }
    print!("}}}}");
    println!();
}
