"""Add AI credentials tables for BYOK

Revision ID: add_ai_credentials
Revises: 9292b9bdd8cf
Create Date: 2025-10-04 12:00:00.000000

"""
from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa


# revision identifiers, used by Alembic.
revision: str = 'add_ai_credentials'
down_revision: Union[str, None] = '9292b9bdd8cf'
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    # Create ai_credentials table
    op.create_table(
        'ai_credentials',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('user_id', sa.String(36), nullable=False),
        sa.Column('provider_type', sa.String(length=50), nullable=False),
        sa.Column('provider_name', sa.String(length=100), nullable=True),
        sa.Column('encrypted_credentials', sa.Text(), nullable=False),
        sa.Column('is_active', sa.Boolean(), nullable=True, default=True),
        sa.Column('is_validated', sa.Boolean(), nullable=True, default=False),
        sa.Column('last_validated_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('validation_error', sa.Text(), nullable=True),
        sa.Column('last_used_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('total_requests', sa.Integer(), nullable=True, default=0),
        sa.Column('total_tokens_used', sa.Integer(), nullable=True, default=0),
        sa.Column('created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=True),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_ai_credentials_id'), 'ai_credentials', ['id'], unique=False)
    op.create_index('ix_ai_credentials_user_provider', 'ai_credentials', ['user_id', 'provider_type'], unique=False)
    op.create_index('ix_ai_credentials_is_active', 'ai_credentials', ['is_active'], unique=False)

    # Create ai_usage_logs table
    op.create_table(
        'ai_usage_logs',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('user_id', sa.String(36), nullable=False),
        sa.Column('credential_id', sa.Integer(), nullable=True),
        sa.Column('provider_type', sa.String(length=50), nullable=False),
        sa.Column('operation_type', sa.String(length=50), nullable=False),
        sa.Column('model', sa.String(length=100), nullable=False),
        sa.Column('tokens_used', sa.Integer(), nullable=False),
        sa.Column('request_count', sa.Integer(), nullable=True, default=1),
        sa.Column('estimated_cost', sa.String(length=20), nullable=True),
        sa.Column('repository_id', sa.String(length=100), nullable=True),
        sa.Column('session_id', sa.String(length=100), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.ForeignKeyConstraint(['credential_id'], ['ai_credentials.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_ai_usage_logs_id'), 'ai_usage_logs', ['id'], unique=False)
    op.create_index('ix_ai_usage_logs_user_created', 'ai_usage_logs', ['user_id', 'created_at'], unique=False)
    op.create_index('ix_ai_usage_logs_credential', 'ai_usage_logs', ['credential_id'], unique=False)
    op.create_index('ix_ai_usage_logs_repository', 'ai_usage_logs', ['repository_id'], unique=False)

    # Create ai_provider_quotas table
    op.create_table(
        'ai_provider_quotas',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('user_id', sa.String(36), nullable=False),
        sa.Column('max_tokens_per_month', sa.Integer(), nullable=True),
        sa.Column('max_requests_per_month', sa.Integer(), nullable=True),
        sa.Column('max_cost_per_month', sa.String(length=20), nullable=True),
        sa.Column('current_tokens', sa.Integer(), nullable=True, default=0),
        sa.Column('current_requests', sa.Integer(), nullable=True, default=0),
        sa.Column('current_cost', sa.String(length=20), nullable=True, default='0.00'),
        sa.Column('quota_period_start', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=True),
        sa.Column('quota_period_end', sa.DateTime(timezone=True), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), server_default=sa.text('now()'), nullable=True),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_ai_provider_quotas_id'), 'ai_provider_quotas', ['id'], unique=False)
    op.create_index('ix_ai_quotas_user', 'ai_provider_quotas', ['user_id'], unique=True)


def downgrade() -> None:
    op.drop_index('ix_ai_quotas_user', table_name='ai_provider_quotas')
    op.drop_index(op.f('ix_ai_provider_quotas_id'), table_name='ai_provider_quotas')
    op.drop_table('ai_provider_quotas')

    op.drop_index('ix_ai_usage_logs_repository', table_name='ai_usage_logs')
    op.drop_index('ix_ai_usage_logs_credential', table_name='ai_usage_logs')
    op.drop_index('ix_ai_usage_logs_user_created', table_name='ai_usage_logs')
    op.drop_index(op.f('ix_ai_usage_logs_id'), table_name='ai_usage_logs')
    op.drop_table('ai_usage_logs')

    op.drop_index('ix_ai_credentials_is_active', table_name='ai_credentials')
    op.drop_index('ix_ai_credentials_user_provider', table_name='ai_credentials')
    op.drop_index(op.f('ix_ai_credentials_id'), table_name='ai_credentials')
    op.drop_table('ai_credentials')
