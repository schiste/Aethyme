"""Add pgvector extension and code embeddings table

Revision ID: add_pgvector_embeddings
Revises: add_ai_credentials
Create Date: 2025-10-04 12:30:00.000000

"""
from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa
from pgvector.sqlalchemy import Vector


# revision identifiers, used by Alembic.
revision: str = 'add_pgvector_embeddings'
down_revision: Union[str, None] = 'add_ai_credentials'
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    # Enable pgvector extension
    op.execute('CREATE EXTENSION IF NOT EXISTS vector')

    # Create code_embeddings table
    op.create_table(
        'code_embeddings',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('repository_id', sa.String(length=100), nullable=False),
        sa.Column('file_path', sa.String(length=500), nullable=False),
        sa.Column('symbol_id', sa.String(length=500), nullable=False),
        sa.Column('symbol_name', sa.String(length=200), nullable=False),
        sa.Column('symbol_kind', sa.String(length=50), nullable=False),
        sa.Column('language', sa.String(length=50), nullable=False),
        sa.Column('signature', sa.Text(), nullable=True),
        sa.Column('documentation', sa.Text(), nullable=True),
        sa.Column('code_snippet', sa.Text(), nullable=True),
        sa.Column('embedding', Vector(1536), nullable=True),
        sa.Column('embedding_model', sa.String(length=100), nullable=True),
        sa.Column('embedding_provider', sa.String(length=50), nullable=True),
        sa.Column('embedding_created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=True),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=True),
        sa.PrimaryKeyConstraint('id')
    )

    # Create indexes
    op.create_index(op.f('ix_code_embeddings_id'), 'code_embeddings', ['id'], unique=False)
    op.create_index(op.f('ix_code_embeddings_repository_id'), 'code_embeddings', ['repository_id'], unique=False)
    op.create_index(op.f('ix_code_embeddings_symbol_id'), 'code_embeddings', ['symbol_id'], unique=False)
    op.create_index('ix_code_embeddings_repository_file', 'code_embeddings', ['repository_id', 'file_path'], unique=False)
    op.create_index('ix_code_embeddings_symbol_kind', 'code_embeddings', ['symbol_kind'], unique=False)
    op.create_index('ix_code_embeddings_language', 'code_embeddings', ['language'], unique=False)

    # Create vector similarity index using IVFFlat
    # IVFFlat is faster than default but requires training on data
    # For now, we'll use the default HNSW index which works without training
    # To use IVFFlat: op.execute('CREATE INDEX ON code_embeddings USING ivfflat (embedding vector_cosine_ops) WITH (lists = 100)')
    op.execute('CREATE INDEX ix_code_embeddings_embedding ON code_embeddings USING hnsw (embedding vector_cosine_ops)')


def downgrade() -> None:
    op.drop_index('ix_code_embeddings_embedding', table_name='code_embeddings')
    op.drop_index('ix_code_embeddings_language', table_name='code_embeddings')
    op.drop_index('ix_code_embeddings_symbol_kind', table_name='code_embeddings')
    op.drop_index('ix_code_embeddings_repository_file', table_name='code_embeddings')
    op.drop_index(op.f('ix_code_embeddings_symbol_id'), table_name='code_embeddings')
    op.drop_index(op.f('ix_code_embeddings_repository_id'), table_name='code_embeddings')
    op.drop_index(op.f('ix_code_embeddings_id'), table_name='code_embeddings')
    op.drop_table('code_embeddings')

    # Drop pgvector extension
    op.execute('DROP EXTENSION IF EXISTS vector')
