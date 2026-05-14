//! Schema types for the Aethyme code graph.
//!
//! This crate implements the design locked in
//! `packages/aethyme/docs/architecture/graph-schema.md`. It carries node
//! kinds, edge kinds, identity, common attributes, and serialization — the
//! "vocabulary" of the graph. It deliberately does NOT carry:
//!
//! - I/O (no file reading, no database access)
//! - Storage layout (no fragment paths, no redb tables)
//! - Parsing (no tree-sitter, no AST handling)
//! - Indexing logic (no traversal, no edge discovery)
//!
//! Those layers live in `aethyme-engine` and consume this crate's types.
//! Keeping the schema as a leaf crate with zero downstream dependencies has
//! two benefits: (1) the type design is the bottleneck for the
//! implementation, so iterating on types without dragging the rest of the
//! engine into recompiles keeps the feedback loop tight; (2) other consumers
//! (the Python CLI via FFI eventually, future tooling) can depend on the
//! schema without inheriting the engine's heavier dependencies.
//!
//! ## Stability contract
//!
//! Per the schema doc's principle 1.2 ("no schema versioning"), every type
//! in this crate is forever. Adding a new node kind, edge kind, or required
//! field is allowed; relaxing an existing required field, removing a kind,
//! or changing serialization shape is a breaking change to every
//! cross-process consumer of the graph. The contract that prevents
//! accidental breakage lives in
//! `docs/architecture/cross-process-consumers.md`.
//!
//! ## Module layout (target, populated across Phase 1)
//!
//! - `kinds` — `NodeKind` enum (commit 1.2) ✓
//! - `edges` — `EdgeKind` enum (commit 1.3) ✓
//! - `edge_struct` — full `Edge` + `EdgeAttributes` + `EdgeSite` (commit 1.12) ✓
//! - `identity` — `NodeId` content-hash scheme (commit 1.4) ✓
//! - `attributes` — `Confidence`, `Source`, `BindingKind` (commit 1.5) ✓
//! - `common` — shared building blocks: `SourceRange`, `Visibility`,
//!   `ModificationHistory` (commit 1.6) ✓
//! - `nodes::containers` — Repository/Directory/Module/File/NonCodeFile (commit 1.7) ✓
//! - `nodes::callables` — Function/Method/Lambda + Callable trait (commit 1.8) ✓
//! - `nodes::types` — Class/Struct/Interface/Trait/Enum/TypeAlias (commit 1.9) ✓
//! - `nodes::sub_symbols` — Field/GlobalVariable/Parameter/Statement/Expression (commit 1.10) ✓
//! - `nodes::non_code` — DocSection/Docstring/Comment/ConfigValue (commit 1.11) ✓
//! - `nodes::unresolved` — UnresolvedSymbol (commit 1.11) ✓
//! - `edges` — full `Edge` struct + per-edge-kind attribute structs
//!   (commit 1.12)
//!
//! Determinism tests live under `tests/determinism.rs` (commit 1.13).

pub mod attributes;
pub mod common;
pub mod edge_struct;
pub mod edges;
pub mod identity;
pub mod kinds;
pub mod nodes;

pub use attributes::{
    ALL_BINDING_KINDS, ALL_SOURCES, BindingKind, Confidence,
    ConfidenceOutOfRange, Source, UnknownBindingKind, UnknownSource,
};
pub use common::{
    ALL_VISIBILITIES, InvalidSourceRange, Modification,
    ModificationHistory, SourceRange, UnknownVisibility, Visibility,
};
pub use edge_struct::{
    ALL_DECISION_STATUSES, ALL_REFERENCE_KIND_HINTS, DecisionStatus, Edge,
    EdgeAttributes, EdgeSite, ReferenceKindHint,
};
pub use edges::{
    ALL_EDGE_KIND_CATEGORIES, ALL_EDGE_KINDS, EdgeKind, EdgeKindCategory,
    UnknownEdgeKind, UnknownEdgeKindCategory,
};
pub use identity::{NodeId, NodeIdConstructionError, NodeIdParseError};
pub use kinds::{
    ALL_NODE_KIND_CATEGORIES, ALL_NODE_KINDS, NodeKind, NodeKindCategory,
    UnknownNodeKind, UnknownNodeKindCategory,
};
pub use nodes::{
    ALL_COMMENT_TAGS, Callable, Class, ClassConstructionError, Comment,
    CommentConstructionError, CommentTag, ConfigValue,
    ConfigValueConstructionError, Directory, DirectoryConstructionError,
    DocSection, DocSectionConstructionError, Docstring,
    DocstringConstructionError, Enum, EnumConstructionError, EnumVariant,
    Expression, ExpressionConstructionError, Field, FieldConstructionError,
    File, FileConstructionError, Function, FunctionConstructionError,
    GlobalVariable, GlobalVariableConstructionError, Interface,
    InterfaceConstructionError, Lambda, LambdaConstructionError, Method,
    MethodConstructionError, Module, ModuleConstructionError, NonCodeFile,
    NonCodeFileConstructionError, NonCodeFormat, Parameter,
    ParameterConstructionError, ParameterSignature, Repository,
    RepositoryConstructionError, Statement, StatementConstructionError,
    Struct, StructConstructionError, Trait, TraitConstructionError,
    TypeAlias, TypeAliasConstructionError, UnresolvedSymbol,
    UnresolvedSymbolConstructionError,
};
