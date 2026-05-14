//! Type-defining node kinds: Class, Struct, Interface, Trait, Enum, TypeAlias.
//!
//! Required fields per schema doc §3.3. Four kinds (Class, Struct,
//! Interface, Trait) share an identical field set; Enum adds
//! `variants[]`, TypeAlias adds `target_type`. The fields are
//! duplicated across structs rather than abstracted (forever-schema
//! bias toward explicit over clever).

use serde::{Deserialize, Serialize};

use crate::common::{SourceRange, Visibility};
use crate::{NodeId, NodeKind};

/// Shared shape for the basic type-defining kinds.
macro_rules! basic_type_defining_struct {
    ($name:ident, $kind:expr, $err:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name {
            id: NodeId,
            name: Box<str>,
            source_range: SourceRange,
            visibility: Visibility,
        }

        impl $name {
            pub fn new(
                repo: &str,
                file_path: &str,
                name: &str,
                source_range: SourceRange,
                visibility: Visibility,
            ) -> Result<Self, $err> {
                if name.is_empty() {
                    return Err($err::EmptyName);
                }
                if file_path.is_empty() {
                    return Err($err::EmptyFilePath);
                }
                let id = NodeId::new($kind, repo, file_path, name)
                    .map_err($err::Id)?;
                Ok(Self {
                    id,
                    name: name.into(),
                    source_range,
                    visibility,
                })
            }

            pub fn id(&self) -> &NodeId {
                &self.id
            }
            pub fn name(&self) -> &str {
                &self.name
            }
            pub fn source_range(&self) -> SourceRange {
                self.source_range
            }
            pub fn visibility(&self) -> Visibility {
                self.visibility
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $err {
            EmptyName,
            EmptyFilePath,
            Id(crate::NodeIdConstructionError),
        }

        impl std::fmt::Display for $err {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::EmptyName => write!(
                        f,
                        "{}: name must not be empty",
                        stringify!($name)
                    ),
                    Self::EmptyFilePath => write!(
                        f,
                        "{}: file_path must not be empty",
                        stringify!($name)
                    ),
                    Self::Id(e) => write!(
                        f,
                        "{}: ID construction failed: {e}",
                        stringify!($name)
                    ),
                }
            }
        }

        impl std::error::Error for $err {}
    };
}

basic_type_defining_struct!(Class, NodeKind::Class, ClassConstructionError);
basic_type_defining_struct!(Struct, NodeKind::Struct, StructConstructionError);
basic_type_defining_struct!(
    Interface,
    NodeKind::Interface,
    InterfaceConstructionError
);
basic_type_defining_struct!(Trait, NodeKind::Trait, TraitConstructionError);

// ─── Enum (variants[] additional) ────────────────────────────────────

/// One variant of an enum. Variants are inline (per schema doc §3.3
/// "variants are inline; not a separate kind").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnumVariant {
    pub name: Box<str>,
    /// Optional discriminant or payload representation as source
    /// text. Kept as a string because variant payloads are arbitrary
    /// expressions in most languages.
    pub payload: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Enum {
    id: NodeId,
    name: Box<str>,
    variants: Vec<EnumVariant>,
    source_range: SourceRange,
    visibility: Visibility,
}

impl Enum {
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        variants: Vec<EnumVariant>,
        source_range: SourceRange,
        visibility: Visibility,
    ) -> Result<Self, EnumConstructionError> {
        if name.is_empty() {
            return Err(EnumConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(EnumConstructionError::EmptyFilePath);
        }
        let id = NodeId::new(NodeKind::Enum, repo, file_path, name)
            .map_err(EnumConstructionError::Id)?;
        Ok(Enum {
            id,
            name: name.into(),
            variants,
            source_range,
            visibility,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn variants(&self) -> &[EnumVariant] {
        &self.variants
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
    pub fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for EnumConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Enum: name must not be empty"),
            Self::EmptyFilePath => {
                f.write_str("Enum: file_path must not be empty")
            }
            Self::Id(e) => write!(f, "Enum: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for EnumConstructionError {}

// ─── TypeAlias (target_type additional) ──────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypeAlias {
    id: NodeId,
    name: Box<str>,
    target_type: Box<str>,
    source_range: SourceRange,
    visibility: Visibility,
}

impl TypeAlias {
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        target_type: &str,
        source_range: SourceRange,
        visibility: Visibility,
    ) -> Result<Self, TypeAliasConstructionError> {
        if name.is_empty() {
            return Err(TypeAliasConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(TypeAliasConstructionError::EmptyFilePath);
        }
        if target_type.is_empty() {
            return Err(TypeAliasConstructionError::EmptyTargetType);
        }
        let id = NodeId::new(NodeKind::TypeAlias, repo, file_path, name)
            .map_err(TypeAliasConstructionError::Id)?;
        Ok(TypeAlias {
            id,
            name: name.into(),
            target_type: target_type.into(),
            source_range,
            visibility,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn target_type(&self) -> &str {
        &self.target_type
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
    pub fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeAliasConstructionError {
    EmptyName,
    EmptyFilePath,
    EmptyTargetType,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for TypeAliasConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => {
                f.write_str("TypeAlias: name must not be empty")
            }
            Self::EmptyFilePath => {
                f.write_str("TypeAlias: file_path must not be empty")
            }
            Self::EmptyTargetType => {
                f.write_str("TypeAlias: target_type must not be empty")
            }
            Self::Id(e) => {
                write!(f, "TypeAlias: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for TypeAliasConstructionError {}
