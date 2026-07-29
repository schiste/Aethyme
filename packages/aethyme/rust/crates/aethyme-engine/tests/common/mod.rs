//! Shared test support for aethyme-engine integration tests.

/// Resolve the `aethyme` router binary for tests.
///
/// The router bin moved to the aethyme-cli crate (retirement plan
/// Phase 1 item 1, 2026-07-29), so `CARGO_BIN_EXE_aethyme` is no longer
/// defined when compiling this crate's tests. Resolve from the target
/// dir — honoring `CARGO_TARGET_DIR` (the broker gates share one) —
/// building the bin if absent. Any new test needing the router should
/// use this instead of `env!("CARGO_BIN_EXE_aethyme")`.
#[allow(dead_code)] // not every test binary that includes this module uses it
pub fn aethyme_bin() -> String {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    // ALWAYS build first: CARGO_BIN_EXE guaranteed a fresh bin for the
    // current tree; a resolve-if-exists shortcut can pick up a stale
    // binary from the shared gate target cache (caught live: an old
    // router failed the Surface/Flow leak-filter assertions). The build
    // is an incremental no-op when already fresh.
    let status = std::process::Command::new("cargo")
        .args(["build", "--quiet", "--bin", "aethyme"])
        .current_dir(&workspace_root)
        .status()
        .expect("spawn cargo build for aethyme bin");
    assert!(status.success(), "cargo build --bin aethyme failed");
    let debug = target_dir.join("debug").join("aethyme");
    assert!(debug.is_file(), "aethyme bin missing after build");
    debug.to_string_lossy().into_owned()
}
