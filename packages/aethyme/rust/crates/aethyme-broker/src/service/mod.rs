//! Incremental shared broker service. Existing CLI operations remain local
//! until explicitly migrated; protocol availability never enables routing.

pub mod protocol;

#[cfg(unix)]
pub mod transport;
