//! Environments: declared, multi-component runnable stacks per worktree
//! (#1208). This module holds the profile layer (#1209): the
//! `.aethyme/environments.toml` schema, static validation, and compilation of
//! a profile into a pinned snapshot plus the host-resource requests that
//! reserve it.
//!
//! Aethyme supplies lifecycle, admission and routing; the repository supplies
//! commands. Nothing here knows any stack: a profile is components with
//! commands, and those commands are opaque strings run with `sh -c`.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub mod cli;
pub mod compile;
pub mod spec;
pub mod validate;

#[cfg(test)]
mod tests;

pub use compile::{
    CompileContext, CompiledComponent, CompiledEnvironment, EnvironmentOwner, compile,
    environment_id,
};
pub use spec::{EnvironmentsFile, HostEnvironmentsFile};
pub use validate::{Diagnostic, validate};

/// Repository-relative location of the environments file.
pub const ENVIRONMENTS_FILE: &str = ".aethyme/environments.toml";
/// Host-scoped settings (host pool limits), under the host state directory.
pub const HOST_ENVIRONMENTS_FILE: &str = "environments.toml";

/// A file read from disk, with the bytes' digest and a parse result whose
/// error, if any, is already a located diagnostic.
pub struct Loaded<T> {
    pub path: PathBuf,
    pub digest: String,
    pub value: Result<T, Box<Diagnostic>>,
}

/// Read and parse `<worktree>/.aethyme/environments.toml`. `Ok(None)` when the
/// repository declares no environments.
pub fn load_repository_file(
    worktree_root: &Path,
) -> std::io::Result<Option<Loaded<EnvironmentsFile>>> {
    let path = worktree_root.join(ENVIRONMENTS_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(parse_loaded(path, ENVIRONMENTS_FILE, &text))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read host settings. A missing file or host state directory is the empty
/// default: host pools then fail validation with the path to create.
pub fn load_host_file() -> std::io::Result<(String, Loaded<HostEnvironmentsFile>)> {
    let Some(directory) = crate::host_state::default_host_state_dir() else {
        let label = "<host state directory unavailable>".to_string();
        return Ok((
            label.clone(),
            Loaded {
                path: PathBuf::from(&label),
                digest: String::new(),
                value: Ok(HostEnvironmentsFile::default()),
            },
        ));
    };
    let path = directory.join(HOST_ENVIRONMENTS_FILE);
    let label = path.display().to_string();
    match fs::read_to_string(&path) {
        Ok(text) => Ok((label.clone(), parse_loaded(path, &label, &text))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
            label,
            Loaded {
                path,
                digest: String::new(),
                value: Ok(HostEnvironmentsFile::default()),
            },
        )),
        Err(error) => Err(error),
    }
}

pub fn parse_loaded<T: serde::de::DeserializeOwned>(
    path: PathBuf,
    label: &str,
    text: &str,
) -> Loaded<T> {
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    let value = toml::from_str::<T>(text).map_err(|error| {
        let (line, column) = error
            .span()
            .map(|span| line_column(text, span.start))
            .unzip();
        Box::new(Diagnostic {
            code: "parse_error",
            file: label.to_string(),
            line,
            column,
            profile: None,
            component: None,
            field: None,
            message: error.message().to_string(),
            fix: "fix the syntax or remove the unknown key".into(),
        })
    });
    Loaded {
        path,
        digest,
        value,
    }
}

/// 1-based line and column of a byte offset.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map_or(before.chars().count(), |newline| {
            before[newline + 1..].chars().count()
        })
        + 1;
    (line, column)
}
