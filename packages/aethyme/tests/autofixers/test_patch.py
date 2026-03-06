"""Tests for patch generator."""

from pathlib import Path

from src.autofixers.patch import FilePatch, PatchGenerator
from src.autofixers.safety import RiskLevel


class TestFilePatch:
    """Tests for FilePatch class."""

    def test_generates_diff(self, tmp_path: Path):
        """Should generate unified diff."""
        file_path = Path("test.py")
        original = "line1\nline2\nline3"
        new = "line1\nline2 modified\nline3"

        patch = FilePatch(file_path, original, new, "test_fix", RiskLevel.LOW)
        diff = patch.generate_diff()

        assert "--- a/test.py" in diff
        assert "+++ b/test.py" in diff
        assert "-line2" in diff
        assert "+line2 modified" in diff

    def test_get_summary(self):
        """Should return patch summary."""
        patch = FilePatch(
            Path("test.py"),
            "original",
            "modified",
            "format_fix",
            RiskLevel.MEDIUM,
        )

        summary = patch.get_summary()

        assert summary["file"] == "test.py"
        assert summary["fix_type"] == "format_fix"
        assert summary["risk_level"] == "medium"
        assert summary["has_changes"] is True

    def test_apply_patch(self, tmp_path: Path):
        """Should apply patch to disk."""
        file_path = tmp_path / "test.txt"
        file_path.write_text("original content")

        patch = FilePatch(
            file_path,
            "original content",
            "new content",
            "test_fix",
            RiskLevel.LOW,
        )

        assert patch.apply()
        assert file_path.read_text() == "new content"


class TestPatchGenerator:
    """Tests for PatchGenerator class."""

    def test_add_patch_with_changes(self, tmp_path: Path):
        """Should add patch when there are changes."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "test.py"
        file_path.write_text("original")

        patch = gen.add_patch(
            file_path,
            "original",
            "modified",
            "test_fix",
        )

        assert patch is not None
        assert len(gen.patches) == 1

    def test_skip_patch_without_changes(self, tmp_path: Path):
        """Should skip patch when no changes."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "test.py"
        file_path.write_text("content")

        patch = gen.add_patch(
            file_path,
            "content",
            "content",  # Same
            "test_fix",
        )

        assert patch is None
        assert len(gen.patches) == 0

    def test_skip_generated_files(self, tmp_path: Path):
        """Should skip generated files."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "generated.gen.py"
        file_path.write_text("original")

        patch = gen.add_patch(
            file_path,
            "original",
            "modified",
            "test_fix",
        )

        assert patch is None
        assert len(gen.patches) == 0

    def test_generate_unified_diff(self, tmp_path: Path):
        """Should generate unified diff for all patches."""
        gen = PatchGenerator(tmp_path)

        file1 = tmp_path / "file1.py"
        file1.write_text("content1")

        file2 = tmp_path / "file2.py"
        file2.write_text("content2")

        gen.add_patch(file1, "content1", "modified1", "test_fix")
        gen.add_patch(file2, "content2", "modified2", "test_fix")

        diff = gen.generate_unified_diff()

        assert "file1.py" in diff
        assert "file2.py" in diff

    def test_get_summary(self, tmp_path: Path):
        """Should return comprehensive summary."""
        gen = PatchGenerator(tmp_path)

        # Low risk file
        low_file = tmp_path / "README.md"
        low_file.write_text("original")
        gen.add_patch(low_file, "original", "modified", "docs_regen")

        # Medium risk file
        med_file = tmp_path / "test_file.py"
        med_file.write_text("original")
        gen.add_patch(med_file, "original", "modified", "format_fix")

        summary = gen.get_summary()

        assert summary["total_files"] == 2
        assert summary["total_low_risk"] == 1
        assert summary["total_medium_risk"] == 1
        assert "docs_regen" in summary["by_fix_type"]
        assert summary["requires_approval"] == 1  # Medium risk

    def test_dry_run(self, tmp_path: Path):
        """Should show changes without applying."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "test.py"
        file_path.write_text("original")

        gen.add_patch(file_path, "original", "modified", "test_fix")

        result = gen.dry_run()

        assert result["mode"] == "dry_run"
        assert result["summary"]["total_files"] == 1
        assert "diff" in result
        assert len(result["patches"]) == 1

        # File should not be modified
        assert file_path.read_text() == "original"

    def test_apply_low_risk(self, tmp_path: Path):
        """Should apply low risk changes without approval."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "README.md"
        file_path.write_text("original")

        gen.add_patch(file_path, "original", "modified", "docs_regen")

        result = gen.apply(skip_approval=False)

        assert result["status"] == "success"
        assert len(result["applied"]) == 1
        assert file_path.read_text() == "modified"

    def test_requires_approval_for_medium_risk(self, tmp_path: Path):
        """Should require approval for medium risk."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "routes.py"
        file_path.write_text("original")

        gen.add_patch(file_path, "original", "modified", "format_fix")

        result = gen.apply(skip_approval=False)

        assert result["status"] == "requires_approval"
        assert len(result["requires_approval"]) == 1

        # File should not be modified
        assert file_path.read_text() == "original"

    def test_skip_approval_applies_all(self, tmp_path: Path):
        """Should apply all changes when skipping approval."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "routes.py"
        file_path.write_text("original")

        gen.add_patch(file_path, "original", "modified", "format_fix")

        result = gen.apply(skip_approval=True)

        assert result["status"] == "success"
        assert len(result["applied"]) == 1
        assert file_path.read_text() == "modified"

    def test_create_commit_message(self, tmp_path: Path):
        """Should create descriptive commit message."""
        gen = PatchGenerator(tmp_path)

        file1 = tmp_path / "file1.md"
        file1.write_text("original")
        gen.add_patch(file1, "original", "modified", "docs_regen")

        file2 = tmp_path / "file2.py"
        file2.write_text("original")
        gen.add_patch(file2, "original", "modified", "format_fix")

        message = gen.create_commit_message()

        assert "autofixes" in message.lower()
        assert "docs_regen" in message
        assert "format_fix" in message
        assert "2" in message  # 2 files

    def test_save_patch_file(self, tmp_path: Path):
        """Should save patch to file."""
        gen = PatchGenerator(tmp_path)

        file_path = tmp_path / "test.py"
        file_path.write_text("original")
        gen.add_patch(file_path, "original", "modified", "test_fix")

        output = tmp_path / "changes.patch"
        result = gen.save_patch_file(output)

        assert result == output
        assert output.exists()
        assert output.read_text()  # Has content

    def test_get_changed_files(self, tmp_path: Path):
        """Should return list of changed files."""
        gen = PatchGenerator(tmp_path)

        file1 = tmp_path / "file1.py"
        file1.write_text("content")
        file2 = tmp_path / "file2.py"
        file2.write_text("content")

        gen.add_patch(file1, "content", "modified1", "test")
        gen.add_patch(file2, "content", "modified2", "test")

        changed = gen.get_changed_files()

        assert len(changed) == 2
        assert file1 in changed
        assert file2 in changed
