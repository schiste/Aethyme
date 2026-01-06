"""Add code graph models for graph analysis

Revision ID: 2a8f9c5e4d3b
Revises: 1caf4ba7b127
Create Date: 2025-10-05 14:30:00.000000

"""
from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision: str = '2a8f9c5e4d3b'
down_revision: Union[str, None] = '1caf4ba7b127'
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    """Create code_relationships and symbol_metadata tables for graph analysis."""

    # Create enum for relationship types
    relationship_type_enum = postgresql.ENUM(
        'calls', 'called_by',
        'inherits', 'inherited_by',
        'implements', 'implemented_by',
        'imports', 'imported_by',
        'references', 'referenced_by',
        'contains', 'contained_in',
        name='relationshiptype',
        create_type=False
    )

    # Check if enum type already exists, create if not
    conn = op.get_bind()
    result = conn.execute(sa.text(
        "SELECT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'relationshiptype')"
    ))
    exists = result.scalar()

    if not exists:
        relationship_type_enum.create(conn, checkfirst=True)

    # Create code_relationships table
    op.create_table(
        'code_relationships',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('repository_id', sa.String(length=100), nullable=False),
        sa.Column('source_symbol_id', sa.String(length=500), nullable=False),
        sa.Column('source_file_path', sa.String(length=500), nullable=False),
        sa.Column('source_name', sa.String(length=200), nullable=False),
        sa.Column('source_kind', sa.String(length=50), nullable=False),
        sa.Column('target_symbol_id', sa.String(length=500), nullable=False),
        sa.Column('target_file_path', sa.String(length=500), nullable=False),
        sa.Column('target_name', sa.String(length=200), nullable=False),
        sa.Column('target_kind', sa.String(length=50), nullable=False),
        sa.Column('relationship_type', relationship_type_enum, nullable=False),
        sa.Column('line_number', sa.Integer(), nullable=True),
        sa.Column('column_number', sa.Integer(), nullable=True),
        sa.Column('language', sa.String(length=50), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=True),
        sa.PrimaryKeyConstraint('id')
    )

    # Create indexes for code_relationships
    op.create_index('ix_relationships_source', 'code_relationships', ['source_symbol_id', 'relationship_type'])
    op.create_index('ix_relationships_target', 'code_relationships', ['target_symbol_id', 'relationship_type'])
    op.create_index('ix_relationships_repo', 'code_relationships', ['repository_id', 'relationship_type'])
    op.create_index('ix_relationships_file', 'code_relationships', ['repository_id', 'source_file_path'])
    op.create_index(
        'ix_relationships_unique',
        'code_relationships',
        ['source_symbol_id', 'target_symbol_id', 'relationship_type'],
        unique=True
    )
    op.create_index('ix_code_relationships_repository_id', 'code_relationships', ['repository_id'])
    op.create_index('ix_code_relationships_source_symbol_id', 'code_relationships', ['source_symbol_id'])
    op.create_index('ix_code_relationships_target_symbol_id', 'code_relationships', ['target_symbol_id'])

    # Create symbol_metadata table
    op.create_table(
        'symbol_metadata',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('symbol_id', sa.String(length=500), nullable=False),
        sa.Column('repository_id', sa.String(length=100), nullable=False),
        sa.Column('inbound_call_count', sa.Integer(), nullable=True, server_default='0'),
        sa.Column('outbound_call_count', sa.Integer(), nullable=True, server_default='0'),
        sa.Column('dependency_count', sa.Integer(), nullable=True, server_default='0'),
        sa.Column('dependent_count', sa.Integer(), nullable=True, server_default='0'),
        sa.Column('cyclomatic_complexity', sa.Integer(), nullable=True),
        sa.Column('lines_of_code', sa.Integer(), nullable=True),
        sa.Column('impact_score', sa.Integer(), nullable=True, server_default='0'),
        sa.Column('change_frequency', sa.Integer(), nullable=True, server_default='0'),
        sa.Column('last_analyzed_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=True),
        sa.PrimaryKeyConstraint('id'),
        sa.UniqueConstraint('symbol_id')
    )

    # Create indexes for symbol_metadata
    op.create_index('ix_symbol_metadata_repo', 'symbol_metadata', ['repository_id'])
    op.create_index('ix_symbol_metadata_impact', 'symbol_metadata', ['impact_score'])
    op.create_index('ix_symbol_metadata_symbol_id', 'symbol_metadata', ['symbol_id'], unique=True)


def downgrade() -> None:
    """Drop code_relationships and symbol_metadata tables."""

    # Drop indexes for symbol_metadata
    op.drop_index('ix_symbol_metadata_symbol_id', table_name='symbol_metadata')
    op.drop_index('ix_symbol_metadata_impact', table_name='symbol_metadata')
    op.drop_index('ix_symbol_metadata_repo', table_name='symbol_metadata')

    # Drop symbol_metadata table
    op.drop_table('symbol_metadata')

    # Drop indexes for code_relationships
    op.drop_index('ix_code_relationships_target_symbol_id', table_name='code_relationships')
    op.drop_index('ix_code_relationships_source_symbol_id', table_name='code_relationships')
    op.drop_index('ix_code_relationships_repository_id', table_name='code_relationships')
    op.drop_index('ix_relationships_unique', table_name='code_relationships')
    op.drop_index('ix_relationships_file', table_name='code_relationships')
    op.drop_index('ix_relationships_repo', table_name='code_relationships')
    op.drop_index('ix_relationships_target', table_name='code_relationships')
    op.drop_index('ix_relationships_source', table_name='code_relationships')

    # Drop code_relationships table
    op.drop_table('code_relationships')

    # Drop enum type
    relationship_type_enum = postgresql.ENUM(
        'calls', 'called_by',
        'inherits', 'inherited_by',
        'implements', 'implemented_by',
        'imports', 'imported_by',
        'references', 'referenced_by',
        'contains', 'contained_in',
        name='relationshiptype'
    )
    relationship_type_enum.drop(op.get_bind())
