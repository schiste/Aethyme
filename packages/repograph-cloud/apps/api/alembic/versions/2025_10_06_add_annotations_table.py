"""Add code_annotations table for Phase 14.3

Revision ID: add_annotations_table_001
Revises: add_viewport_fields_001
Create Date: 2025-10-06 16:00:00.000000

"""
from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision = 'add_annotations_table_001'
down_revision = 'add_viewport_fields_001'
branch_labels = None
depends_on = None


def upgrade() -> None:
    """
    Create code_annotations table for persistent code highlights.
    """

    # Create code_annotations table
    op.create_table(
        'code_annotations',
        sa.Column('id', postgresql.UUID(as_uuid=True), nullable=False, server_default=sa.text('gen_random_uuid()')),
        sa.Column('user_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('location', sa.String(length=500), nullable=False),
        sa.Column('line_start', sa.Integer(), nullable=False),
        sa.Column('line_end', sa.Integer(), nullable=False),
        sa.Column('column_start', sa.Integer(), nullable=True),
        sa.Column('column_end', sa.Integer(), nullable=True),
        sa.Column('color', sa.String(length=20), nullable=False, server_default='yellow'),
        sa.Column('label', sa.String(length=200), nullable=True),
        sa.Column('description', sa.Text(), nullable=True),
        sa.Column('is_temporary', sa.Boolean(), nullable=False, server_default='false'),
        sa.Column('is_public', sa.Boolean(), nullable=False, server_default='true'),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('expires_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('metadata', postgresql.JSONB(), nullable=False, server_default='{}'),
        sa.PrimaryKeyConstraint('id'),
    )

    # Create indexes
    op.create_index(
        'ix_code_annotations_user_id',
        'code_annotations',
        ['user_id']
    )
    op.create_index(
        'ix_code_annotations_location',
        'code_annotations',
        ['location']
    )
    op.create_index(
        'ix_code_annotations_created_at',
        'code_annotations',
        ['created_at']
    )
    # Composite index for location + line range queries
    op.create_index(
        'ix_code_annotations_location_lines',
        'code_annotations',
        ['location', 'line_start', 'line_end']
    )
    # Partial index for public annotations
    op.create_index(
        'ix_code_annotations_public',
        'code_annotations',
        ['location', 'is_public'],
        postgresql_where=sa.text('is_public = true')
    )
    # Partial index for non-expired annotations
    op.create_index(
        'ix_code_annotations_active',
        'code_annotations',
        ['location'],
        postgresql_where=sa.text('expires_at IS NULL OR expires_at > NOW()')
    )
    # Index for color filtering
    op.create_index(
        'ix_code_annotations_color',
        'code_annotations',
        ['location', 'color']
    )

    print("✅ Created code_annotations table")
    print("✅ Created 7 indexes for annotations")
    print("✅ Phase 14.3 annotations migration complete")


def downgrade() -> None:
    """
    Drop code_annotations table.
    """
    # Drop indexes
    op.drop_index('ix_code_annotations_color', table_name='code_annotations')
    op.drop_index('ix_code_annotations_active', table_name='code_annotations')
    op.drop_index('ix_code_annotations_public', table_name='code_annotations')
    op.drop_index('ix_code_annotations_location_lines', table_name='code_annotations')
    op.drop_index('ix_code_annotations_created_at', table_name='code_annotations')
    op.drop_index('ix_code_annotations_location', table_name='code_annotations')
    op.drop_index('ix_code_annotations_user_id', table_name='code_annotations')

    # Drop table
    op.drop_table('code_annotations')

    print("✅ Dropped code_annotations table")
