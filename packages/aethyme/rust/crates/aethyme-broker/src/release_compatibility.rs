//! Compatibility values published with release artifacts.
//!
//! These constants deliberately live next to the runtime implementations.
//! Release tooling imports them instead of maintaining workflow-only copies.

/// Oldest broker database schema this binary can migrate and open.
pub const BROKER_STORAGE_MINIMUM_SCHEMA: i64 = 1;

/// Current broker database schema written by this binary.
pub const BROKER_STORAGE_CURRENT_SCHEMA: i64 = crate::schema::SCHEMA_VERSION;

/// Engine daemon wire-protocol version used by the paired sibling binary.
pub const ENGINE_PROTOCOL_VERSION: u32 = aethyme_engine::daemon::ENGINE_PROTOCOL_VERSION;

/// Current schema for repository-owned deployment policy and generated files.
pub const REPOSITORY_SCHEMA_VERSION: u32 = 1;

/// Minimum Git release required for merge-tree submission simulation.
pub const MINIMUM_GIT_VERSION: &str = "2.38";

pub(crate) fn minimum_git_version_parts() -> (u32, u32) {
    let (major, minor) = MINIMUM_GIT_VERSION
        .split_once('.')
        .expect("minimum Git version must be major.minor");
    (
        major.parse().expect("minimum Git major must be numeric"),
        minor.parse().expect("minimum Git minor must be numeric"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_range_tracks_runtime_schema_and_protocol() {
        assert_eq!(BROKER_STORAGE_CURRENT_SCHEMA, crate::SCHEMA_VERSION);
        assert!(BROKER_STORAGE_MINIMUM_SCHEMA <= BROKER_STORAGE_CURRENT_SCHEMA);
        assert_eq!(
            ENGINE_PROTOCOL_VERSION,
            aethyme_engine::daemon::ENGINE_PROTOCOL_VERSION
        );
        assert_eq!(REPOSITORY_SCHEMA_VERSION, 1);
        assert_eq!(minimum_git_version_parts(), (2, 38));
    }
}
