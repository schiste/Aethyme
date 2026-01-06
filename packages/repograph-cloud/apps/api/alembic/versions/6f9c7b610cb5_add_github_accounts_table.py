"""Add GitHub accounts table

Revision ID: 6f9c7b610cb5
Revises: c365b2a133aa
Create Date: 2025-10-03 10:46:00.289102

"""
from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa


# revision identifiers, used by Alembic.
revision: str = '6f9c7b610cb5'
down_revision: Union[str, None] = 'c365b2a133aa'
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    # Create github_accounts table
    op.create_table(
        'github_accounts',
        sa.Column('id', sa.String(36), primary_key=True),
        sa.Column('user_id', sa.String(36), sa.ForeignKey('users.id', ondelete='CASCADE'), nullable=False, index=True),
        sa.Column('github_user_id', sa.String(255), unique=True, nullable=False, index=True),
        sa.Column('github_username', sa.String(255), nullable=False),
        sa.Column('github_email', sa.String(255), nullable=True),
        sa.Column('access_token_encrypted', sa.Text(), nullable=False),
        sa.Column('refresh_token_encrypted', sa.Text(), nullable=True),
        sa.Column('token_expires_at', sa.DateTime(), nullable=True),
        sa.Column('scopes', sa.Text(), nullable=True),
        sa.Column('avatar_url', sa.Text(), nullable=True),
        sa.Column('created_at', sa.DateTime(), server_default=sa.func.now(), nullable=False),
        sa.Column('updated_at', sa.DateTime(), server_default=sa.func.now(), onupdate=sa.func.now(), nullable=False),
    )


def downgrade() -> None:
    # Drop github_accounts table
    op.drop_table('github_accounts')
