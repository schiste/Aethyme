//! Aethyme CLI router. Selects the platform entry point at compile time.
//!
//! Windows phase 1 provides native navigation and generated deployment;
//! Unix retains the full broker-backed command suite.
#[cfg(unix)]
include!("main_unix.rs");

#[cfg(windows)]
include!("main_windows.rs");
