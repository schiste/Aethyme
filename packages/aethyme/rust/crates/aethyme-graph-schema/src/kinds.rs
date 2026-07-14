//! The canonical node-kind taxonomy.
//!
//! `NodeKind` is the flat enum of every node type the graph admits. There
//! are 25 variants grouped into six categories (containers, callables,
//! type-defining, sub-symbol, non-code, partial-knowledge). Each variant
//! has a stable snake_case canonical name (used in node IDs, in NDJSON
//! index shards, and in logs).
//!
//! ## Ordering is load-bearing
//!
//! Variant order in this enum determines the bincode discriminant tag.
//! Per the schema doc's "no schema versioning" principle, this order is
//! **forever** — adding a variant must append to the end of its category
//! group, never insert into the middle. Old fragments compiled against
//! an earlier ordering would deserialize to the wrong variant if the
//! discriminants drifted.
//!
//! The ordering rule:
//!
//! 1. Group by category, in this order:
//!    containers → callables → type-defining → sub-symbol →
//!    non-code → partial-knowledge.
//! 2. Within a group, order by the canonical snake_case name's
//!    alphabetical order. This is enforced by `tests/kinds.rs`.
//! 3. New variants append to the end of their group, never insert.
//!    Yes, this means new variants break alphabetical order within
//!    the group — that's the cost of "no schema versioning". The
//!    alphabetical rule applies to the initial set, not to
//!    additions. Tests assert the initial set is alphabetical;
//!    tests do not assert alphabetical order across additions.
//!
//! This rule produces a deterministic, reviewable diff when new kinds are
//! added: the new variant always lands at the tail of its category. A
//! diff that inserts mid-list is a contract violation.

use serde::{Deserialize, Serialize};

/// Every kind of node the graph admits. See module docs for the ordering
/// rule.
///
/// `#[serde(rename_all = "snake_case")]` keeps the serialized form
/// (NDJSON index shards, JSON debug output) aligned with the canonical
/// name returned by [`NodeKind::name`]. The bincode discriminant is
/// determined by variant order in the source; see the module docs.
///
/// `Ord` / `PartialOrd` are derived from variant declaration order — same
/// order as the bincode discriminant. This makes "canonical sort by kind"
/// (used in fragment serialization for determinism) a one-liner, and it
/// keeps the sort order consistent across in-memory and on-disk
/// representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    // ─── Containers (alphabetical) ───────────────────────────────────
    /// A source directory. One per filesystem directory containing tracked
    /// files.
    Directory,
    /// A source code file.
    File,
    /// A logical module (Python package, TS module, PHP namespace). Does
    /// not always map 1:1 to a directory.
    Module,
    /// A non-code file (Markdown, YAML, JSON, TOML, etc.). The
    /// documentation-as-graph entry point. Lives in the container category
    /// (it's a file, structurally) but holds non-code sub-symbol kinds
    /// like `Docstring`, `DocSection`, `ConfigValue`.
    NonCodeFile,
    /// Top of the namespace. One per repo.
    Repository,
    /// A package: a build/distribution unit rooted at a manifest file
    /// (Cargo crate's `Cargo.toml`, npm package's `package.json`,
    /// Python project's `pyproject.toml`, Godot project's
    /// `project.godot`, etc.). Distinct from [`NodeKind::Module`]:
    /// `Module` is a *language-level* construct (Python package, TS
    /// module, PHP namespace); `Package` is a *build/distribution*
    /// unit. One repository can contain many packages (monorepo);
    /// one package can contain many modules.
    ///
    /// Appended to the tail of the Containers group per the
    /// "no schema versioning" ordering rule — see module docs.
    Package,

    // ─── Callables (alphabetical) ────────────────────────────────────
    /// A function. Top-level or nested. Satisfies the `Callable`
    /// protocol (signature, parameters, return type).
    Function,
    /// An anonymous function. Always first-class (vs. folding into the
    /// enclosing scope) so callback-heavy patterns stay traversable.
    /// When the lambda is assigned to a named variable, `assigned_name`
    /// is set; when it's inline-passed, only the positional anchor is
    /// recorded.
    Lambda,
    /// A bound method on a class/struct/trait. Satisfies `Callable` and
    /// additionally requires a receiver type.
    Method,

    // ─── Type-defining (alphabetical) ────────────────────────────────
    /// A class (Python, TS, PHP, Java, C#, Ruby). Distinct from `Struct`
    /// because class semantics (method dispatch, multiple inheritance in
    /// some languages, mutable receivers) differ meaningfully from
    /// struct semantics.
    Class,
    /// An enum/sum type. Variants are inline; not a separate kind.
    Enum,
    /// An interface (TS, Go, Java/C# pre-default-methods). Kept distinct
    /// from `Trait` because interface semantics (no default methods,
    /// no associated types) differ from trait semantics. The two have
    /// converged in some languages but the schema preserves the
    /// distinction for query precision.
    Interface,
    /// A struct (Rust, C, Go). Data-layout-focused, immutable defaults
    /// in some languages.
    Struct,
    /// A trait (Rust). Supports default methods, associated types,
    /// associated constants. Closest neutral analogue to interfaces but
    /// not identical.
    Trait,
    /// A type alias (TS `type =`, Rust `type =`, C++ `using =`).
    TypeAlias,

    // ─── Sub-symbol (alphabetical) ───────────────────────────────────
    /// An expression inside a statement. The schema's deepest first-class
    /// kind. Tier-3 materialization.
    Expression,
    /// A field on a class/struct/trait/interface.
    Field,
    /// A module-level binding (Python global, Rust `static`/`const`, JS
    /// module-level `let`/`const`/`var`).
    GlobalVariable,
    /// A function/method parameter. First-class so that future
    /// dataflow analysis (ARISE-style) can attach DefUse edges
    /// directly to parameter nodes.
    Parameter,
    /// A statement inside a function/method body. Top-level AST
    /// statements only (direct children of the body). Tier-2
    /// materialization — only emitted when an intent asks for
    /// statement-level evidence.
    Statement,

    // ─── Non-code (alphabetical) ─────────────────────────────────────
    /// An inline source comment carrying a recognized tag (TODO,
    /// FIXME, NOTE, DECISION, SEE_ALSO). Untagged comments are stored
    /// as attributes on the enclosing statement, not as nodes — only
    /// load-bearing comments become first-class.
    Comment,
    /// A single addressable config value (e.g. `database.url` inside a
    /// YAML/TOML config file). Granularity is per-key, not per-file,
    /// so `Configures` edges connect specific values to the code that
    /// reads them.
    ConfigValue,
    /// A heading-bounded section of a markdown doc or similar
    /// structured non-code file. Subdivides `NonCodeFile` for finer
    /// `Documents`/`Decides`/`References` edges.
    DocSection,
    /// A docstring attached to a callable/class/module. Distinct from
    /// `Comment` because docstrings have a well-defined target
    /// (the enclosing symbol).
    Docstring,

    // ─── Partial-knowledge ───────────────────────────────────────────
    /// A symbol that's referenced but not located. Used instead of
    /// relaxing required fields on real kinds — preserves the
    /// "required-when-applicable" invariant across the rest of the
    /// schema.
    UnresolvedSymbol,
}

impl NodeKind {
    /// Canonical snake_case name. Stable for ID construction and logs.
    ///
    /// This is the inverse of [`NodeKind::from_name`]. The mapping is
    /// hand-maintained (rather than reflected from the variant ident)
    /// because the canonical name is part of the cross-process contract:
    /// it appears in node IDs and on the wire. Reflecting from
    /// `#[serde(rename_all = "snake_case")]` would couple the wire format
    /// to a serde implementation detail, which is exactly the kind of
    /// hidden cross-process dependency
    /// `docs/architecture/cross-process-consumers.md` exists to track.
    pub const fn name(self) -> &'static str {
        match self {
            // Containers
            NodeKind::Directory => "directory",
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::NonCodeFile => "non_code_file",
            NodeKind::Repository => "repository",
            NodeKind::Package => "package",
            // Callables
            NodeKind::Function => "function",
            NodeKind::Lambda => "lambda",
            NodeKind::Method => "method",
            // Type-defining
            NodeKind::Class => "class",
            NodeKind::Enum => "enum",
            NodeKind::Interface => "interface",
            NodeKind::Struct => "struct",
            NodeKind::Trait => "trait",
            NodeKind::TypeAlias => "type_alias",
            // Sub-symbol
            NodeKind::Expression => "expression",
            NodeKind::Field => "field",
            NodeKind::GlobalVariable => "global_variable",
            NodeKind::Parameter => "parameter",
            NodeKind::Statement => "statement",
            // Non-code
            NodeKind::Comment => "comment",
            NodeKind::ConfigValue => "config_value",
            NodeKind::DocSection => "doc_section",
            NodeKind::Docstring => "docstring",
            // Partial-knowledge
            NodeKind::UnresolvedSymbol => "unresolved_symbol",
        }
    }

    /// Inverse of [`NodeKind::name`]. Returns
    /// [`UnknownNodeKind`] (carrying the rejected string) for unknown
    /// names.
    ///
    /// Returns `Result` rather than `Option` because the primary callers
    /// are at cross-process boundaries (deserializing node IDs from
    /// fragment files, parsing query filter strings). At those
    /// boundaries, the caller needs the rejected string in any error
    /// message they produce; making the API surface that automatically
    /// prevents the `from_name(s).ok_or_else(|| anyhow!("unknown: {s:?}"))`
    /// boilerplate that would otherwise repeat at every callsite.
    ///
    /// Match order mirrors [`NodeKind::name`] for grep-ability.
    pub fn from_name(name: &str) -> Result<NodeKind, UnknownNodeKind> {
        Ok(match name {
            // Containers
            "directory" => NodeKind::Directory,
            "file" => NodeKind::File,
            "module" => NodeKind::Module,
            "non_code_file" => NodeKind::NonCodeFile,
            "repository" => NodeKind::Repository,
            "package" => NodeKind::Package,
            // Callables
            "function" => NodeKind::Function,
            "lambda" => NodeKind::Lambda,
            "method" => NodeKind::Method,
            // Type-defining
            "class" => NodeKind::Class,
            "enum" => NodeKind::Enum,
            "interface" => NodeKind::Interface,
            "struct" => NodeKind::Struct,
            "trait" => NodeKind::Trait,
            "type_alias" => NodeKind::TypeAlias,
            // Sub-symbol
            "expression" => NodeKind::Expression,
            "field" => NodeKind::Field,
            "global_variable" => NodeKind::GlobalVariable,
            "parameter" => NodeKind::Parameter,
            "statement" => NodeKind::Statement,
            // Non-code
            "comment" => NodeKind::Comment,
            "config_value" => NodeKind::ConfigValue,
            "doc_section" => NodeKind::DocSection,
            "docstring" => NodeKind::Docstring,
            // Partial-knowledge
            "unresolved_symbol" => NodeKind::UnresolvedSymbol,
            _ => return Err(UnknownNodeKind { given: name.into() }),
        })
    }

    /// Category this kind belongs to. Used for tier-decision logic in
    /// the indexer (tier-2/tier-3 kinds are sub-symbol kinds) and for
    /// query routing (e.g. "find all callables").
    pub const fn category(self) -> NodeKindCategory {
        match self {
            NodeKind::Directory
            | NodeKind::File
            | NodeKind::Module
            | NodeKind::NonCodeFile
            | NodeKind::Repository
            | NodeKind::Package => NodeKindCategory::Container,

            NodeKind::Function | NodeKind::Lambda | NodeKind::Method => NodeKindCategory::Callable,

            NodeKind::Class
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::TypeAlias => NodeKindCategory::TypeDefining,

            NodeKind::Expression
            | NodeKind::Field
            | NodeKind::GlobalVariable
            | NodeKind::Parameter
            | NodeKind::Statement => NodeKindCategory::SubSymbol,

            NodeKind::Comment
            | NodeKind::ConfigValue
            | NodeKind::DocSection
            | NodeKind::Docstring => NodeKindCategory::NonCode,

            NodeKind::UnresolvedSymbol => NodeKindCategory::PartialKnowledge,
        }
    }
}

/// Categories of node kinds, used for tier-decision and query routing.
///
/// Categories are part of the schema's stable surface: they appear in
/// derived facts (`is_callable`, `is_type_definition`) and in query
/// filters. Adding a new category is a contract change; reordering is
/// a contract change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKindCategory {
    Container,
    Callable,
    TypeDefining,
    SubSymbol,
    NonCode,
    PartialKnowledge,
}

impl NodeKindCategory {
    /// Canonical snake_case name. Mirrors [`NodeKind::name`] in shape and
    /// rationale: provides a string form that doesn't require pulling
    /// `serde_json` into production code paths just to log a category.
    pub const fn name(self) -> &'static str {
        match self {
            NodeKindCategory::Container => "container",
            NodeKindCategory::Callable => "callable",
            NodeKindCategory::TypeDefining => "type_defining",
            NodeKindCategory::SubSymbol => "sub_symbol",
            NodeKindCategory::NonCode => "non_code",
            NodeKindCategory::PartialKnowledge => "partial_knowledge",
        }
    }

    /// Inverse of [`NodeKindCategory::name`]. Mirrors
    /// [`NodeKind::from_name`]'s `Result` shape so that consumers see a
    /// uniform error-handling pattern across the two enums.
    pub fn from_name(name: &str) -> Result<NodeKindCategory, UnknownNodeKindCategory> {
        Ok(match name {
            "container" => NodeKindCategory::Container,
            "callable" => NodeKindCategory::Callable,
            "type_defining" => NodeKindCategory::TypeDefining,
            "sub_symbol" => NodeKindCategory::SubSymbol,
            "non_code" => NodeKindCategory::NonCode,
            "partial_knowledge" => NodeKindCategory::PartialKnowledge,
            _ => {
                return Err(UnknownNodeKindCategory { given: name.into() });
            }
        })
    }
}

/// Every variant in declaration order. Used by tests and any consumer
/// that needs to iterate the full kind set (e.g., schema validation,
/// docs generation).
///
/// Order matches the enum source order — which, per the module docs,
/// is also the bincode discriminant order. Tests assert this.
pub const ALL_NODE_KINDS: &[NodeKind] = &[
    // Containers (alphabetical)
    NodeKind::Directory,
    NodeKind::File,
    NodeKind::Module,
    NodeKind::NonCodeFile,
    NodeKind::Repository,
    NodeKind::Package,
    // Callables (alphabetical)
    NodeKind::Function,
    NodeKind::Lambda,
    NodeKind::Method,
    // Type-defining (alphabetical)
    NodeKind::Class,
    NodeKind::Enum,
    NodeKind::Interface,
    NodeKind::Struct,
    NodeKind::Trait,
    NodeKind::TypeAlias,
    // Sub-symbol (alphabetical)
    NodeKind::Expression,
    NodeKind::Field,
    NodeKind::GlobalVariable,
    NodeKind::Parameter,
    NodeKind::Statement,
    // Non-code (alphabetical)
    NodeKind::Comment,
    NodeKind::ConfigValue,
    NodeKind::DocSection,
    NodeKind::Docstring,
    // Partial-knowledge
    NodeKind::UnresolvedSymbol,
];

/// Every category in declaration order. Parallel to [`ALL_NODE_KINDS`]:
/// gives consumers a canonical iteration handle that does not depend on
/// reflecting the enum. Order is the same as the source enum, which
/// matches the bincode discriminant order — keep in sync.
pub const ALL_NODE_KIND_CATEGORIES: &[NodeKindCategory] = &[
    NodeKindCategory::Container,
    NodeKindCategory::Callable,
    NodeKindCategory::TypeDefining,
    NodeKindCategory::SubSymbol,
    NodeKindCategory::NonCode,
    NodeKindCategory::PartialKnowledge,
];

/// Returned by [`NodeKind::from_name`] when the given string does not
/// match any known node kind. Carries the rejected string so the caller
/// can produce informative diagnostics without re-passing the input.
///
/// The field is `Box<str>` rather than `String` for memory compactness
/// (16 bytes vs. 24 on 64-bit). Construction is cheap because the input
/// is typically a borrowed `&str` from an outer parse loop; we own a
/// copy here so the error can outlive the input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownNodeKind {
    given: Box<str>,
}

impl UnknownNodeKind {
    /// The string the caller passed that did not match any node kind.
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownNodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown node kind: {:?}", self.given)
    }
}

impl std::error::Error for UnknownNodeKind {}

/// Returned by [`NodeKindCategory::from_name`] when the given string
/// does not match any known category. Mirrors [`UnknownNodeKind`] in
/// shape; see that type's docs for the rationale.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownNodeKindCategory {
    given: Box<str>,
}

impl UnknownNodeKindCategory {
    /// The string the caller passed that did not match any category.
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownNodeKindCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown node kind category: {:?}", self.given)
    }
}

impl std::error::Error for UnknownNodeKindCategory {}
