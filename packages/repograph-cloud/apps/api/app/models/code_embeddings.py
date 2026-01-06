"""
Code Embeddings Model

Stores vector embeddings for semantic code search using pgvector.
"""

from sqlalchemy import Column, String, Integer, DateTime, ForeignKey, Text, Index
from sqlalchemy.sql import func
from pgvector.sqlalchemy import Vector

from app.core.database import Base


class CodeEmbedding(Base):
    """
    Stores code embeddings for semantic search.

    Each embedding represents a code symbol (function, class, method) with:
    - Vector embedding from customer's AI provider (OpenAI, etc.)
    - Metadata about the code symbol
    - Reference to original symbol in Elasticsearch
    """

    __tablename__ = "code_embeddings"

    id = Column(Integer, primary_key=True, index=True)

    # Repository and file info
    repository_id = Column(String(100), nullable=False, index=True)
    file_path = Column(String(500), nullable=False)

    # Symbol information (matches SCIP/Elasticsearch)
    symbol_id = Column(String(500), nullable=False, index=True)  # SCIP symbol ID
    symbol_name = Column(String(200), nullable=False)
    symbol_kind = Column(String(50), nullable=False)  # function, class, method, etc.
    language = Column(String(50), nullable=False)

    # Code content
    signature = Column(Text)  # Function/method signature
    documentation = Column(Text)  # Docstring
    code_snippet = Column(Text)  # First 500 chars of code

    # Vector embedding
    # Dimension depends on model:
    # - OpenAI text-embedding-3-small: 1536
    # - OpenAI text-embedding-3-large: 3072
    # - Cohere embed-v3: 1024
    # We'll use 1536 as default (most common)
    embedding = Column(Vector(1536))

    # Embedding metadata
    embedding_model = Column(String(100))  # e.g., "text-embedding-3-small"
    embedding_provider = Column(String(50))  # e.g., "openai", "cohere"
    embedding_created_at = Column(DateTime(timezone=True), server_default=func.now())

    # Timestamps
    created_at = Column(DateTime(timezone=True), server_default=func.now())
    updated_at = Column(DateTime(timezone=True), onupdate=func.now())

    # Indexes
    __table_args__ = (
        Index("ix_code_embeddings_repository_file", "repository_id", "file_path"),
        Index("ix_code_embeddings_symbol_kind", "symbol_kind"),
        Index("ix_code_embeddings_language", "language"),
        # Vector similarity index (IVFFlat for faster searches)
        # Created separately in migration
    )
