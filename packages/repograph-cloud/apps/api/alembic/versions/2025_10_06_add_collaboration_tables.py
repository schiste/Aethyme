"""add_collaboration_tables

Revision ID: 2025_10_06_collab
Revises: c3972e10aae3
Create Date: 2025-10-06 12:33:42.000000

"""
from typing import Sequence, Union
from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision: str = '2025_10_06_collab'
down_revision: Union[str, None] = 'c3972e10aae3'
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    """Create collaboration tables for real-time features."""
    
    # Create collaboration_sessions table
    op.create_table(
        'collaboration_sessions',
        sa.Column('id', postgresql.UUID(as_uuid=True), nullable=False, server_default=sa.text('gen_random_uuid()')),
        sa.Column('location', sa.String(length=500), nullable=False),
        sa.Column('participant_count', sa.Integer(), nullable=False, server_default='0'),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('last_activity', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('metadata', postgresql.JSONB(astext_type=sa.Text()), nullable=True, server_default='{}'),
        sa.PrimaryKeyConstraint('id'),
    )
    
    # Create indexes for collaboration_sessions
    op.create_index('ix_collaboration_sessions_location', 'collaboration_sessions', ['location'])
    op.create_index('ix_collaboration_sessions_last_activity', 'collaboration_sessions', ['last_activity'])
    
    # Create user_presence table
    op.create_table(
        'user_presence',
        sa.Column('id', postgresql.UUID(as_uuid=True), nullable=False, server_default=sa.text('gen_random_uuid()')),
        sa.Column('user_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('session_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('status', sa.String(length=20), nullable=False, server_default='online'),
        sa.Column('location', sa.String(length=500), nullable=False),
        sa.Column('cursor_line', sa.Integer(), nullable=True),
        sa.Column('cursor_column', sa.Integer(), nullable=True),
        sa.Column('selection_start_line', sa.Integer(), nullable=True),
        sa.Column('selection_start_column', sa.Integer(), nullable=True),
        sa.Column('selection_end_line', sa.Integer(), nullable=True),
        sa.Column('selection_end_column', sa.Integer(), nullable=True),
        sa.Column('last_seen', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('joined_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('metadata', postgresql.JSONB(astext_type=sa.Text()), nullable=True, server_default='{}'),
        sa.PrimaryKeyConstraint('id'),
        sa.ForeignKeyConstraint(['session_id'], ['collaboration_sessions.id'], ondelete='CASCADE'),
    )
    
    # Create indexes for user_presence
    op.create_index('ix_user_presence_user_id', 'user_presence', ['user_id'])
    op.create_index('ix_user_presence_session_id', 'user_presence', ['session_id'])
    op.create_index('ix_user_presence_status', 'user_presence', ['status'])
    op.create_index('ix_user_presence_location', 'user_presence', ['location'])
    op.create_index('ix_user_presence_last_seen', 'user_presence', ['last_seen'])
    
    # Create composite indexes for common queries
    op.create_index('ix_user_presence_user_location', 'user_presence', ['user_id', 'location'])
    op.create_index('ix_user_presence_location_status', 'user_presence', ['location', 'status'])
    
    # Create activity_events table
    op.create_table(
        'activity_events',
        sa.Column('id', postgresql.UUID(as_uuid=True), nullable=False, server_default=sa.text('gen_random_uuid()')),
        sa.Column('user_id', postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column('event_type', sa.String(length=50), nullable=False),
        sa.Column('location', sa.String(length=500), nullable=True),
        sa.Column('payload', postgresql.JSONB(astext_type=sa.Text()), nullable=True, server_default='{}'),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.PrimaryKeyConstraint('id'),
    )
    
    # Create indexes for activity_events
    op.create_index('ix_activity_events_user_id', 'activity_events', ['user_id'])
    op.create_index('ix_activity_events_event_type', 'activity_events', ['event_type'])
    op.create_index('ix_activity_events_location', 'activity_events', ['location'])
    op.create_index('ix_activity_events_created_at', 'activity_events', ['created_at'], postgresql_using='btree')
    
    # Create composite indexes for common queries
    op.create_index('ix_activity_events_user_created', 'activity_events', ['user_id', 'created_at'])
    op.create_index('ix_activity_events_location_created', 'activity_events', ['location', 'created_at'])


def downgrade() -> None:
    """Drop collaboration tables."""
    
    # Drop activity_events table and indexes
    op.drop_index('ix_activity_events_location_created', table_name='activity_events')
    op.drop_index('ix_activity_events_user_created', table_name='activity_events')
    op.drop_index('ix_activity_events_created_at', table_name='activity_events')
    op.drop_index('ix_activity_events_location', table_name='activity_events')
    op.drop_index('ix_activity_events_event_type', table_name='activity_events')
    op.drop_index('ix_activity_events_user_id', table_name='activity_events')
    op.drop_table('activity_events')
    
    # Drop user_presence table and indexes
    op.drop_index('ix_user_presence_location_status', table_name='user_presence')
    op.drop_index('ix_user_presence_user_location', table_name='user_presence')
    op.drop_index('ix_user_presence_last_seen', table_name='user_presence')
    op.drop_index('ix_user_presence_location', table_name='user_presence')
    op.drop_index('ix_user_presence_status', table_name='user_presence')
    op.drop_index('ix_user_presence_session_id', table_name='user_presence')
    op.drop_index('ix_user_presence_user_id', table_name='user_presence')
    op.drop_table('user_presence')
    
    # Drop collaboration_sessions table and indexes
    op.drop_index('ix_collaboration_sessions_last_activity', table_name='collaboration_sessions')
    op.drop_index('ix_collaboration_sessions_location', table_name='collaboration_sessions')
    op.drop_table('collaboration_sessions')
