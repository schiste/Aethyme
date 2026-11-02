//! Read-only addressing for an explicitly selected private service root.

use super::{job_protocol::RepositoryIdentity, job_runtime, transport};
use serde::Serialize;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::SocketAddr;
use std::path::{Path, PathBuf};

/// A location is an address, not proof that a service exists or owns a repository.
#[derive(Debug, Serialize)]
pub struct ServiceDirectory {
    pub schema_version: u32,
    pub directory: PathBuf,
    pub repository: RepositoryIdentity,
}

/// Resolve the same address for the main checkout and its linked worktrees.
/// Neither the service directory nor repository state is created or maintained.
pub fn locate(root: &Path, repository: &Path) -> io::Result<ServiceDirectory> {
    let before = transport::private_directory(root)?;
    let repository = job_runtime::selected_repository_identity(repository)?;
    // Shorten only the address, leaving enough room for a Unix socket pathname.
    // Callers must still verify and pin the full repository identity on requests.
    let directory = root.join(&repository.key[..32]);
    let _socket_address = SocketAddr::from_pathname(directory.join("broker-service.sock"))?;
    match fs::symlink_metadata(&directory) {
        Ok(_) => {
            transport::private_directory(&directory)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let after = transport::private_directory(root)?;
    if before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private service root changed during discovery",
        ));
    }
    Ok(ServiceDirectory {
        schema_version: 1,
        directory,
        repository,
    })
}
