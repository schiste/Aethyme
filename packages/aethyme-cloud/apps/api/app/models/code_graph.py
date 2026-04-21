"""
Code Graph Models

Stores code relationships for graph analysis:
- Function calls (caller -> callee)
- Class inheritance (child -> parent)
- Import dependencies (importer -> imported)
- Type references
"""

from sqlalchemy import Column, String, Integer, DateTime, Index, Enum as SQLEnum
from sqlalchemy.sql import func
import enum

from app.core.database import Base


class RelationshipType(str, enum.Enum):
    """Types of code relationships."""

    # Function relationships
    CALLS = "calls"  # Function A calls Function B
    CALLED_BY = "called_by"  # Function A is called by Function B

    # Class relationships
    INHERITS = "inherits"  # Class A inherits from Class B
    INHERITED_BY = "inherited_by"  # Class A is inherited by Class B
    IMPLEMENTS = "implements"  # Class A implements Interface B
    IMPLEMENTED_BY = "implemented_by"  # Interface A is implemented by Class B

    # Import relationships
    IMPORTS = "imports"  # File A imports from File B
    IMPORTED_BY = "imported_by"  # File A is imported by File B

    # Type relationships
    REFERENCES = "references"  # Symbol A references Type B
    REFERENCED_BY = "referenced_by"  # Type A is referenced by Symbol B

    # Container relationships
    CONTAINS = "contains"  # Class/Module contains Symbol
    CONTAINED_IN = "contained_in"  # Symbol is contained in Class/Module


class CodeRelationship(Base):
    """
    Represents a relationship between two code symbols.

    This is a directed edge in the code graph:
    - source_symbol_id → target_symbol_id
    - relationship_type determines the edge type

    Examples:
    - authenticate() CALLS validate_token()
    - UserService INHERITS BaseService
    - auth.py IMPORTS jwt from library
    """

    __tablename__ = "code_relationships"

    id = Column(Integer, primary_key=True, index=True)

    # Repository context
    repository_id = Column(String(100), nullable=False, index=True)

    # Source symbol (the "from" in the relationship)
    source_symbol_id = Column(String(500), nullable=False, index=True)
    source_file_path = Column(String(500), nullable=False)
    source_name = Column(String(200), nullable=False)
    source_kind = Column(String(50), nullable=False)  # function, class, etc.

    # Target symbol (the "to" in the relationship)
    target_symbol_id = Column(String(500), nullable=False, index=True)
    target_file_path = Column(String(500), nullable=False)
    target_name = Column(String(200), nullable=False)
    target_kind = Column(String(50), nullable=False)

    # Relationship metadata
    relationship_type = Column(
        SQLEnum(RelationshipType, values_callable=lambda x: [e.value for e in x]),
        nullable=False,
        index=True
    )

    # Position where relationship occurs (for "CALLS", this is the call site)
    line_number = Column(Integer)
    column_number = Column(Integer)

    # Language context
    language = Column(String(50), nullable=False)

    # Timestamps
    created_at = Column(DateTime(timezone=True), server_default=func.now())
    updated_at = Column(DateTime(timezone=True), onupdate=func.now())

    # Indexes for efficient graph queries
    __table_args__ = (
        # Find all outgoing edges from a symbol
        Index("ix_relationships_source", "source_symbol_id", "relationship_type"),

        # Find all incoming edges to a symbol
        Index("ix_relationships_target", "target_symbol_id", "relationship_type"),

        # Find relationships in a repository
        Index("ix_relationships_repo", "repository_id", "relationship_type"),

        # Find relationships in a file
        Index("ix_relationships_file", "repository_id", "source_file_path"),

        # Prevent duplicate relationships
        Index(
            "ix_relationships_unique",
            "source_symbol_id",
            "target_symbol_id",
            "relationship_type",
            unique=True
        ),
    )


class SymbolMetadata(Base):
    """
    Extended metadata for code symbols (complements SCIP data).

    Stores computed graph metrics and analysis results:
    - Complexity metrics
    - Dependency counts
    - Impact scores
    """

    __tablename__ = "symbol_metadata"

    id = Column(Integer, primary_key=True, index=True)

    # Symbol identification
    symbol_id = Column(String(500), unique=True, nullable=False, index=True)
    repository_id = Column(String(100), nullable=False, index=True)

    # Graph metrics
    inbound_call_count = Column(Integer, default=0)  # How many callers
    outbound_call_count = Column(Integer, default=0)  # How many callees
    dependency_count = Column(Integer, default=0)  # Total dependencies
    dependent_count = Column(Integer, default=0)  # How many depend on this

    # Complexity metrics
    cyclomatic_complexity = Column(Integer)  # Code complexity
    lines_of_code = Column(Integer)  # LOC

    # Impact analysis
    impact_score = Column(Integer, default=0)  # 0-100, how critical is this symbol
    change_frequency = Column(Integer, default=0)  # How often does it change

    # Last analysis
    last_analyzed_at = Column(DateTime(timezone=True))

    # Timestamps
    created_at = Column(DateTime(timezone=True), server_default=func.now())
    updated_at = Column(DateTime(timezone=True), onupdate=func.now())

    __table_args__ = (
        Index("ix_symbol_metadata_repo", "repository_id"),
        Index("ix_symbol_metadata_impact", "impact_score"),
    )
