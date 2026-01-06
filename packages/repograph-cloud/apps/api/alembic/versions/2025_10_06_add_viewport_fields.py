"""Add viewport fields for Phase 14.2 follow mode

Revision ID: add_viewport_fields_001
Revises: add_comment_tables_001
Create Date: 2025-10-06 15:00:00.000000

"""
from alembic import op
import sqlalchemy as sa

# revision identifiers, used by Alembic.
revision = 'add_viewport_fields_001'
down_revision = 'add_comment_tables_001'
branch_labels = None
depends_on = None


def upgrade() -> None:
    """
    Add viewport tracking fields to user_presence table for follow mode.
    """

    # Add viewport columns
    op.add_column(
        'user_presence',
        sa.Column('viewport_start_line', sa.Integer(), nullable=True)
    )
    op.add_column(
        'user_presence',
        sa.Column('viewport_end_line', sa.Integer(), nullable=True)
    )
    op.add_column(
        'user_presence',
        sa.Column('viewport_scroll_top', sa.Integer(), nullable=True)
    )

    # Create composite index for viewport queries
    op.create_index(
        'ix_user_presence_viewport',
        'user_presence',
        ['location', 'viewport_start_line', 'viewport_end_line']
    )

    print("✅ Added viewport_start_line, viewport_end_line, viewport_scroll_top to user_presence")
    print("✅ Created composite index for viewport queries")
    print("✅ Phase 14.2 viewport fields migration complete")


def downgrade() -> None:
    """
    Remove viewport fields from user_presence table.
    """

    # Drop index
    op.drop_index('ix_user_presence_viewport', table_name='user_presence')

    # Drop columns
    op.drop_column('user_presence', 'viewport_scroll_top')
    op.drop_column('user_presence', 'viewport_end_line')
    op.drop_column('user_presence', 'viewport_start_line')

    print("✅ Removed viewport fields from user_presence")
