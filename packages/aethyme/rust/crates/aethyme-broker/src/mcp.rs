//! `aethyme mcp`: launch MCP servers without launcher chains.
//!
//! Agents start their MCP servers over stdio, often through package-manager
//! launchers (`pnpm exec`, `npm exec`, `npx`) that leave one or two extra
//! Node processes alive per server. With many agent sessions on one host the
//! launchers cost as much memory as the servers. This module resolves such a
//! chain offline to the installed executable and `exec`s it in place of the
//! `aethyme` process, so nothing extra stays alive.
//!
//! The pieces, each its own file so the shared hub (later) adds files rather
//! than rewriting these:
//! - [`checkouts`]: which checkout a path is in, and whether it is a
//!   broker session worktree.
//! - [`config`]: the host and repository service files, validated; entries
//!   must point at stable copies, never at broker session worktrees.
//! - [`resolve`]: launcher chain -> direct argv, without running or
//!   downloading anything, plus the executable's identity.
//! - [`key`]: which copy of a server a caller gets (stable, or its session
//!   worktree's when that differs) and the key that names one server.
//! - [`identity`]: content identities of source trees and lockfiles, capped
//!   and cached.
//! - [`secret`]: the per-host HMAC secret that keeps credentials out of keys.
//! - [`trust`]: host trust for repository files.
//! - [`envfile`]: `KEY=VALUE` files read without a shell.
//! - [`telemetry`]: phase timings, outcomes and remembered resolutions under
//!   `<host state>/mcp/`.
//! - [`jsonrpc`], [`mux`], [`hub`] and [`hub_client`]: the shared hub for
//!   `mode = "shared"` stdio servers: framing, multiplexing rules, the hub
//!   process, and `connect`'s side of it.
//! - [`connect`] and [`cli`]: the commands.
//!
//! Nothing here is automatic: an owner opts in by pasting the snippet `aethyme
//! mcp config print` shows into an agent's configuration.

pub mod checkouts;
pub mod cli;
pub mod config;
pub mod connect;
pub mod envfile;
pub mod hub;
pub mod hub_client;
pub mod identity;
pub mod jsonrpc;
pub mod key;
pub mod mux;
pub mod resolve;
pub mod secret;
pub mod telemetry;
pub mod trust;

// Managed servers: one matcher, config repair, launcher interception and
// bypass detection (see [`managed`]).
pub mod bypass;
pub mod caller;
pub mod intercept;
pub mod json_span;
pub mod managed;
pub mod managed_cli;
pub mod repair;

#[cfg(test)]
mod hub_tests;
#[cfg(test)]
mod managed_tests;
#[cfg(test)]
mod tests;

/// Directory under the host state directory that holds everything MCP.
pub const HOST_DIRECTORY: &str = "mcp";

/// `<host state>/mcp`, when the host state directory can be resolved.
pub fn host_directory() -> Option<std::path::PathBuf> {
    crate::host_state::default_host_state_dir().map(|root| root.join(HOST_DIRECTORY))
}
