"""
Tests for language detection and support.

Tests language detection, allowlisting, and mixed-language repositories.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from src.indexing.language_support import (
    LANGUAGE_REGISTRY,
    LanguageDetector,
    LanguageSupport,
    get_recommended_languages_for_repo,
)


class TestLanguageDetection:
    """Tests for language detection."""

    def test_detect_python(self, tmp_path: Path) -> None:
        """Test detecting Python files."""
        detector = LanguageDetector()

        test_file = tmp_path / "test.py"
        assert detector.detect_language(test_file) == "python"

        test_file = tmp_path / "test.pyi"
        assert detector.detect_language(test_file) == "python"

    def test_detect_typescript(self, tmp_path: Path) -> None:
        """Test detecting TypeScript files."""
        detector = LanguageDetector()

        test_file = tmp_path / "test.ts"
        assert detector.detect_language(test_file) == "typescript"

        test_file = tmp_path / "test.tsx"
        assert detector.detect_language(test_file) == "typescript"

    def test_detect_javascript(self, tmp_path: Path) -> None:
        """Test detecting JavaScript files."""
        detector = LanguageDetector()

        test_file = tmp_path / "test.js"
        assert detector.detect_language(test_file) == "javascript"

        test_file = tmp_path / "test.jsx"
        assert detector.detect_language(test_file) == "javascript"

    def test_detect_go(self, tmp_path: Path) -> None:
        """Test detecting Go files."""
        detector = LanguageDetector()

        test_file = tmp_path / "test.go"
        assert detector.detect_language(test_file) == "go"

    def test_detect_rust(self, tmp_path: Path) -> None:
        """Test detecting Rust files."""
        detector = LanguageDetector()

        test_file = tmp_path / "test.rs"
        assert detector.detect_language(test_file) == "rust"

    def test_detect_java(self, tmp_path: Path) -> None:
        """Test detecting Java files."""
        detector = LanguageDetector()

        test_file = tmp_path / "Test.java"
        assert detector.detect_language(test_file) == "java"

    def test_detect_unknown(self, tmp_path: Path) -> None:
        """Test detecting unknown file types."""
        detector = LanguageDetector()

        test_file = tmp_path / "test.xyz"
        assert detector.detect_language(test_file) is None

        test_file = tmp_path / "README.md"
        assert detector.detect_language(test_file) is None

    def test_allowlist_filtering(self, tmp_path: Path) -> None:
        """Test language allowlist filtering."""
        detector = LanguageDetector(allowed_languages=["python", "typescript"])

        # Allowed
        assert detector.detect_language(tmp_path / "test.py") == "python"
        assert detector.detect_language(tmp_path / "test.ts") == "typescript"

        # Not allowed (but recognized)
        assert detector.detect_language(tmp_path / "test.go") is None
        assert detector.detect_language(tmp_path / "test.java") is None


class TestLanguageSupport:
    """Tests for language support checking."""

    def test_is_supported_languages(self):
        """Test checking supported languages."""
        detector = LanguageDetector()

        # Fully supported
        assert detector.is_supported("python")
        assert detector.is_supported("typescript")
        assert detector.is_supported("go")

        # Fallback supported
        assert detector.is_supported("java")

        # Unknown
        assert not detector.is_supported("unknown")

    def test_get_support_level(self):
        """Test getting support levels."""
        detector = LanguageDetector()

        assert detector.get_support_level("python") == LanguageSupport.FULL
        assert detector.get_support_level("typescript") == LanguageSupport.FULL
        assert detector.get_support_level("java") == LanguageSupport.FALLBACK
        assert detector.get_support_level("kotlin") == LanguageSupport.EXPERIMENTAL
        assert detector.get_support_level("unknown") is None

    def test_should_use_scip(self):
        """Test determining if SCIP should be used."""
        detector = LanguageDetector()

        # SCIP available
        assert detector.should_use_scip("python")
        assert detector.should_use_scip("typescript")
        assert detector.should_use_scip("go")

        # SCIP not available
        assert not detector.should_use_scip("java")
        assert not detector.should_use_scip("ruby")

    def test_get_language_config(self):
        """Test getting language configuration."""
        detector = LanguageDetector()

        config = detector.get_language_config("python")
        assert config is not None
        assert config.name == "python"
        assert config.support_level == LanguageSupport.FULL
        assert ".py" in config.file_extensions

        config = detector.get_language_config("unknown")
        assert config is None


class TestFileScanning:
    """Tests for scanning directories for files."""

    def test_get_files_by_language(self, tmp_path: Path) -> None:
        """Test grouping files by language."""
        # Create test files
        (tmp_path / "test.py").touch()
        (tmp_path / "app.py").touch()
        (tmp_path / "main.ts").touch()
        (tmp_path / "util.go").touch()
        (tmp_path / "README.md").touch()

        detector = LanguageDetector()
        files_by_lang = detector.get_files_by_language(tmp_path)

        assert "python" in files_by_lang
        assert len(files_by_lang["python"]) == 2

        assert "typescript" in files_by_lang
        assert len(files_by_lang["typescript"]) == 1

        assert "go" in files_by_lang
        assert len(files_by_lang["go"]) == 1

    def test_exclude_directories(self, tmp_path: Path) -> None:
        """Test excluding directories from scan."""
        # Create test structure
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "main.py").touch()

        (tmp_path / "node_modules").mkdir()
        (tmp_path / "node_modules" / "lib.js").touch()

        (tmp_path / "__pycache__").mkdir()
        (tmp_path / "__pycache__" / "cache.pyc").touch()

        detector = LanguageDetector()
        files_by_lang = detector.get_files_by_language(
            tmp_path,
            exclude_dirs=["node_modules", "__pycache__"],
        )

        # Should only find main.py
        assert "python" in files_by_lang
        assert len(files_by_lang["python"]) == 1
        assert "javascript" not in files_by_lang

    def test_get_language_stats(self, tmp_path: Path) -> None:
        """Test getting language statistics."""
        # Create test files
        files: list[Path] = [
            tmp_path / "test1.py",
            tmp_path / "test2.py",
            tmp_path / "test3.py",
            tmp_path / "main.ts",
            tmp_path / "app.ts",
        ]

        for f in files:
            f.touch()

        detector = LanguageDetector()
        stats = detector.get_language_stats(files)

        assert stats["python"] == 3
        assert stats["typescript"] == 2


class TestLanguageValidation:
    """Tests for language validation."""

    def test_validate_language_list(self):
        """Test validating list of languages."""
        detector = LanguageDetector()

        languages = ["python", "typescript", "unknown", "kotlin"]
        results = detector.validate_language_list(languages)

        assert results["python"] == ""  # Valid
        assert results["typescript"] == ""  # Valid
        assert "Unknown" in results["unknown"]
        assert "Experimental" in results["kotlin"]

    def test_get_supported_languages(self):
        """Test getting list of supported languages."""
        detector = LanguageDetector()

        # Without experimental
        languages = detector.get_supported_languages(include_experimental=False)
        assert "python" in languages
        assert "typescript" in languages
        assert "kotlin" not in languages

        # With experimental
        languages = detector.get_supported_languages(include_experimental=True)
        assert "python" in languages
        assert "kotlin" in languages


class TestLanguageRegistry:
    """Tests for language registry."""

    def test_registry_completeness(self):
        """Test that registry has expected languages."""
        expected_languages = [
            "python",
            "typescript",
            "javascript",
            "go",
            "rust",
            "java",
            "kotlin",
            "ruby",
            "php",
            "csharp",
            "cpp",
            "c",
        ]

        for lang in expected_languages:
            assert lang in LANGUAGE_REGISTRY

    def test_registry_structure(self):
        """Test that registry entries have proper structure."""
        for lang_name, config in LANGUAGE_REGISTRY.items():
            assert config.name == lang_name
            assert isinstance(config.file_extensions, list)
            assert len(config.file_extensions) > 0
            assert isinstance(config.scip_available, bool)
            assert isinstance(config.fallback_available, bool)
            assert isinstance(config.support_level, LanguageSupport)


class TestMixedLanguageRepos:
    """Tests for handling mixed-language repositories."""

    def test_mixed_language_detection(self, tmp_path: Path) -> None:
        """Test detecting multiple languages in one repo."""
        # Create mixed-language structure
        (tmp_path / "backend").mkdir()
        (tmp_path / "backend" / "main.py").touch()
        (tmp_path / "backend" / "utils.py").touch()

        (tmp_path / "frontend").mkdir()
        (tmp_path / "frontend" / "app.ts").touch()
        (tmp_path / "frontend" / "components.tsx").touch()

        detector = LanguageDetector()
        files_by_lang = detector.get_files_by_language(tmp_path)

        assert "python" in files_by_lang
        assert "typescript" in files_by_lang
        assert len(files_by_lang["python"]) == 2
        assert len(files_by_lang["typescript"]) == 2

    def test_recommend_languages(self, tmp_path: Path) -> None:
        """Test recommending languages for a repo."""
        # Create many Python files, few JS files
        for i in range(10):
            (tmp_path / f"file{i}.py").touch()

        (tmp_path / "single.js").touch()

        recommended = get_recommended_languages_for_repo(tmp_path)
        assert recommended == ["python"]


class TestUnsupportedLanguages:
    """Tests for handling unsupported languages gracefully."""

    def test_unsupported_file_skipped(self, tmp_path: Path) -> None:
        """Test that unsupported files are skipped."""
        detector = LanguageDetector()

        # Unknown extension
        assert detector.detect_language(tmp_path / "file.xyz") is None

        # Binary file
        assert detector.detect_language(tmp_path / "file.exe") is None

    def test_mixed_supported_unsupported(self, tmp_path: Path) -> None:
        """Test scanning with mix of supported and unsupported files."""
        # Supported
        (tmp_path / "main.py").touch()

        # Unsupported
        (tmp_path / "README.md").touch()
        (tmp_path / "image.png").touch()

        detector = LanguageDetector()
        files_by_lang = detector.get_files_by_language(tmp_path)

        # Only Python should be detected
        assert "python" in files_by_lang
        assert len(files_by_lang) == 1


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
