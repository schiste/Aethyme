"""Patch generator for autofixers - Creates and applies diffs."""

import difflib
import subprocess
from enum import Enum
from pathlib import Path
from typing import Dict, List, Optional
from datetime import datetime
import structlog

from .safety import SafetyEngine, RiskLevel

logger = structlog.get_logger()


class PatchMode(Enum):
    """Mode for patch operations."""

    DRY_RUN = "dry_run"  # Show changes without applying
    APPLY = "apply"  # Apply changes to disk
    PR = "pr"  # Create pull request


class FilePatch:
    """Represents a patch for a single file."""

    def __init__(
        self,
        file_path: Path,
        original_content: str,
        new_content: str,
        fix_type: str,
        risk_level: RiskLevel,
    ):
        self.file_path = file_path
        self.original_content = original_content
        self.new_content = new_content
        self.fix_type = fix_type
        self.risk_level = risk_level
        self.validation = None

    def generate_diff(self) -> str:
        """Generate unified diff for this file."""
        original_lines = self.original_content.splitlines(keepends=True)
        new_lines = self.new_content.splitlines(keepends=True)

        diff = difflib.unified_diff(
            original_lines,
            new_lines,
            fromfile=f"a/{self.file_path}",
            tofile=f"b/{self.file_path}",
            lineterm="",
        )

        return "".join(diff)

    def get_summary(self) -> Dict[str, any]:
        """Get summary of changes."""
        orig_lines = self.original_content.splitlines()
        new_lines = self.new_content.splitlines()

        return {
            "file": str(self.file_path),
            "fix_type": self.fix_type,
            "risk_level": self.risk_level.value,
            "lines_added": len(new_lines) - len(orig_lines),
            "size_change": len(self.new_content) - len(self.original_content),
            "has_changes": self.original_content != self.new_content,
        }

    def apply(self) -> bool:
        """Apply this patch to disk."""
        try:
            with open(self.file_path, 'w', encoding='utf-8') as f:
                f.write(self.new_content)
            logger.info("Applied patch", file=str(self.file_path), fix_type=self.fix_type)
            return True
        except Exception as e:
            logger.error("Failed to apply patch", file=str(self.file_path), error=str(e))
            return False


class PatchGenerator:
    """Generates and manages patches for autofixes."""

    def __init__(self, repo_path: Path, safety_engine: Optional[SafetyEngine] = None):
        self.repo_path = Path(repo_path)
        self.safety_engine = safety_engine or SafetyEngine()
        self.patches: List[FilePatch] = []

    def add_patch(
        self,
        file_path: Path,
        original_content: str,
        new_content: str,
        fix_type: str,
    ) -> Optional[FilePatch]:
        """Add a patch for a file."""
        # Make path relative to repo
        if file_path.is_absolute():
            try:
                file_path = file_path.relative_to(self.repo_path)
            except ValueError:
                # File not in repo
                logger.warning("File outside repo", file=str(file_path))
                return None

        # Skip if no changes
        if original_content == new_content:
            logger.debug("No changes to file", file=str(file_path))
            return None

        # Assess risk
        full_path = self.repo_path / file_path
        try:
            risk_level = self.safety_engine.assess_risk(full_path, fix_type)
        except ValueError as e:
            logger.error("Cannot patch file", file=str(file_path), error=str(e))
            return None

        # Validate changes
        validation = self.safety_engine.validate_changes(
            original_content, new_content, full_path
        )

        if not validation["safe"]:
            logger.warning(
                "Unsafe changes detected",
                file=str(file_path),
                warnings=validation["warnings"],
            )
            # Still create patch but mark as requiring approval
            if risk_level == RiskLevel.LOW:
                risk_level = RiskLevel.MEDIUM

        patch = FilePatch(file_path, original_content, new_content, fix_type, risk_level)
        patch.validation = validation
        self.patches.append(patch)

        logger.info(
            "Created patch",
            file=str(file_path),
            fix_type=fix_type,
            risk=risk_level.value,
            lines_added=validation["stats"]["lines_added"],
        )

        return patch

    def generate_unified_diff(self) -> str:
        """Generate unified diff for all patches."""
        if not self.patches:
            return ""

        diffs = []
        for patch in self.patches:
            diff = patch.generate_diff()
            if diff:
                diffs.append(diff)

        return "\n".join(diffs)

    def get_summary(self) -> Dict[str, any]:
        """Get summary of all patches."""
        by_risk = {
            RiskLevel.LOW: [],
            RiskLevel.MEDIUM: [],
            RiskLevel.HIGH: [],
        }

        by_fix_type = {}

        for patch in self.patches:
            by_risk[patch.risk_level].append(patch)
            if patch.fix_type not in by_fix_type:
                by_fix_type[patch.fix_type] = []
            by_fix_type[patch.fix_type].append(patch)

        return {
            "total_files": len(self.patches),
            "total_low_risk": len(by_risk[RiskLevel.LOW]),
            "total_medium_risk": len(by_risk[RiskLevel.MEDIUM]),
            "total_high_risk": len(by_risk[RiskLevel.HIGH]),
            "by_fix_type": {k: len(v) for k, v in by_fix_type.items()},
            "requires_approval": len(by_risk[RiskLevel.MEDIUM]) + len(by_risk[RiskLevel.HIGH]),
        }

    def dry_run(self) -> Dict[str, any]:
        """Show what would be changed without applying."""
        summary = self.get_summary()
        diff = self.generate_unified_diff()

        return {
            "mode": "dry_run",
            "summary": summary,
            "diff": diff,
            "patches": [p.get_summary() for p in self.patches],
        }

    def apply(self, skip_approval: bool = False) -> Dict[str, any]:
        """Apply all patches."""
        if not skip_approval:
            # Check if any patches require approval
            requires_approval = [
                p for p in self.patches
                if p.risk_level in [RiskLevel.MEDIUM, RiskLevel.HIGH]
            ]
            if requires_approval:
                return {
                    "mode": "apply",
                    "status": "requires_approval",
                    "message": f"{len(requires_approval)} patches require approval",
                    "requires_approval": [p.get_summary() for p in requires_approval],
                }

        # Apply all patches
        applied = []
        failed = []

        for patch in self.patches:
            full_path = self.repo_path / patch.file_path
            if patch.apply():
                applied.append(str(patch.file_path))
            else:
                failed.append(str(patch.file_path))

        logger.info(
            "Applied patches",
            total=len(self.patches),
            applied=len(applied),
            failed=len(failed),
        )

        return {
            "mode": "apply",
            "status": "success" if not failed else "partial",
            "applied": applied,
            "failed": failed,
            "summary": self.get_summary(),
        }

    def create_commit_message(self) -> str:
        """Generate commit message for changes."""
        summary = self.get_summary()

        lines = ["fix: apply autofixes", ""]

        # Group by fix type
        for fix_type, count in summary["by_fix_type"].items():
            lines.append(f"- {fix_type}: {count} files")

        lines.extend([
            "",
            f"Total files modified: {summary['total_files']}",
            f"Risk levels: {summary['total_low_risk']} low, "
            f"{summary['total_medium_risk']} medium, {summary['total_high_risk']} high",
            "",
            "Generated with RepoGraph Autofixer",
        ])

        return "\n".join(lines)

    def save_patch_file(self, output_path: Path) -> Path:
        """Save unified diff to a patch file."""
        diff = self.generate_unified_diff()

        with open(output_path, 'w', encoding='utf-8') as f:
            f.write(diff)

        logger.info("Saved patch file", path=str(output_path), size=len(diff))
        return output_path

    def get_changed_files(self) -> List[Path]:
        """Get list of files that would be changed."""
        return [self.repo_path / p.file_path for p in self.patches]

    def rollback(self) -> Dict[str, any]:
        """Rollback changes (requires git)."""
        try:
            changed_files = self.get_changed_files()
            if not changed_files:
                return {"status": "no_changes", "message": "No files to rollback"}

            # Use git checkout to restore original files
            for file_path in changed_files:
                subprocess.run(
                    ["git", "checkout", "HEAD", "--", str(file_path)],
                    cwd=self.repo_path,
                    check=True,
                    capture_output=True,
                )

            logger.info("Rolled back changes", files=len(changed_files))
            return {
                "status": "success",
                "message": f"Rolled back {len(changed_files)} files",
                "files": [str(f) for f in changed_files],
            }

        except subprocess.CalledProcessError as e:
            logger.error("Rollback failed", error=str(e))
            return {
                "status": "error",
                "message": f"Rollback failed: {e}",
            }
