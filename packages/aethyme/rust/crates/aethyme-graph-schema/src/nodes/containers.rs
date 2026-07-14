//! Container node kinds: Repository, Directory, Module, File, NonCodeFile.
//!
//! Required fields per schema doc §3.1.

use serde::{Deserialize, Serialize};

use crate::{NodeId, NodeKind};

// ─── Repository ──────────────────────────────────────────────────────

/// The top of a repo's namespace. One per repo.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Repository {
    id: NodeId,
    name: Box<str>,
    root_path: Box<str>,
    /// VCS in use. Kept as a free-form string (rather than an enum)
    /// because the set of VCS systems is small and stable enough
    /// that introducing an enum here adds boilerplate without much
    /// type-safety benefit.
    vcs: Box<str>,
}

impl Repository {
    pub fn new(
        name: &str,
        root_path: &str,
        vcs: &str,
    ) -> Result<Self, RepositoryConstructionError> {
        if name.is_empty() {
            return Err(RepositoryConstructionError::EmptyName);
        }
        if root_path.is_empty() {
            return Err(RepositoryConstructionError::EmptyRootPath);
        }
        if vcs.is_empty() {
            return Err(RepositoryConstructionError::EmptyVcs);
        }
        let id = NodeId::new(NodeKind::Repository, name, "", "")
            .map_err(RepositoryConstructionError::Id)?;
        Ok(Repository {
            id,
            name: name.into(),
            root_path: root_path.into(),
            vcs: vcs.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn root_path(&self) -> &str {
        &self.root_path
    }
    pub fn vcs(&self) -> &str {
        &self.vcs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryConstructionError {
    EmptyName,
    EmptyRootPath,
    EmptyVcs,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for RepositoryConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Repository: name must not be empty"),
            Self::EmptyRootPath => f.write_str("Repository: root_path must not be empty"),
            Self::EmptyVcs => f.write_str("Repository: vcs must not be empty"),
            Self::Id(e) => write!(f, "Repository: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for RepositoryConstructionError {}

// ─── Directory ───────────────────────────────────────────────────────

/// A source directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Directory {
    id: NodeId,
    path: Box<str>,
}

impl Directory {
    pub fn new(repo: &str, path: &str) -> Result<Self, DirectoryConstructionError> {
        if path.is_empty() {
            return Err(DirectoryConstructionError::EmptyPath);
        }
        let id = NodeId::new(NodeKind::Directory, repo, path, "")
            .map_err(DirectoryConstructionError::Id)?;
        Ok(Directory {
            id,
            path: path.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryConstructionError {
    EmptyPath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for DirectoryConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("Directory: path must not be empty"),
            Self::Id(e) => write!(f, "Directory: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for DirectoryConstructionError {}

// ─── Module ──────────────────────────────────────────────────────────

/// A logical module (Python package, TS module, PHP namespace).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Module {
    id: NodeId,
    path: Box<str>,
    name: Box<str>,
    language: Box<str>,
}

impl Module {
    pub fn new(
        repo: &str,
        path: &str,
        name: &str,
        language: &str,
    ) -> Result<Self, ModuleConstructionError> {
        if path.is_empty() {
            return Err(ModuleConstructionError::EmptyPath);
        }
        if name.is_empty() {
            return Err(ModuleConstructionError::EmptyName);
        }
        if language.is_empty() {
            return Err(ModuleConstructionError::EmptyLanguage);
        }
        let id =
            NodeId::new(NodeKind::Module, repo, path, name).map_err(ModuleConstructionError::Id)?;
        Ok(Module {
            id,
            path: path.into(),
            name: name.into(),
            language: language.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn language(&self) -> &str {
        &self.language
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleConstructionError {
    EmptyPath,
    EmptyName,
    EmptyLanguage,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for ModuleConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("Module: path must not be empty"),
            Self::EmptyName => f.write_str("Module: name must not be empty"),
            Self::EmptyLanguage => f.write_str("Module: language must not be empty"),
            Self::Id(e) => write!(f, "Module: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for ModuleConstructionError {}

// ─── File ────────────────────────────────────────────────────────────

/// A source code file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct File {
    id: NodeId,
    path: Box<str>,
    language: Box<str>,
    byte_size: u64,
    /// Content-addressed hash of the file's full contents. Used by
    /// indexers that need to detect identical-content files across
    /// paths. Format is a hex string; this layer doesn't constrain
    /// which hash algorithm produced it (BLAKE3 is recommended for
    /// consistency with NodeId).
    content_hash: Box<str>,
}

impl File {
    pub fn new(
        repo: &str,
        path: &str,
        language: &str,
        byte_size: u64,
        content_hash: &str,
    ) -> Result<Self, FileConstructionError> {
        if path.is_empty() {
            return Err(FileConstructionError::EmptyPath);
        }
        if language.is_empty() {
            return Err(FileConstructionError::EmptyLanguage);
        }
        if content_hash.is_empty() {
            return Err(FileConstructionError::EmptyContentHash);
        }
        let id = NodeId::new(NodeKind::File, repo, path, "").map_err(FileConstructionError::Id)?;
        Ok(File {
            id,
            path: path.into(),
            language: language.into(),
            byte_size,
            content_hash: content_hash.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn language(&self) -> &str {
        &self.language
    }
    pub fn byte_size(&self) -> u64 {
        self.byte_size
    }
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileConstructionError {
    EmptyPath,
    EmptyLanguage,
    EmptyContentHash,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for FileConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("File: path must not be empty"),
            Self::EmptyLanguage => f.write_str("File: language must not be empty"),
            Self::EmptyContentHash => f.write_str("File: content_hash must not be empty"),
            Self::Id(e) => write!(f, "File: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for FileConstructionError {}

// ─── NonCodeFile ─────────────────────────────────────────────────────

/// A non-code file (Markdown, YAML, JSON, TOML, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NonCodeFile {
    id: NodeId,
    path: Box<str>,
    format: NonCodeFormat,
}

impl NonCodeFile {
    pub fn new(
        repo: &str,
        path: &str,
        format: NonCodeFormat,
    ) -> Result<Self, NonCodeFileConstructionError> {
        if path.is_empty() {
            return Err(NonCodeFileConstructionError::EmptyPath);
        }
        let id = NodeId::new(NodeKind::NonCodeFile, repo, path, "")
            .map_err(NonCodeFileConstructionError::Id)?;
        Ok(NonCodeFile {
            id,
            path: path.into(),
            format,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn format(&self) -> &NonCodeFormat {
        &self.format
    }
}

/// Recognized formats for [`NonCodeFile`]. The `Other` escape hatch
/// covers any format we don't enumerate, with the carrier string
/// for downstream-typed parsers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonCodeFormat {
    Markdown,
    Yaml,
    Json,
    Toml,
    Plain,
    Other(Box<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonCodeFileConstructionError {
    EmptyPath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for NonCodeFileConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("NonCodeFile: path must not be empty"),
            Self::Id(e) => {
                write!(f, "NonCodeFile: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for NonCodeFileConstructionError {}

// ─── Package ─────────────────────────────────────────────────────────

/// A package: a build/distribution unit rooted at a manifest file
/// (a Cargo crate's `Cargo.toml`, an npm package's `package.json`,
/// a Python project's `pyproject.toml`, a Godot project's
/// `project.godot`, etc.).
///
/// Distinct from [`Module`]:
/// - `Module` is a **language-level** construct (Python package,
///   TS module, PHP namespace) — what the language's import
///   resolver sees.
/// - `Package` is a **build/distribution** unit — what the package
///   manager publishes and what dependencies pin against.
///
/// One repository can contain many packages (monorepo); one
/// package can contain many modules. The two are produced by
/// different overlays and live side-by-side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Package {
    id: NodeId,
    /// The manifest's parent directory, relative to the repo root.
    /// This is the package's "home" — sibling files belong to this
    /// package by default.
    path: Box<str>,
    /// The package's declared name (parsed from the manifest when
    /// present, falling back to the directory basename when the
    /// manifest format has no name field).
    name: Box<str>,
    /// The manifest path, relative to the repo root. Always points
    /// at a single file (Cargo.toml, package.json, …).
    manifest_path: Box<str>,
    /// A short tag identifying which manifest family produced this
    /// package: `cargo`, `npm`, `pyproject`, `godot`, `docker`,
    /// `tsconfig`, `jsconfig`, `turbo`, `biome`, `deno`,
    /// `pnpm-workspace`. Free-form string so producers can extend
    /// the set without a schema change.
    manifest_kind: Box<str>,
}

impl Package {
    pub fn new(
        repo: &str,
        path: &str,
        name: &str,
        manifest_path: &str,
        manifest_kind: &str,
    ) -> Result<Self, PackageConstructionError> {
        if path.is_empty() {
            return Err(PackageConstructionError::EmptyPath);
        }
        if name.is_empty() {
            return Err(PackageConstructionError::EmptyName);
        }
        if manifest_path.is_empty() {
            return Err(PackageConstructionError::EmptyManifestPath);
        }
        if manifest_kind.is_empty() {
            return Err(PackageConstructionError::EmptyManifestKind);
        }
        let id = NodeId::new(NodeKind::Package, repo, path, name)
            .map_err(PackageConstructionError::Id)?;
        Ok(Package {
            id,
            path: path.into(),
            name: name.into(),
            manifest_path: manifest_path.into(),
            manifest_kind: manifest_kind.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }
    pub fn manifest_kind(&self) -> &str {
        &self.manifest_kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageConstructionError {
    EmptyPath,
    EmptyName,
    EmptyManifestPath,
    EmptyManifestKind,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for PackageConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("Package: path must not be empty"),
            Self::EmptyName => f.write_str("Package: name must not be empty"),
            Self::EmptyManifestPath => f.write_str("Package: manifest_path must not be empty"),
            Self::EmptyManifestKind => f.write_str("Package: manifest_kind must not be empty"),
            Self::Id(e) => write!(f, "Package: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for PackageConstructionError {}
