//! Typed CLI errors so the router can distinguish misuse from failure.
//!
//! ## Why this exists
//!
//! Every front end returned `Result<(), String>`, so the router could
//! not tell a *usage* error ("you passed the wrong arguments") from a
//! *runtime* failure ("the store is missing", "that node does not
//! exist"). Both exited 1, while unknown top-level subcommands and
//! `explore` usage errors exited 2. An agent that scripted against the
//! CLI had to string-match stderr to find out what went wrong.
//!
//! [`CliError`] splits the two cases and lets the router map them to
//! distinct exit codes:
//!
//! - [`CliErrorKind::Usage`] → exit 2, "you asked for something that
//!   does not exist as an invocation".
//! - [`CliErrorKind::Runtime`] → exit 1, "the invocation was valid but
//!   the work failed".
//!
//! A not-found target is deliberately **runtime**, not usage: the
//! invocation was well-formed, the symbol simply is not in the graph.
//! That distinction is what lets an agent tell "I mistyped the flag"
//! from "I mistyped the symbol".
//!
//! `From<String>` maps to [`CliErrorKind::Runtime`], so the ~200
//! existing `?` sites that propagate a plain message keep compiling
//! and keep their current meaning. Front ends opt into a usage exit code
//! by returning [`CliError::usage`] explicitly.

use std::fmt;

/// Which class of failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliErrorKind {
    /// The invocation itself was wrong: unknown flag, missing
    /// subcommand, unparseable value, wrong arity.
    Usage,
    /// The invocation was valid but the work could not complete:
    /// missing store, unreadable path, target not found.
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    kind: CliErrorKind,
    message: String,
}

impl CliError {
    /// A usage error. The router exits 2 for these.
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            kind: CliErrorKind::Usage,
            message: message.into(),
        }
    }

    /// A runtime failure. The router exits 1 for these.
    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            kind: CliErrorKind::Runtime,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> CliErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Process exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self.kind {
            CliErrorKind::Usage => 2,
            CliErrorKind::Runtime => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// Propagating a bare message keeps its historical meaning: a runtime
/// failure. Front ends that want the usage exit code return
/// [`CliError::usage`] at the point where they detect misuse.
impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self::runtime(message)
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        Self::runtime(message.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::runtime(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_errors_exit_two_and_runtime_errors_exit_one() {
        assert_eq!(CliError::usage("bad flag").exit_code(), 2);
        assert_eq!(CliError::runtime("store missing").exit_code(), 1);
    }

    #[test]
    fn bare_messages_are_runtime_failures() {
        // This is the compatibility guarantee for the existing `?`
        // sites: they keep exiting 1.
        let from_string = CliError::from("node not found: x".to_string());
        assert_eq!(from_string.kind(), CliErrorKind::Runtime);
        let from_str = CliError::from("io failure");
        assert_eq!(from_str.kind(), CliErrorKind::Runtime);
    }

    #[test]
    fn message_survives_conversion() {
        let error = CliError::from("graph store is missing".to_string());
        assert_eq!(error.message(), "graph store is missing");
        assert_eq!(error.to_string(), "graph store is missing");
    }

    #[test]
    fn question_mark_converts_from_io_error() {
        fn fallible() -> Result<(), CliError> {
            Err(std::io::Error::other("boom"))?
        }
        let error = fallible().unwrap_err();
        assert_eq!(error.kind(), CliErrorKind::Runtime);
        assert!(error.message().contains("boom"));
    }
}