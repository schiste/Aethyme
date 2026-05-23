//! The unifying `Node` enum.
//!
//! Each of the 25 node kinds has its own struct (commits 1.7-1.11).
//! The `Node` enum unifies them under one type so callers that
//! handle "any node" (storage layer, graph traversal, query
//! infrastructure) can do so without 24-way trait-object pointers.
//!
//! Uses serde's default (externally tagged) representation:
//! `{"function": {...}}`. This is bincode-compatible. Index shards
//! (NDJSON) don't carry full Node payloads — they carry the lighter
//! `SymbolRecord` form that the storage crate will define.

use serde::{Deserialize, Serialize};

use crate::{
    Class, Comment, ConfigValue, Directory, DocSection, Docstring, Enum,
    Expression, Field, File, Function, GlobalVariable, Interface, Lambda,
    Method, Module, NodeId, NodeKind, NonCodeFile, Package, Parameter,
    Repository, Statement, Struct, Trait, TypeAlias, UnresolvedSymbol,
};

/// A node of any kind. Every variant wraps the corresponding
/// per-kind struct.
///
/// Variant order matches `ALL_NODE_KINDS` exactly so the bincode
/// discriminant for `Node::X(...)` aligns with `NodeKind::X`'s
/// discriminant. This is checked by tests/node_enum.rs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    // Containers (alphabetical initial set; Package tail-appended)
    Directory(Directory),
    File(File),
    Module(Module),
    NonCodeFile(NonCodeFile),
    Repository(Repository),
    Package(Package),
    // Callables (alphabetical)
    Function(Function),
    Lambda(Lambda),
    Method(Method),
    // Type-defining (alphabetical)
    Class(Class),
    Enum(Enum),
    Interface(Interface),
    Struct(Struct),
    Trait(Trait),
    TypeAlias(TypeAlias),
    // Sub-symbol (alphabetical)
    Expression(Expression),
    Field(Field),
    GlobalVariable(GlobalVariable),
    Parameter(Parameter),
    Statement(Statement),
    // Non-code (alphabetical)
    Comment(Comment),
    ConfigValue(ConfigValue),
    DocSection(DocSection),
    Docstring(Docstring),
    // Partial-knowledge
    UnresolvedSymbol(UnresolvedSymbol),
}

impl Node {
    /// The canonical [`NodeKind`] for this variant. Mirrors
    /// [`Edge::kind`] in spirit: the kind is derived from the
    /// variant, never stored alongside it.
    pub const fn kind(&self) -> NodeKind {
        match self {
            Node::Directory(_) => NodeKind::Directory,
            Node::File(_) => NodeKind::File,
            Node::Module(_) => NodeKind::Module,
            Node::NonCodeFile(_) => NodeKind::NonCodeFile,
            Node::Repository(_) => NodeKind::Repository,
            Node::Package(_) => NodeKind::Package,
            Node::Function(_) => NodeKind::Function,
            Node::Lambda(_) => NodeKind::Lambda,
            Node::Method(_) => NodeKind::Method,
            Node::Class(_) => NodeKind::Class,
            Node::Enum(_) => NodeKind::Enum,
            Node::Interface(_) => NodeKind::Interface,
            Node::Struct(_) => NodeKind::Struct,
            Node::Trait(_) => NodeKind::Trait,
            Node::TypeAlias(_) => NodeKind::TypeAlias,
            Node::Expression(_) => NodeKind::Expression,
            Node::Field(_) => NodeKind::Field,
            Node::GlobalVariable(_) => NodeKind::GlobalVariable,
            Node::Parameter(_) => NodeKind::Parameter,
            Node::Statement(_) => NodeKind::Statement,
            Node::Comment(_) => NodeKind::Comment,
            Node::ConfigValue(_) => NodeKind::ConfigValue,
            Node::DocSection(_) => NodeKind::DocSection,
            Node::Docstring(_) => NodeKind::Docstring,
            Node::UnresolvedSymbol(_) => NodeKind::UnresolvedSymbol,
        }
    }

    /// The node's display name, if it has one. Returns `None` for
    /// kinds that don't carry an addressable name (Directory,
    /// File, NonCodeFile, Statement, Expression, etc. — they're
    /// either identified by path or by anonymous position).
    ///
    /// Used by the storage layer's index-shard builder to emit
    /// `SymbolRecord`s keyed by name. A `None` here means "don't
    /// emit a record for this node."
    pub fn name(&self) -> Option<&str> {
        use crate::Callable;
        match self {
            // Container kinds: identified by path, not name.
            Node::Directory(_) => None,
            Node::File(_) => None,
            Node::NonCodeFile(_) => None,
            Node::Module(n) => Some(n.name()),
            Node::Repository(n) => Some(n.name()),
            Node::Package(n) => Some(n.name()),
            // Callables: name() via the Callable trait.
            Node::Function(n) => Some(n.name()),
            Node::Lambda(n) => Some(n.name()),
            Node::Method(n) => Some(n.name()),
            // Type-defining kinds: all named.
            Node::Class(n) => Some(n.name()),
            Node::Enum(n) => Some(n.name()),
            Node::Interface(n) => Some(n.name()),
            Node::Struct(n) => Some(n.name()),
            Node::Trait(n) => Some(n.name()),
            Node::TypeAlias(n) => Some(n.name()),
            // Sub-symbol kinds:
            Node::Expression(_) => None, // anonymous, position-based
            Node::Statement(_) => None,  // anonymous, position-based
            Node::Field(n) => Some(n.name()),
            Node::GlobalVariable(n) => Some(n.name()),
            Node::Parameter(n) => Some(n.name()),
            // Non-code kinds:
            Node::Comment(_) => None,    // position-based
            Node::DocSection(n) => Some(n.heading()),
            Node::Docstring(_) => None,  // attached to a target
            Node::ConfigValue(n) => Some(n.config_path()),
            // Partial-knowledge:
            Node::UnresolvedSymbol(n) => Some(n.name()),
        }
    }

    /// The node's identifier, regardless of kind.
    pub fn id(&self) -> &NodeId {
        // Callable trait is brought in scope so we can call .id()
        // uniformly on Function/Method/Lambda (which only expose
        // `id` via the Callable trait, not as an inherent method).
        use crate::Callable;
        match self {
            Node::Directory(n) => n.id(),
            Node::File(n) => n.id(),
            Node::Module(n) => n.id(),
            Node::NonCodeFile(n) => n.id(),
            Node::Repository(n) => n.id(),
            Node::Package(n) => n.id(),
            Node::Function(n) => n.id(),
            Node::Lambda(n) => n.id(),
            Node::Method(n) => n.id(),
            Node::Class(n) => n.id(),
            Node::Enum(n) => n.id(),
            Node::Interface(n) => n.id(),
            Node::Struct(n) => n.id(),
            Node::Trait(n) => n.id(),
            Node::TypeAlias(n) => n.id(),
            Node::Expression(n) => n.id(),
            Node::Field(n) => n.id(),
            Node::GlobalVariable(n) => n.id(),
            Node::Parameter(n) => n.id(),
            Node::Statement(n) => n.id(),
            Node::Comment(n) => n.id(),
            Node::ConfigValue(n) => n.id(),
            Node::DocSection(n) => n.id(),
            Node::Docstring(n) => n.id(),
            Node::UnresolvedSymbol(n) => n.id(),
        }
    }
}

// Conversions from each per-kind struct into Node. Saves callers
// from writing `Node::Function(f)` everywhere.
macro_rules! impl_node_from {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for Node {
            fn from(value: $ty) -> Self {
                Node::$variant(value)
            }
        }
    };
}

impl_node_from!(Directory, Directory);
impl_node_from!(File, File);
impl_node_from!(Module, Module);
impl_node_from!(NonCodeFile, NonCodeFile);
impl_node_from!(Repository, Repository);
impl_node_from!(Package, Package);
impl_node_from!(Function, Function);
impl_node_from!(Lambda, Lambda);
impl_node_from!(Method, Method);
impl_node_from!(Class, Class);
impl_node_from!(Enum, Enum);
impl_node_from!(Interface, Interface);
impl_node_from!(Struct, Struct);
impl_node_from!(Trait, Trait);
impl_node_from!(TypeAlias, TypeAlias);
impl_node_from!(Expression, Expression);
impl_node_from!(Field, Field);
impl_node_from!(GlobalVariable, GlobalVariable);
impl_node_from!(Parameter, Parameter);
impl_node_from!(Statement, Statement);
impl_node_from!(Comment, Comment);
impl_node_from!(ConfigValue, ConfigValue);
impl_node_from!(DocSection, DocSection);
impl_node_from!(Docstring, Docstring);
impl_node_from!(UnresolvedSymbol, UnresolvedSymbol);
