//! Build-if-missing resolution of the product binaries.
//!
//! Successor to `tests/support/engine_binary.py` and the binary half of
//! `tests/support/cli_invoke.py`.
//!
//! Two deliberate carry-overs from the retired Python harness:
//!
//! * **No environment-dependent skips.** A build failure is an
//!   `AssertionError`, never a skip. Environment-dependent skips are a
//!   known gate blind spot (python-retirement-plan.md, cross-phase
//!   risks): a suite that quietly skips its subject looks identical to a
//!   suite that passes.
//! * **`CARGO_TARGET_DIR` is honoured**, because the broker gates share
//!   one target dir across merge-simulation worktrees.
//!
//! One deliberate divergence: instead of "use whatever binary exists,
//! newest mtime wins", every resolution runs `cargo build` first. The
//! Python harness could not afford that (it paid a cargo invocation per
//! pytest session and hand-rolled a staleness check over `rglob("*.rs")`
//! to avoid it); a Rust test binary is already running inside a cargo
//! build graph, so an up-to-date `cargo build --bin` is nearly free and
//! removes the staleness question entirely. It also removes the
//! CPython `rglob`-ordering landmine the old staleness scan carried.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::paths::rust_workspace_root;

/// The Cargo target directory, honouring `CARGO_TARGET_DIR`.
pub fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| rust_workspace_root().join("target"))
}

fn built() -> &'static Mutex<HashMap<&'static str, PathBuf>> {
    static BUILT: OnceLock<Mutex<HashMap<&'static str, PathBuf>>> = OnceLock::new();
    BUILT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Build `name` if needed and return its path.
///
/// Cached per test binary, so a suite with fifty cases pays at most one
/// `cargo build` per bin. Panics — loudly — when the build fails.
pub fn cargo_bin(name: &'static str) -> PathBuf {
    let mut cache = built().lock().expect("testkit bin cache poisoned");
    if let Some(path) = cache.get(name) {
        return path.clone();
    }

    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--quiet", "--bin", name])
        .current_dir(rust_workspace_root())
        .output()
        .unwrap_or_else(|error| panic!("spawn cargo build --bin {name}: {error}"));
    assert!(
        output.status.success(),
        "cargo build --bin {name} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let path = target_dir().join("debug").join(name);
    assert!(path.is_file(), "{name} missing after build: {}", path.display());
    cache.insert(name, path.clone());
    path
}

/// The `aethyme` router binary — the single entry point every
/// implementation-blind CLI suite drives.
pub fn aethyme_bin() -> PathBuf {
    cargo_bin("aethyme")
}

/// The engine daemon/CLI sibling (`aethyme-engine-cli`).
pub fn engine_bin() -> PathBuf {
    cargo_bin("aethyme-engine-cli")
}

/// The fragment writer (`aethyme-graph-index`).
pub fn graph_index_bin() -> PathBuf {
    cargo_bin("aethyme-graph-index")
}
