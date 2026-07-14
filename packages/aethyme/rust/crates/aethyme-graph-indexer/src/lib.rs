//! Source-tree → graph indexer.
//!
//! Two layers:
//!
//! 1. **Filesystem indexer** (`filesystem.rs`): walks a repo, finds
//!    source files, computes file metadata (byte_size, content_hash,
//!    language from extension), and produces one `File` or
//!    `NonCodeFile` node per source file. No AST parsing — this
//!    layer establishes the per-file Fragment skeleton that
//!    language-specific indexers fill in.
//! 2. **Language indexers** (commits 3.2+): consume file content,
//!    parse via tree-sitter, and append symbol-level nodes +
//!    structural edges to the Fragment that the filesystem layer
//!    produced.
//!
//! The two layers are decoupled so a repo with mixed languages can
//! run the filesystem walk once and then dispatch each file to its
//! language-specific indexer (or leave it alone if the file is
//! non-code).

pub mod context;
pub mod filesystem;
pub mod language;
pub mod language_map;
pub mod linker;
pub mod php;
pub mod pipeline;
pub mod python;
pub mod rust_lang;
pub mod typescript;

pub use context::IndexerContext;
pub use filesystem::{
    FilesystemIndexResult, FilesystemIndexerError, IndexedFile, SkipReason, SkippedFile,
    WalkOptions, walk_source_tree,
};
pub use language::{
    LanguageIndexError, LanguageIndexResult, LanguageIndexer, LanguageRegistry, LineIndex,
};
pub use language_map::{FileClassification, classify_file, infer_language_from_extension};
pub use linker::{LinkError, LinkSummary, link_repo, link_repo_path, link_with_store};
pub use php::PhpIndexer;
pub use pipeline::{
    BuildFragmentError, BuiltFragment, IndexRepoError, IndexRepoSummary, build_fragment,
    build_index_records, default_registry, index_repo_to_disk,
};
pub use python::PythonIndexer;
pub use rust_lang::RustIndexer;
pub use typescript::TypeScriptIndexer;
