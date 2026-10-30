//! Incremental shared broker service. Existing CLI operations remain local
//! until explicitly migrated; protocol availability never enables routing.

pub mod job_protocol;
pub mod jobs;
pub mod protocol;

#[cfg(unix)]
pub(crate) mod client;

#[cfg(unix)]
pub mod discovery;

#[cfg(unix)]
pub(crate) mod job_runtime;

#[cfg(unix)]
pub mod transport;
