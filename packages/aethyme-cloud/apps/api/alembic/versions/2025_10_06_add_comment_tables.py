"""Add comment tables for Phase 14 collaboration

Revision ID: add_comment_tables_001
Revises: add_collaboration_tables
Create Date: 2025-10-06 14:00:00.000000

"""
from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision = 'add_comment_tables_001'
down_revision = 'add_collaboration_tables'
branch_labels = None
depends_on = None


def upgrade() -> None:
    """
    Create tables for code comments with threading and mentions.
    """

    # Create collaboration_comments table
    op.create_table(
        'collaboration_comments',
        sa.Column('id', postgresql.UUID(as_uuid=True), nullable=False, server_default=sa.text('gen_random_uuid()')),
        sa.Column('user_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('location', sa.String(length=500), nullable=False),
        sa.Column('line_start', sa.Integer(), nullable=False),
        sa.Column('line_end', sa.Integer(), nullable=True),
        sa.Column('column_start', sa.Integer(), nullable=True),
        sa.Column('column_end', sa.Integer(), nullable=True),
        sa.Column('content', sa.Text(), nullable=False),
        sa.Column('parent_id', postgresql.UUID(as_uuid=True), nullable=True),
        sa.Column('resolved', sa.Boolean(), nullable=False, server_default='false'),
        sa.Column('resolved_by', postgresql.UUID(as_uuid=True), nullable=True),
        sa.Column('resolved_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('metadata', postgresql.JSONB(), nullable=False, server_default='{}'),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.PrimaryKeyConstraint('id'),
        sa.ForeignKeyConstraint(['parent_id'], ['collaboration_comments.id'], ondelete='CASCADE'),
    )

    # Create indexes for collaboration_comments
    op.create_index(
        'ix_collaboration_comments_user_id',
        'collaboration_comments',
        ['user_id']
    )
    op.create_index(
        'ix_collaboration_comments_location',
        'collaboration_comments',
        ['location']
    )
    op.create_index(
        'ix_collaboration_comments_resolved',
        'collaboration_comments',
        ['resolved']
    )
    op.create_index(
        'ix_collaboration_comments_created_at',
        'collaboration_comments',
        ['created_at']
    )
    # Composite index for location + line range queries
    op.create_index(
        'ix_collaboration_comments_location_lines',
        'collaboration_comments',
        ['location', 'line_start', 'line_end']
    )
    # Index for unresolved comments by location
    op.create_index(
        'ix_collaboration_comments_location_unresolved',
        'collaboration_comments',
        ['location', 'resolved'],
        postgresql_where=sa.text('resolved = false')
    )

    # Create collaboration_mentions table
    op.create_table(
        'collaboration_mentions',
        sa.Column('id', postgresql.UUID(as_uuid=True), nullable=False, server_default=sa.text('gen_random_uuid()')),
        sa.Column('comment_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('mentioned_user_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('notified', sa.Boolean(), nullable=False, server_default='false'),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.PrimaryKeyConstraint('id'),
        sa.ForeignKeyConstraint(['comment_id'], ['collaboration_comments.id'], ondelete='CASCADE'),
    )

    # Create indexes for collaboration_mentions
    op.create_index(
        'ix_collaboration_mentions_comment_id',
        'collaboration_mentions',
        ['comment_id']
    )
    op.create_index(
        'ix_collaboration_mentions_mentioned_user_id',
        'collaboration_mentions',
        ['mentioned_user_id']
    )
    op.create_index(
        'ix_collaboration_mentions_notified',
        'collaboration_mentions',
        ['notified']
    )
    # Composite index for unread mentions by user
    op.create_index(
        'ix_collaboration_mentions_user_unread',
        'collaboration_mentions',
        ['mentioned_user_id', 'notified'],
        postgresql_where=sa.text('notified = false')
    )
    # Unique constraint to prevent duplicate mentions
    op.create_index(
        'uq_collaboration_mentions_comment_user',
        'collaboration_mentions',
        ['comment_id', 'mentioned_user_id'],
        unique=True
    )

    print("✅ Created collaboration_comments table with 6 indexes")
    print("✅ Created collaboration_mentions table with 5 indexes")
    print("✅ Phase 14 comment tables migration complete")


def downgrade() -> None:
    """
    Drop comment tables.
    """
    # Drop indexes first
    op.drop_index('uq_collaboration_mentions_comment_user', table_name='collaboration_mentions')
    op.drop_index('ix_collaboration_mentions_user_unread', table_name='collaboration_mentions')
    op.drop_index('ix_collaboration_mentions_notified', table_name='collaboration_mentions')
    op.drop_index('ix_collaboration_mentions_mentioned_user_id', table_name='collaboration_mentions')
    op.drop_index('ix_collaboration_mentions_comment_id', table_name='collaboration_mentions')

    op.drop_index('ix_collaboration_comments_location_unresolved', table_name='collaboration_comments')
    op.drop_index('ix_collaboration_comments_location_lines', table_name='collaboration_comments')
    op.drop_index('ix_collaboration_comments_created_at', table_name='collaboration_comments')
    op.drop_index('ix_collaboration_comments_resolved', table_name='collaboration_comments')
    op.drop_index('ix_collaboration_comments_location', table_name='collaboration_comments')
    op.drop_index('ix_collaboration_comments_user_id', table_name='collaboration_comments')

    # Drop tables
    op.drop_table('collaboration_mentions')
    op.drop_table('collaboration_comments')

    print("✅ Dropped collaboration comment tables")
