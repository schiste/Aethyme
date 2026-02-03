"""add_repository_indexing_fields

Revision ID: e6a352d76653
Revises: 6f9c7b610cb5
Create Date: 2025-10-03 11:41:31.462335

"""
from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql


# revision identifiers, used by Alembic.
revision: str = 'e6a352d76653'
down_revision: Union[str, None] = '6f9c7b610cb5'
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    # Add new fields to repositories table
    op.add_column('repositories', sa.Column('github_id', sa.String(255), nullable=True))
    op.add_column('repositories', sa.Column('clone_url', sa.String(512), nullable=True))
    op.add_column('repositories', sa.Column('stars', sa.Integer(), nullable=False, server_default='0'))
    op.add_column('repositories', sa.Column('forks', sa.Integer(), nullable=False, server_default='0'))
    op.add_column('repositories', sa.Column('total_files', sa.Integer(), nullable=False, server_default='0'))
    op.add_column('repositories', sa.Column('total_lines', sa.Integer(), nullable=False, server_default='0'))
    op.add_column('repositories', sa.Column('languages', postgresql.JSONB(astext_type=sa.Text()), nullable=True))
    op.add_column('repositories', sa.Column('indexed_at', sa.DateTime(), nullable=True))
    op.add_column('repositories', sa.Column('primary_language', sa.String(100), nullable=True))

    # Add index on github_id for faster lookups
    op.create_index('ix_repositories_github_id', 'repositories', ['github_id'])


def downgrade() -> None:
    # Remove index
    op.drop_index('ix_repositories_github_id', table_name='repositories')

    # Remove columns
    op.drop_column('repositories', 'primary_language')
    op.drop_column('repositories', 'indexed_at')
    op.drop_column('repositories', 'languages')
    op.drop_column('repositories', 'total_lines')
    op.drop_column('repositories', 'total_files')
    op.drop_column('repositories', 'forks')
    op.drop_column('repositories', 'stars')
    op.drop_column('repositories', 'clone_url')
    op.drop_column('repositories', 'github_id')
