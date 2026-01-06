"""Tests for approval workflow."""

import pytest
from src.autofixers.approval import ApprovalWorkflow, ApprovalStatus


class TestApprovalWorkflow:
    """Tests for approval workflow."""

    @pytest.fixture
    def workflow(self):
        """Create approval workflow for testing."""
        return ApprovalWorkflow(tenant_id="test-tenant-123")

    def test_request_approval(self, workflow):
        """Should create approval request."""
        approval_id = workflow.request_approval(
            fix_id="fix-123",
            fix_type="selector_insert",
            risk_level="medium",
            file_count=5,
            summary={"total_files": 5},
            requested_by="test@example.com",
        )

        assert approval_id is not None

        # Check status
        status = workflow.get_status(approval_id)
        assert status is not None
        assert status["status"] == "pending"
        assert status["fix_id"] == "fix-123"

    def test_approve_fix(self, workflow):
        """Should approve a pending fix."""
        # Create approval
        approval_id = workflow.request_approval(
            fix_id="fix-456",
            fix_type="format_fix",
            risk_level="low",
            file_count=3,
            summary={},
            requested_by="requester@example.com",
        )

        # Approve
        success = workflow.approve(
            approval_id,
            reviewed_by="reviewer@example.com",
            comment="Looks good",
        )

        assert success

        # Check status
        status = workflow.get_status(approval_id)
        assert status["status"] == "approved"
        assert status["reviewed_by"] == "reviewer@example.com"
        assert status["review_comment"] == "Looks good"

    def test_reject_fix(self, workflow):
        """Should reject a pending fix."""
        # Create approval
        approval_id = workflow.request_approval(
            fix_id="fix-789",
            fix_type="i18n_scaffold",
            risk_level="high",
            file_count=10,
            summary={},
        )

        # Reject
        success = workflow.reject(
            approval_id,
            reviewed_by="reviewer@example.com",
            comment="Not ready",
        )

        assert success

        # Check status
        status = workflow.get_status(approval_id)
        assert status["status"] == "rejected"

    def test_auto_approve(self, workflow):
        """Should auto-approve low-risk fixes."""
        approval_id = workflow.auto_approve(
            fix_id="fix-auto",
            reason="Low risk documentation update",
        )

        assert approval_id is not None

        status = workflow.get_status(approval_id)
        assert status["status"] == "auto_approved"

    def test_is_approved(self, workflow):
        """Should check if fix is approved."""
        # Create and approve
        approval_id = workflow.request_approval(
            fix_id="fix-check",
            fix_type="docs_regen",
            risk_level="low",
            file_count=1,
            summary={},
        )

        # Not approved yet
        assert not workflow.is_approved(approval_id)

        # Approve
        workflow.approve(approval_id, "reviewer@example.com")

        # Now approved
        assert workflow.is_approved(approval_id)

    def test_get_pending_approvals(self, workflow):
        """Should list pending approvals."""
        # Create several approvals
        workflow.request_approval(
            "fix-1", "docs", "low", 1, {}, "user@example.com"
        )
        workflow.request_approval(
            "fix-2", "links", "medium", 2, {}, "user@example.com"
        )

        pending = workflow.get_pending_approvals()

        assert len(pending) >= 2
        assert all(a["status"] == "pending" for a in pending)

    def test_get_approval_history(self, workflow):
        """Should get approval history."""
        # Create and approve one
        approval_id = workflow.request_approval(
            "fix-history", "format", "low", 1, {}
        )
        workflow.approve(approval_id, "reviewer@example.com")

        history = workflow.get_approval_history()

        assert len(history) >= 1
        assert any(h["fix_id"] == "fix-history" for h in history)

    def test_get_audit_trail(self, workflow):
        """Should get audit trail for a fix."""
        # Create approval
        workflow.request_approval(
            "fix-audit", "selectors", "medium", 3, {}
        )

        trail = workflow.get_audit_trail("fix-audit")

        assert len(trail) >= 1
        assert trail[0]["status"] == "pending"

    def test_cannot_approve_twice(self, workflow):
        """Should not approve already-approved fix."""
        approval_id = workflow.request_approval(
            "fix-twice", "docs", "low", 1, {}
        )

        # First approval succeeds
        workflow.approve(approval_id, "reviewer1@example.com")

        # Second approval should not change status
        # (Implementation may vary - this tests the behavior)
        status_before = workflow.get_status(approval_id)["status"]

        workflow.approve(approval_id, "reviewer2@example.com")

        status_after = workflow.get_status(approval_id)["status"]

        # Status should remain approved
        assert status_after == "approved"
