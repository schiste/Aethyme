"""
Autofix Patch Generator

Generates patches for invariant violations with dependency resolution.
Links to autofixers from S1-T5 and handles conflict detection.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import difflib
import logging
import re
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Any, Set

from .invariants import InvariantViolation, InvariantRulesEngine
from .gap_detector import DetectedGap, GapDetector

logger = logging.getLogger(__name__)


@dataclass
class PatchHunk:
    """Single hunk in a patch"""
    file_path: str
    start_line: int
    end_line: int
    original: List[str]
    modified: List[str]
    description: str


@dataclass
class AutofixPatch:
    """Complete autofix patch for a violation"""
    patch_id: str
    invariant_id: str
    gap_id: Optional[str]
    hunks: List[PatchHunk] = field(default_factory=list)
    dependencies: List[str] = field(default_factory=list)  # Other patches needed first
    conflicts: List[str] = field(default_factory=list)  # Conflicting patches
    safe: bool = True  # Safe to auto-apply
    estimated_impact: str = "low"  # low, medium, high
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_unified_diff(self) -> str:
        """Generate unified diff format"""
        diff_lines = []

        diff_lines.append(f"Patch: {self.patch_id}")
        diff_lines.append(f"Invariant: {self.invariant_id}")
        diff_lines.append(f"Safe: {self.safe}")
        diff_lines.append(f"Impact: {self.estimated_impact}")
        diff_lines.append("")

        for hunk in self.hunks:
            diff_lines.append(f"--- a/{hunk.file_path}")
            diff_lines.append(f"+++ b/{hunk.file_path}")

            # Generate unified diff for this hunk
            diff = difflib.unified_diff(
                hunk.original,
                hunk.modified,
                fromfile=hunk.file_path,
                tofile=hunk.file_path,
                lineterm=""
            )

            diff_lines.extend(list(diff)[2:])  # Skip the first two lines (already added)
            diff_lines.append("")

        return "\n".join(diff_lines)


class AutofixPatchGenerator:
    """
    Generates autofix patches for invariant violations.

    Links to autofixers:
    - data-ui-selectors: Insert data-ui attributes
    - generated-file-protection: Add @generated markers
    - relative-links: Convert absolute to relative links
    - i18n-coverage: Add i18n function calls
    - folder-docs: Generate FOLDER.md templates
    - index-files: Generate barrel export indices
    - onboarding-prompts: Generate Agent.md
    - discovery-hooks: Generate .repograph.json

    Features:
    - Dependency resolution
    - Conflict detection
    - Batch generation
    - Safety validation
    """

    def __init__(
        self,
        rules_engine: Optional[InvariantRulesEngine] = None,
        gap_detector: Optional[GapDetector] = None
    ):
        self.rules_engine = rules_engine or InvariantRulesEngine()
        self.gap_detector = gap_detector or GapDetector(self.rules_engine)

        # Map invariants to autofix generators
        self.fixers = {
            "data-ui-selectors": self._fix_data_ui_selectors,
            "generated-file-protection": self._fix_generated_markers,
            "relative-links": self._fix_relative_links,
            "i18n-coverage": self._fix_i18n_strings,
            "folder-docs": self._fix_folder_docs,
            "index-files": self._fix_index_files,
            "onboarding-prompts": self._fix_agent_manifest,
            "discovery-hooks": self._fix_repograph_config,
        }

    def generate_patch(
        self,
        gap: DetectedGap,
        repo_path: Path
    ) -> Optional[AutofixPatch]:
        """Generate autofix patch for a single gap"""

        if gap.invariant_id not in self.fixers:
            logger.warning(f"No fixer available for {gap.invariant_id}")
            return None

        fixer = self.fixers[gap.invariant_id]

        try:
            patch = fixer(gap, repo_path)
            return patch
        except Exception as e:
            logger.error(f"Error generating patch for {gap.gap_id}: {e}")
            return None

    def generate_batch_patches(
        self,
        gaps: List[DetectedGap],
        repo_path: Path
    ) -> List[AutofixPatch]:
        """Generate patches for multiple gaps"""
        patches = []

        for gap in gaps:
            if gap.fix and gap.fix.autofix_available:
                patch = self.generate_patch(gap, repo_path)
                if patch:
                    patches.append(patch)

        # Resolve dependencies
        patches = self._resolve_dependencies(patches)

        # Detect conflicts
        patches = self._detect_conflicts(patches)

        return patches

    def get_fix_sequence(self, patches: List[AutofixPatch]) -> List[List[AutofixPatch]]:
        """
        Get patches in dependency-resolved sequence.

        Returns list of batches where each batch can be applied in parallel.
        """
        # Topological sort
        sequence = []
        applied = set()

        patch_by_id = {p.patch_id: p for p in patches}

        while len(applied) < len(patches):
            # Find patches with no unapplied dependencies
            batch = []

            for patch in patches:
                if patch.patch_id in applied:
                    continue

                # Check dependencies
                deps_met = all(dep in applied for dep in patch.dependencies)

                # Check for conflicts with already applied
                no_conflicts = all(conf not in applied for conf in patch.conflicts)

                if deps_met and no_conflicts:
                    batch.append(patch)
                    applied.add(patch.patch_id)

            if batch:
                sequence.append(batch)
            else:
                # Circular dependency or unresolvable conflicts
                # Add remaining patches with warning
                remaining = [p for p in patches if p.patch_id not in applied]
                if remaining:
                    logger.warning("Circular dependencies or conflicts detected")
                    sequence.append(remaining)
                break

        return sequence

    def apply_patch(
        self,
        patch: AutofixPatch,
        repo_path: Path,
        dry_run: bool = True
    ) -> bool:
        """Apply a single patch to repository"""

        if not patch.safe and not dry_run:
            logger.warning(f"Patch {patch.patch_id} is not safe for auto-apply")
            return False

        try:
            for hunk in patch.hunks:
                file_path = repo_path / hunk.file_path

                if dry_run:
                    logger.info(f"[DRY RUN] Would modify: {file_path}")
                    continue

                # Read current content
                if file_path.exists():
                    current_lines = file_path.read_text().splitlines(keepends=True)
                else:
                    current_lines = []

                # Apply hunk
                new_lines = (
                    current_lines[:hunk.start_line - 1] +
                    hunk.modified +
                    current_lines[hunk.end_line:]
                )

                # Write back
                file_path.parent.mkdir(parents=True, exist_ok=True)
                file_path.write_text("".join(new_lines))

                logger.info(f"Applied patch to: {file_path}")

            return True

        except Exception as e:
            logger.error(f"Error applying patch {patch.patch_id}: {e}")
            return False

    # Autofix generators for each invariant

    def _fix_data_ui_selectors(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to add data-ui selectors"""
        file_path = repo_path / gap.file_path
        content = file_path.read_text()
        lines = content.splitlines(keepends=True)

        line_idx = gap.line_number - 1
        original_line = lines[line_idx]

        # Generate data-ui attribute name from context
        # Simple heuristic: use component name or element type
        match = re.search(r'<(\w+)', original_line)
        if match:
            element_type = match.group(1)
            # Generate selector name
            file_stem = Path(gap.file_path).stem.lower()
            selector = f"{file_stem}-{element_type}"

            # Insert data-ui attribute
            modified_line = re.sub(
                r'(<\w+\b)',
                rf'\1 data-ui="{selector}"',
                original_line,
                count=1
            )

            hunk = PatchHunk(
                file_path=gap.file_path,
                start_line=gap.line_number,
                end_line=gap.line_number,
                original=[original_line],
                modified=[modified_line],
                description=f"Add data-ui='{selector}' attribute"
            )

            patch = AutofixPatch(
                patch_id=f"PATCH-{gap.gap_id}",
                invariant_id=gap.invariant_id,
                gap_id=gap.gap_id,
                hunks=[hunk],
                safe=True,
                estimated_impact="low"
            )

            return patch

        # Fallback: couldn't parse
        raise ValueError("Could not parse element to add data-ui")

    def _fix_generated_markers(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to add @generated markers"""
        file_path = repo_path / gap.file_path

        # Detect file type for comment style
        if file_path.suffix in [".py"]:
            marker = "# @generated - DO NOT EDIT MANUALLY\n"
        elif file_path.suffix in [".ts", ".tsx", ".js", ".jsx"]:
            marker = "// @generated - DO NOT EDIT MANUALLY\n"
        elif file_path.suffix in [".html", ".xml"]:
            marker = "<!-- @generated - DO NOT EDIT MANUALLY -->\n"
        else:
            marker = "# @generated - DO NOT EDIT MANUALLY\n"

        # Add marker at top of file
        hunk = PatchHunk(
            file_path=gap.file_path,
            start_line=1,
            end_line=1,
            original=[],
            modified=[marker],
            description="Add @generated marker to file header"
        )

        patch = AutofixPatch(
            patch_id=f"PATCH-{gap.gap_id}",
            invariant_id=gap.invariant_id,
            gap_id=gap.gap_id,
            hunks=[hunk],
            safe=True,
            estimated_impact="low"
        )

        return patch

    def _fix_relative_links(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to convert absolute links to relative"""
        file_path = repo_path / gap.file_path
        content = file_path.read_text()
        lines = content.splitlines(keepends=True)

        line_idx = gap.line_number - 1
        original_line = lines[line_idx]

        # Convert absolute URL to relative path
        # This is a simplified heuristic
        modified_line = re.sub(
            r'\(https?://[^/]+/[^/]+/[^/]+/blob/[^/]+/([^)]+)\)',
            r'(\1)',
            original_line
        )

        hunk = PatchHunk(
            file_path=gap.file_path,
            start_line=gap.line_number,
            end_line=gap.line_number,
            original=[original_line],
            modified=[modified_line],
            description="Convert absolute link to relative"
        )

        patch = AutofixPatch(
            patch_id=f"PATCH-{gap.gap_id}",
            invariant_id=gap.invariant_id,
            gap_id=gap.gap_id,
            hunks=[hunk],
            safe=True,
            estimated_impact="low"
        )

        return patch

    def _fix_i18n_strings(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to externalize hardcoded strings"""
        file_path = repo_path / gap.file_path
        content = file_path.read_text()
        lines = content.splitlines(keepends=True)

        line_idx = gap.line_number - 1
        original_line = lines[line_idx]

        # Extract string content
        match = re.search(r'>([A-Z][a-z\s]+)<', original_line)
        if match:
            text = match.group(1)

            # Generate i18n key
            key = text.lower().replace(' ', '_')[:30]
            file_stem = Path(gap.file_path).stem

            # Replace with i18n function
            modified_line = re.sub(
                r'>([A-Z][a-z\s]+)<',
                rf'>{{{t("{file_stem}.{key}")}}}<',
                original_line
            )

            hunk = PatchHunk(
                file_path=gap.file_path,
                start_line=gap.line_number,
                end_line=gap.line_number,
                original=[original_line],
                modified=[modified_line],
                description=f"Externalize string to i18n key: {file_stem}.{key}"
            )

            patch = AutofixPatch(
                patch_id=f"PATCH-{gap.gap_id}",
                invariant_id=gap.invariant_id,
                gap_id=gap.gap_id,
                hunks=[hunk],
                safe=False,  # Requires manual review
                estimated_impact="medium",
                metadata={"i18n_key": f"{file_stem}.{key}", "original_text": text}
            )

            return patch

        raise ValueError("Could not extract string for i18n")

    def _fix_folder_docs(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to create FOLDER.md"""
        dir_path = gap.file_path
        folder_md_path = f"{dir_path}/FOLDER.md"

        # Generate FOLDER.md content
        dir_name = Path(dir_path).name
        content = f"""# {dir_name}

## Purpose

[TODO: Describe the purpose of this directory]

## Contents

[TODO: List key files and their responsibilities]

## Usage

[TODO: Explain how to use the code in this directory]

## Dependencies

[TODO: List dependencies or related directories]

---
*This file was auto-generated. Please update with relevant information.*
"""

        hunk = PatchHunk(
            file_path=folder_md_path,
            start_line=1,
            end_line=1,
            original=[],
            modified=content.splitlines(keepends=True),
            description=f"Create FOLDER.md for {dir_name}"
        )

        patch = AutofixPatch(
            patch_id=f"PATCH-{gap.gap_id}",
            invariant_id=gap.invariant_id,
            gap_id=gap.gap_id,
            hunks=[hunk],
            safe=True,
            estimated_impact="low"
        )

        return patch

    def _fix_index_files(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to create index/barrel export files"""
        dir_path = repo_path / gap.file_path

        # Detect language and create appropriate index file
        if any(dir_path.glob("*.py")):
            index_file = "__init__.py"
            exports = self._generate_python_exports(dir_path)
        else:
            index_file = "index.ts"
            exports = self._generate_ts_exports(dir_path)

        index_path = f"{gap.file_path}/{index_file}"

        hunk = PatchHunk(
            file_path=index_path,
            start_line=1,
            end_line=1,
            original=[],
            modified=exports,
            description=f"Create {index_file} barrel export"
        )

        patch = AutofixPatch(
            patch_id=f"PATCH-{gap.gap_id}",
            invariant_id=gap.invariant_id,
            gap_id=gap.gap_id,
            hunks=[hunk],
            safe=True,
            estimated_impact="low"
        )

        return patch

    def _fix_agent_manifest(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to create Agent.md"""
        # Use onboarding manifest generator (cross-reference)
        # For now, create a simple template

        content = """# AI Agent Onboarding Manifest

## Project Identity
- **Name:** [Project Name]
- **Type:** [Type]

## Agent Role
You are an AI development assistant for this project.

## Build/Run/Test
```bash
# Add commands here
```

## Coding Conventions
- [Add conventions]

## Architecture
- [Add architecture notes]

---
*This file was auto-generated. Please update with project-specific information.*
"""

        hunk = PatchHunk(
            file_path="agent.md",
            start_line=1,
            end_line=1,
            original=[],
            modified=content.splitlines(keepends=True),
            description="Create Agent.md onboarding manifest"
        )

        patch = AutofixPatch(
            patch_id=f"PATCH-{gap.gap_id}",
            invariant_id=gap.invariant_id,
            gap_id=gap.gap_id,
            hunks=[hunk],
            safe=True,
            estimated_impact="low"
        )

        return patch

    def _fix_repograph_config(self, gap: DetectedGap, repo_path: Path) -> AutofixPatch:
        """Generate patch to create .repograph.json"""
        config = {
            "version": "1.0.0",
            "indexing": {
                "enabled": True,
                "languages": ["python", "typescript", "javascript"]
            },
            "features": {
                "agent_parity": True,
                "context_packs": True
            }
        }

        content = json.dumps(config, indent=2) + "\n"

        hunk = PatchHunk(
            file_path=".repograph.json",
            start_line=1,
            end_line=1,
            original=[],
            modified=content.splitlines(keepends=True),
            description="Create RepoGraph configuration"
        )

        patch = AutofixPatch(
            patch_id=f"PATCH-{gap.gap_id}",
            invariant_id=gap.invariant_id,
            gap_id=gap.gap_id,
            hunks=[hunk],
            safe=True,
            estimated_impact="low"
        )

        return patch

    # Helper methods

    def _generate_python_exports(self, dir_path: Path) -> List[str]:
        """Generate Python __init__.py exports"""
        exports = ["# Auto-generated barrel exports\n\n"]

        for py_file in dir_path.glob("*.py"):
            if py_file.name != "__init__.py":
                module = py_file.stem
                exports.append(f"from .{module} import *\n")

        return exports

    def _generate_ts_exports(self, dir_path: Path) -> List[str]:
        """Generate TypeScript index.ts exports"""
        exports = ["// Auto-generated barrel exports\n\n"]

        for ts_file in dir_path.glob("*.ts"):
            if ts_file.name != "index.ts":
                module = ts_file.stem
                exports.append(f"export * from './{module}';\n")

        return exports

    def _resolve_dependencies(self, patches: List[AutofixPatch]) -> List[AutofixPatch]:
        """Resolve dependencies between patches"""
        # Build dependency graph based on invariant dependencies
        invariant_deps = {
            "onboarding-prompts": [],
            "discovery-hooks": [],
            "config-driven-routes": [],
            "folder-docs": [],
            "index-files": ["folder-docs"],
            "data-ui-selectors": ["config-driven-routes"],
            "i18n-coverage": ["config-driven-routes"],
            "relative-links": ["folder-docs"],
            "generated-file-protection": [],
        }

        patch_by_id = {p.patch_id: p for p in patches}
        patch_by_invariant = {}
        for p in patches:
            if p.invariant_id not in patch_by_invariant:
                patch_by_invariant[p.invariant_id] = []
            patch_by_invariant[p.invariant_id].append(p.patch_id)

        # Add dependencies
        for patch in patches:
            if patch.invariant_id in invariant_deps:
                for dep_invariant in invariant_deps[patch.invariant_id]:
                    if dep_invariant in patch_by_invariant:
                        patch.dependencies.extend(patch_by_invariant[dep_invariant])

        return patches

    def _detect_conflicts(self, patches: List[AutofixPatch]) -> List[AutofixPatch]:
        """Detect conflicts between patches"""
        # Group patches by file
        patches_by_file = {}
        for patch in patches:
            for hunk in patch.hunks:
                if hunk.file_path not in patches_by_file:
                    patches_by_file[hunk.file_path] = []
                patches_by_file[hunk.file_path].append((patch, hunk))

        # Check for overlapping line ranges
        for file_path, file_patches in patches_by_file.items():
            for i, (patch1, hunk1) in enumerate(file_patches):
                for patch2, hunk2 in file_patches[i + 1:]:
                    # Check if hunks overlap
                    if self._hunks_overlap(hunk1, hunk2):
                        if patch2.patch_id not in patch1.conflicts:
                            patch1.conflicts.append(patch2.patch_id)
                        if patch1.patch_id not in patch2.conflicts:
                            patch2.conflicts.append(patch1.patch_id)

        return patches

    def _hunks_overlap(self, hunk1: PatchHunk, hunk2: PatchHunk) -> bool:
        """Check if two hunks overlap in line ranges"""
        return not (hunk1.end_line < hunk2.start_line or hunk2.end_line < hunk1.start_line)
