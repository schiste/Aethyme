"""Merge graph models and pgvector branches

Revision ID: c3972e10aae3
Revises: 2a8f9c5e4d3b, add_pgvector_embeddings
Create Date: 2025-10-06 09:39:05.207088

"""
from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa


# revision identifiers, used by Alembic.
revision: str = 'c3972e10aae3'
down_revision: Union[str, None] = ('2a8f9c5e4d3b', 'add_pgvector_embeddings')
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    pass


def downgrade() -> None:
    pass
