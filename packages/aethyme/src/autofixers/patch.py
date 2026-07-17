"""Patch generator for autofixers."""

from __future__ import annotations

import difflib
from enum import Enum
from pathlib import Path
from typing import Any

from ._log import get_logger
from .safety import RiskLevel, SafetyEngine, ValidationResult

logger = get_logger()


class PatchMode(Enum):
    DRY_RUN = "dry_run"
    APPLY = "apply"
    PR = "pr"


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
        self.validation: ValidationResult | None = None

    def generate_diff(self) -> str:
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

    def get_summary(self) -> dict[str, Any]:
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

    def apply(self, repo_path: Path | None = None) -> bool:
        try:
            target_path = self.file_path if self.file_path.is_absolute() or repo_path is None else repo_path / self.file_path
            with open(target_path, "w", encoding="utf-8") as handle:
                handle.write(self.new_content)
            logger.info("Applied patch", file=str(target_path), fix_type=self.fix_type)
            return True
        except Exception as err:
            logger.error("Failed to apply patch", file=str(self.file_path), error=str(err))
            return False


class PatchGenerator:
    """Generate and manage patches for autofixes."""

    def __init__(self, repo_path: Path, safety_engine: SafetyEngine | None = None):
        self.repo_path = Path(repo_path)
        self.safety_engine = safety_engine or SafetyEngine()
        self.patches: list[FilePatch] = []

    def add_patch(
        self,
        file_path: Path,
        original_content: str,
        new_content: str,
        fix_type: str,
    ) -> FilePatch | None:
        if file_path.is_absolute():
            try:
                file_path = file_path.relative_to(self.repo_path)
            except ValueError:
                logger.warning("File outside repo", file=str(file_path))
                return None

        if original_content == new_content:
            return None

        full_path = self.repo_path / file_path
        try:
            risk_level = self.safety_engine.assess_risk(full_path, fix_type)
        except ValueError as err:
            logger.error("Cannot patch file", file=str(file_path), error=str(err))
            return None

        validation = self.safety_engine.validate_changes(original_content, new_content, full_path)
        if not validation["safe"] and risk_level == RiskLevel.LOW:
            risk_level = RiskLevel.MEDIUM

        patch = FilePatch(file_path, original_content, new_content, fix_type, risk_level)
        patch.validation = validation
        self.patches.append(patch)
        return patch

    def generate_unified_diff(self) -> str:
        return "\n".join(diff for diff in (patch.generate_diff() for patch in self.patches) if diff)

    def get_summary(self) -> dict[str, Any]:
        by_risk = {RiskLevel.LOW: 0, RiskLevel.MEDIUM: 0, RiskLevel.HIGH: 0}
        by_fix_type: dict[str, int] = {}
        for patch in self.patches:
            by_risk[patch.risk_level] += 1
            by_fix_type[patch.fix_type] = by_fix_type.get(patch.fix_type, 0) + 1
        return {
            "total_files": len(self.patches),
            "total_low_risk": by_risk[RiskLevel.LOW],
            "total_medium_risk": by_risk[RiskLevel.MEDIUM],
            "total_high_risk": by_risk[RiskLevel.HIGH],
            "by_fix_type": by_fix_type,
            "requires_approval": by_risk[RiskLevel.MEDIUM] + by_risk[RiskLevel.HIGH],
        }

    def dry_run(self) -> dict[str, Any]:
        return {
            "mode": PatchMode.DRY_RUN.value,
            "summary": self.get_summary(),
            "diff": self.generate_unified_diff(),
            "patches": [patch.get_summary() for patch in self.patches],
        }

    def apply(self, skip_approval: bool = False) -> dict[str, Any]:
        if not skip_approval:
            requires_approval = [
                patch for patch in self.patches if patch.risk_level in {RiskLevel.MEDIUM, RiskLevel.HIGH}
            ]
            if requires_approval:
                return {
                    "mode": PatchMode.APPLY.value,
                    "status": "requires_approval",
                    "message": f"{len(requires_approval)} patches require approval",
                    "requires_approval": [patch.get_summary() for patch in requires_approval],
                }

        applied: list[str] = []
        failed: list[str] = []
        for patch in self.patches:
            if patch.apply(self.repo_path):
                applied.append(str(patch.file_path))
            else:
                failed.append(str(patch.file_path))

        return {
            "mode": PatchMode.APPLY.value,
            "status": "success" if not failed else "partial",
            "applied": applied,
            "failed": failed,
            "summary": self.get_summary(),
        }

    def create_commit_message(self) -> str:
        summary = self.get_summary()
        lines = ["fix: apply autofixes", ""]
        for fix_type, count in summary["by_fix_type"].items():
            lines.append(f"- {fix_type}: {count} files")
        lines.extend(
            [
                "",
                f"Total files modified: {summary['total_files']}",
                f"Risk levels: {summary['total_low_risk']} low, {summary['total_medium_risk']} medium, {summary['total_high_risk']} high",
                "",
                "Generated with Aethyme Autofixer",
            ]
        )
        return "\n".join(lines)

    def save_patch_file(self, output_path: Path) -> Path:
        diff = self.generate_unified_diff()
        with open(output_path, "w", encoding="utf-8") as handle:
            handle.write(diff)
        logger.info("Saved patch file", path=str(output_path), size=len(diff))
        return output_path

    def get_changed_files(self) -> list[Path]:
        return [self.repo_path / patch.file_path for patch in self.patches]

