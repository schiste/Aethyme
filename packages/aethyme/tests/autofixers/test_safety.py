"""Tests for safety engine."""

from pathlib import Path

from src.autofixers.safety import GeneratedFileDetector, RiskLevel, SafetyEngine


class TestGeneratedFileDetector:
    """Tests for generated file detection."""

    def test_detects_lock_files(self, tmp_path: Path):
        """Should detect lock files."""
        detector = GeneratedFileDetector()

        lock_file = tmp_path / "package-lock.json"
        lock_file.touch()

        assert detector.is_generated(lock_file)

    def test_detects_generated_pattern(self, tmp_path: Path):
        """Should detect files with .generated. pattern."""
        detector = GeneratedFileDetector()

        gen_file = tmp_path / "schema.generated.ts"
        gen_file.touch()

        assert detector.is_generated(gen_file)

    def test_detects_build_directories(self, tmp_path: Path):
        """Should detect files in build directories."""
        detector = GeneratedFileDetector()

        build_file = tmp_path / "node_modules" / "package" / "index.js"
        build_file.parent.mkdir(parents=True)
        build_file.touch()

        assert detector.is_generated(build_file)

    def test_detects_generated_header(self, tmp_path: Path):
        """Should detect files with generated header."""
        detector = GeneratedFileDetector()

        gen_file = tmp_path / "generated.ts"
        gen_file.write_text("// @generated\n// Do not edit\n\ncode here")

        assert detector.is_generated(gen_file)

    def test_does_not_detect_normal_files(self, tmp_path: Path):
        """Should not detect normal files as generated."""
        detector = GeneratedFileDetector()

        normal_file = tmp_path / "component.tsx"
        normal_file.write_text("import React from 'react';")

        assert not detector.is_generated(normal_file)

    def test_filters_safe_files(self, tmp_path: Path):
        """Should filter list to safe files only."""
        detector = GeneratedFileDetector()

        # Create mix of files
        normal = tmp_path / "normal.py"
        normal.touch()

        generated = tmp_path / "generated.gen.py"
        generated.touch()

        lock = tmp_path / "poetry.lock"
        lock.touch()

        files = [normal, generated, lock]
        safe = detector.get_safe_files(files)

        assert len(safe) == 1
        assert safe[0] == normal


class TestSafetyEngine:
    """Tests for safety engine."""

    def test_assess_low_risk_docs(self, tmp_path: Path):
        """Should assess docs regeneration as low risk."""
        engine = SafetyEngine()

        doc_file = tmp_path / "README.md"
        doc_file.touch()

        risk = engine.assess_risk(doc_file, "docs_regen")
        assert risk == RiskLevel.LOW

    def test_assess_high_risk_config(self, tmp_path: Path):
        """Should assess config files as high risk."""
        engine = SafetyEngine()

        config_file = tmp_path / "package.json"
        config_file.touch()

        risk = engine.assess_risk(config_file, "any_fix")
        assert risk == RiskLevel.HIGH

    def test_assess_medium_risk_test(self, tmp_path: Path):
        """Should assess test files as medium risk."""
        engine = SafetyEngine()

        test_file = tmp_path / "test_component.py"
        test_file.touch()

        risk = engine.assess_risk(test_file, "format_fix")
        assert risk == RiskLevel.MEDIUM

    def test_rejects_generated_files(self, tmp_path: Path):
        """Should raise error for generated files."""
        engine = SafetyEngine()

        gen_file = tmp_path / "schema.generated.ts"
        gen_file.touch()

        try:
            engine.assess_risk(gen_file, "any_fix")
            raise AssertionError("Expected ValueError for generated file")
        except ValueError as err:
            assert "Cannot modify generated file" in str(err)

    def test_validates_safe_changes(self):
        """Should validate safe changes."""
        engine = SafetyEngine()

        original = "line1\nline2\nline3"
        new = "line1\nline2 updated\nline3\nline4"

        validation = engine.validate_changes(original, new, Path("test.py"))

        assert validation["safe"]
        assert validation["stats"]["lines_added"] == 1

    def test_detects_unsafe_deletion(self):
        """Should detect unsafe content deletion."""
        engine = SafetyEngine()

        original = "def function():\n    pass\n"
        new = ""  # Everything deleted

        validation = engine.validate_changes(original, new, Path("test.py"))

        assert not validation["safe"]
        assert "All content removed" in validation["warnings"][0]

    def test_detects_large_size_increase(self):
        """Should detect suspicious size increases."""
        engine = SafetyEngine()

        original = "small"
        new = "very large content" * 1000  # More than 2x

        validation = engine.validate_changes(original, new, Path("test.py"))

        assert not validation["safe"]
        assert any("doubled" in w for w in validation["warnings"])

    def test_should_skip_generated_file(self, tmp_path: Path):
        """Should skip generated files."""
        engine = SafetyEngine()

        gen_file = tmp_path / "node_modules" / "lib.js"
        gen_file.parent.mkdir(parents=True)
        gen_file.touch()

        assert engine.should_skip_file(gen_file)

    def test_requires_approval_for_high_risk(self):
        """Should require approval for high risk."""
        engine = SafetyEngine()

        assert engine.get_approval_required(RiskLevel.HIGH)
        assert engine.get_approval_required(RiskLevel.MEDIUM)
        assert not engine.get_approval_required(RiskLevel.LOW)

    def test_get_skipped_patterns(self):
        """Should return all safety patterns."""
        engine = SafetyEngine()

        patterns = engine.get_skipped_patterns()

        assert "generated_patterns" in patterns
        assert "lock_files" in patterns
        assert "build_dirs" in patterns
        assert len(patterns["lock_files"]) > 0
