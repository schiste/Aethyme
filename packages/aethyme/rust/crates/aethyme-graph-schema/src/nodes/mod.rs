//! Per-kind node struct definitions.
//!
//! Each submodule holds the structs for one category. Every struct
//! carries its required fields and a [`crate::NodeId`] computed at
//! construction time. Common attributes (`source`, `confidence`,
//! `modification_history`, `extensions`) are NOT yet stored on the
//! per-kind structs — they'll land alongside the unifying `Node`
//! enum in commit 1.12.

pub mod containers;

pub use containers::{
    Directory, DirectoryConstructionError, File, FileConstructionError,
    Module, ModuleConstructionError, NonCodeFile,
    NonCodeFileConstructionError, NonCodeFormat, Repository,
    RepositoryConstructionError,
};
