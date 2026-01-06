"""Approval workflow for risky autofixes."""

import uuid
from enum import Enum
from datetime import datetime
from typing import Dict, List, Optional
from pathlib import Path
import structlog

from ..graph.connection_pool import db_pool

logger = structlog.get_logger()


class ApprovalStatus(Enum):
    """Status of an approval request."""

    PENDING = "pending"
    APPROVED = "approved"
    REJECTED = "rejected"
    AUTO_APPROVED = "auto_approved"


class ApprovalWorkflow:
    """Manages approval workflow for risky fixes."""

    def __init__(self, tenant_id: str, repository_id: Optional[str] = None):
        self.tenant_id = tenant_id
        self.repository_id = repository_id
        self._ensure_tables()

    def _ensure_tables(self):
        """Ensure approval tables exist."""
        try:
            db_pool.execute(
                """
                CREATE TABLE IF NOT EXISTS repograph.autofix_approvals (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    tenant_id UUID NOT NULL,
                    repository_id UUID,
                    fix_id VARCHAR(255) NOT NULL,
                    fix_type VARCHAR(100) NOT NULL,
                    risk_level VARCHAR(50) NOT NULL,
                    file_count INTEGER NOT NULL,
                    summary JSONB NOT NULL,
                    status VARCHAR(50) NOT NULL DEFAULT 'pending',
                    requested_by VARCHAR(255),
                    requested_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    reviewed_by VARCHAR(255),
                    reviewed_at TIMESTAMP,
                    review_comment TEXT,
                    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
                )
                """,
                fetch=False,
            )

            # Add index for lookups
            db_pool.execute(
                """
                CREATE INDEX IF NOT EXISTS idx_autofix_approvals_tenant
                ON repograph.autofix_approvals(tenant_id, status)
                """,
                fetch=False,
            )

        except Exception as e:
            logger.warning("Could not create approval tables", error=str(e))

    def request_approval(
        self,
        fix_id: str,
        fix_type: str,
        risk_level: str,
        file_count: int,
        summary: Dict,
        requested_by: Optional[str] = None,
    ) -> str:
        """Request approval for a fix."""
        approval_id = str(uuid.uuid4())

        db_pool.execute(
            """
            INSERT INTO repograph.autofix_approvals (
                id, tenant_id, repository_id, fix_id, fix_type,
                risk_level, file_count, summary, requested_by
            )
            VALUES (%s, %s, %s, %s, %s, %s, %s, %s::jsonb, %s)
            """,
            (
                approval_id,
                self.tenant_id,
                self.repository_id,
                fix_id,
                fix_type,
                risk_level,
                file_count,
                str(summary),
                requested_by,
            ),
            fetch=False,
        )

        logger.info(
            "Approval requested",
            approval_id=approval_id,
            fix_id=fix_id,
            fix_type=fix_type,
            risk_level=risk_level,
        )

        return approval_id

    def approve(
        self,
        approval_id: str,
        reviewed_by: str,
        comment: Optional[str] = None,
    ) -> bool:
        """Approve a fix."""
        try:
            db_pool.execute(
                """
                UPDATE repograph.autofix_approvals
                SET status = %s,
                    reviewed_by = %s,
                    reviewed_at = CURRENT_TIMESTAMP,
                    review_comment = %s,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = %s AND tenant_id = %s AND status = 'pending'
                """,
                (ApprovalStatus.APPROVED.value, reviewed_by, comment, approval_id, self.tenant_id),
                fetch=False,
            )

            logger.info("Approval granted", approval_id=approval_id, reviewed_by=reviewed_by)
            return True

        except Exception as e:
            logger.error("Failed to approve", approval_id=approval_id, error=str(e))
            return False

    def reject(
        self,
        approval_id: str,
        reviewed_by: str,
        comment: Optional[str] = None,
    ) -> bool:
        """Reject a fix."""
        try:
            db_pool.execute(
                """
                UPDATE repograph.autofix_approvals
                SET status = %s,
                    reviewed_by = %s,
                    reviewed_at = CURRENT_TIMESTAMP,
                    review_comment = %s,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = %s AND tenant_id = %s AND status = 'pending'
                """,
                (ApprovalStatus.REJECTED.value, reviewed_by, comment, approval_id, self.tenant_id),
                fetch=False,
            )

            logger.info("Approval rejected", approval_id=approval_id, reviewed_by=reviewed_by)
            return True

        except Exception as e:
            logger.error("Failed to reject", approval_id=approval_id, error=str(e))
            return False

    def auto_approve(self, fix_id: str, reason: str) -> str:
        """Auto-approve a low-risk fix."""
        approval_id = str(uuid.uuid4())

        db_pool.execute(
            """
            INSERT INTO repograph.autofix_approvals (
                id, tenant_id, repository_id, fix_id, fix_type,
                risk_level, file_count, summary, status, review_comment
            )
            VALUES (%s, %s, %s, %s, %s, %s, %s, %s::jsonb, %s, %s)
            """,
            (
                approval_id,
                self.tenant_id,
                self.repository_id,
                fix_id,
                "auto",
                "low",
                0,
                "{}",
                ApprovalStatus.AUTO_APPROVED.value,
                reason,
            ),
            fetch=False,
        )

        logger.info("Auto-approved fix", approval_id=approval_id, fix_id=fix_id, reason=reason)
        return approval_id

    def get_status(self, approval_id: str) -> Optional[Dict]:
        """Get status of an approval request."""
        result = db_pool.execute(
            """
            SELECT id, fix_id, fix_type, risk_level, file_count,
                   status, requested_by, requested_at,
                   reviewed_by, reviewed_at, review_comment
            FROM repograph.autofix_approvals
            WHERE id = %s AND tenant_id = %s
            """,
            (approval_id, self.tenant_id),
        )

        if not result:
            return None

        row = result[0]
        return {
            "id": str(row["id"]),
            "fix_id": row["fix_id"],
            "fix_type": row["fix_type"],
            "risk_level": row["risk_level"],
            "file_count": row["file_count"],
            "status": row["status"],
            "requested_by": row["requested_by"],
            "requested_at": row["requested_at"].isoformat() if row["requested_at"] else None,
            "reviewed_by": row["reviewed_by"],
            "reviewed_at": row["reviewed_at"].isoformat() if row["reviewed_at"] else None,
            "review_comment": row["review_comment"],
        }

    def get_pending_approvals(self, limit: int = 50) -> List[Dict]:
        """Get pending approval requests."""
        results = db_pool.execute(
            """
            SELECT id, fix_id, fix_type, risk_level, file_count,
                   requested_by, requested_at
            FROM repograph.autofix_approvals
            WHERE tenant_id = %s AND status = 'pending'
            ORDER BY requested_at DESC
            LIMIT %s
            """,
            (self.tenant_id, limit),
        )

        return [
            {
                "id": str(row["id"]),
                "fix_id": row["fix_id"],
                "fix_type": row["fix_type"],
                "risk_level": row["risk_level"],
                "file_count": row["file_count"],
                "requested_by": row["requested_by"],
                "requested_at": row["requested_at"].isoformat() if row["requested_at"] else None,
            }
            for row in results
        ]

    def get_approval_history(self, limit: int = 100) -> List[Dict]:
        """Get approval history."""
        results = db_pool.execute(
            """
            SELECT id, fix_id, fix_type, risk_level, file_count,
                   status, requested_by, requested_at,
                   reviewed_by, reviewed_at
            FROM repograph.autofix_approvals
            WHERE tenant_id = %s
            ORDER BY requested_at DESC
            LIMIT %s
            """,
            (self.tenant_id, limit),
        )

        return [
            {
                "id": str(row["id"]),
                "fix_id": row["fix_id"],
                "fix_type": row["fix_type"],
                "risk_level": row["risk_level"],
                "file_count": row["file_count"],
                "status": row["status"],
                "requested_by": row["requested_by"],
                "requested_at": row["requested_at"].isoformat() if row["requested_at"] else None,
                "reviewed_by": row["reviewed_by"],
                "reviewed_at": row["reviewed_at"].isoformat() if row["reviewed_at"] else None,
            }
            for row in results
        ]

    def is_approved(self, approval_id: str) -> bool:
        """Check if an approval request is approved."""
        status = self.get_status(approval_id)
        if not status:
            return False

        return status["status"] in [
            ApprovalStatus.APPROVED.value,
            ApprovalStatus.AUTO_APPROVED.value,
        ]

    def get_audit_trail(self, fix_id: str) -> List[Dict]:
        """Get audit trail for a specific fix."""
        results = db_pool.execute(
            """
            SELECT id, status, requested_by, requested_at,
                   reviewed_by, reviewed_at, review_comment
            FROM repograph.autofix_approvals
            WHERE fix_id = %s AND tenant_id = %s
            ORDER BY requested_at DESC
            """,
            (fix_id, self.tenant_id),
        )

        return [
            {
                "id": str(row["id"]),
                "status": row["status"],
                "requested_by": row["requested_by"],
                "requested_at": row["requested_at"].isoformat() if row["requested_at"] else None,
                "reviewed_by": row["reviewed_by"],
                "reviewed_at": row["reviewed_at"].isoformat() if row["reviewed_at"] else None,
                "review_comment": row["review_comment"],
            }
            for row in results
        ]
