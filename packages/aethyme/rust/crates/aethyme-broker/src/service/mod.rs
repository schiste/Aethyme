//! Incremental shared broker service. Existing CLI operations remain local
//! until explicitly migrated; protocol availability never enables routing.

pub mod job_protocol;
pub mod jobs;
pub mod protocol;

#[cfg(unix)]
pub mod client;

#[cfg(unix)]
pub mod discovery;

#[cfg(unix)]
pub mod startup;

#[cfg(unix)]
pub mod job_runtime;

#[cfg(unix)]
pub mod transport;

#[cfg(unix)]
mod socket_io;
