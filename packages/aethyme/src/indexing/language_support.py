"""
Language Support and Guardrails Module

Provides language detection, allowlisting, and graceful handling of unsupported files.
"""

from dataclasses import dataclass
from enum import Enum
from pathlib import Path

import structlog

logger = structlog.get_logger(__name__)


class LanguageSupport(Enum):
    """Level of support for a language."""
    FULL = "full"  # SCIP indexer available and tested
    FALLBACK = "fallback"  # Only fallback indexer available
    EXPERIMENTAL = "experimental"  # Limited testing
    UNSUPPORTED = "unsupported"  # Not supported


@dataclass
class LanguageConfig:
    """Configuration for a language."""
    name: str
    support_level: LanguageSupport
    file_extensions: list[str]
    scip_available: bool
    fallback_available: bool
    notes: str = ""


# Language configuration registry
LANGUAGE_REGISTRY: dict[str, LanguageConfig] = {
    "python": LanguageConfig(
        name="python",
        support_level=LanguageSupport.FULL,
        file_extensions=[".py", ".pyi", ".pyx"],
        scip_available=True,
        fallback_available=True,
        notes="Full support with scip-python. Best performance with type hints.",
    ),
    "typescript": LanguageConfig(
        name="typescript",
        support_level=LanguageSupport.FULL,
        file_extensions=[".ts", ".tsx", ".mts", ".cts"],
        scip_available=True,
        fallback_available=True,
        notes="Full support with scip-typescript. Handles JSX/TSX.",
    ),
    "javascript": LanguageConfig(
        name="javascript",
        support_level=LanguageSupport.FULL,
        file_extensions=[".js", ".jsx", ".mjs", ".cjs"],
        scip_available=True,
        fallback_available=True,
        notes="Full support. TypeScript indexer handles JavaScript too.",
    ),
    "go": LanguageConfig(
        name="go",
        support_level=LanguageSupport.FULL,
        file_extensions=[".go"],
        scip_available=True,
        fallback_available=True,
        notes="Full support with scip-go. Excellent for interface analysis.",
    ),
    "rust": LanguageConfig(
        name="rust",
        support_level=LanguageSupport.FULL,
        file_extensions=[".rs"],
        scip_available=True,
        fallback_available=True,
        notes="Full support with rust-analyzer. Handles macros well.",
    ),
    "java": LanguageConfig(
        name="java",
        support_level=LanguageSupport.FALLBACK,
        file_extensions=[".java"],
        scip_available=False,
        fallback_available=True,
        notes="Fallback support only. Limited to basic symbol extraction.",
    ),
    "kotlin": LanguageConfig(
        name="kotlin",
        support_level=LanguageSupport.EXPERIMENTAL,
        file_extensions=[".kt", ".kts"],
        scip_available=False,
        fallback_available=True,
        notes="Experimental. Fallback indexer only.",
    ),
    "ruby": LanguageConfig(
        name="ruby",
        support_level=LanguageSupport.FALLBACK,
        file_extensions=[".rb", ".rake"],
        scip_available=False,
        fallback_available=True,
        notes="Fallback support. Limited meta-programming detection.",
    ),
    "php": LanguageConfig(
        name="php",
        support_level=LanguageSupport.FALLBACK,
        file_extensions=[".php"],
        scip_available=False,
        fallback_available=True,
        notes="Fallback support only.",
    ),
    "csharp": LanguageConfig(
        name="csharp",
        support_level=LanguageSupport.EXPERIMENTAL,
        file_extensions=[".cs"],
        scip_available=False,
        fallback_available=True,
        notes="Experimental. Basic symbol extraction.",
    ),
    "cpp": LanguageConfig(
        name="cpp",
        support_level=LanguageSupport.FALLBACK,
        file_extensions=[".cpp", ".cc", ".cxx", ".hpp", ".h", ".hxx"],
        scip_available=False,
        fallback_available=True,
        notes="Fallback support. Complex macros may be missed.",
    ),
    "c": LanguageConfig(
        name="c",
        support_level=LanguageSupport.FALLBACK,
        file_extensions=[".c", ".h"],
        scip_available=False,
        fallback_available=True,
        notes="Fallback support.",
    ),
}


class LanguageDetector:
    """
    Detects programming language from file paths and content.

    Provides allowlist checking and graceful handling of unsupported files.
    """

    def __init__(self, allowed_languages: list[str] | None = None):
        """
        Initialize language detector.

        Args:
            allowed_languages: List of allowed language names. If None, all registered languages allowed.
        """
        self.allowed_languages: set[str] | None = (
            set(allowed_languages) if allowed_languages else None
        )
        self.logger = logger.bind(component="LanguageDetector")

    def detect_language(self, file_path: Path) -> str | None:
        """
        Detect language from file extension.

        Args:
            file_path: Path to file

        Returns:
            Language name if detected and allowed, None otherwise
        """
        extension = file_path.suffix.lower()

        for lang_name, config in LANGUAGE_REGISTRY.items():
            if extension in config.file_extensions:
                # Check if language is allowed
                if self.allowed_languages and lang_name not in self.allowed_languages:
                    self.logger.debug(
                        "Language not in allowlist",
                        language=lang_name,
                        file=str(file_path),
                    )
                    return None

                return lang_name

        return None

    def is_supported(self, language: str) -> bool:
        """
        Check if a language is supported.

        Args:
            language: Language name

        Returns:
            True if supported (not UNSUPPORTED level)
        """
        config = LANGUAGE_REGISTRY.get(language)
        if not config:
            return False

        return config.support_level != LanguageSupport.UNSUPPORTED

    def get_support_level(self, language: str) -> LanguageSupport | None:
        """
        Get support level for a language.

        Args:
            language: Language name

        Returns:
            LanguageSupport enum or None if not found
        """
        config = LANGUAGE_REGISTRY.get(language)
        return config.support_level if config else None

    def get_language_config(self, language: str) -> LanguageConfig | None:
        """
        Get full configuration for a language.

        Args:
            language: Language name

        Returns:
            LanguageConfig or None if not found
        """
        return LANGUAGE_REGISTRY.get(language)

    def should_use_scip(self, language: str) -> bool:
        """
        Determine if SCIP indexer should be attempted for a language.

        Args:
            language: Language name

        Returns:
            True if SCIP is available for this language
        """
        config = LANGUAGE_REGISTRY.get(language)
        if not config:
            return False

        return config.scip_available

    def get_files_by_language(
        self,
        root_path: Path,
        exclude_dirs: list[str] | None = None,
    ) -> dict[str, list[Path]]:
        """
        Scan directory and group files by detected language.

        Args:
            root_path: Root directory to scan
            exclude_dirs: Directories to skip

        Returns:
            Dict mapping language names to lists of file paths
        """
        excluded_dirs = set(exclude_dirs or [])
        files_by_language: dict[str, list[Path]] = {}

        for file_path in root_path.rglob("*"):
            # Skip directories and excluded paths
            if file_path.is_dir():
                continue

            # Check if in excluded directory
            if any(excluded in file_path.parts for excluded in excluded_dirs):
                continue

            # Detect language
            language = self.detect_language(file_path)
            if language:
                if language not in files_by_language:
                    files_by_language[language] = []
                files_by_language[language].append(file_path)

        self.logger.info(
            "Files grouped by language",
            languages=list(files_by_language.keys()),
            total_files=sum(len(files) for files in files_by_language.values()),
        )

        return files_by_language

    def validate_language_list(self, languages: list[str]) -> dict[str, str]:
        """
        Validate a list of language names.

        Args:
            languages: List of language names to validate

        Returns:
            Dict mapping language name to validation message (empty if valid)
        """
        validation_results: dict[str, str] = {}

        for language in languages:
            if language not in LANGUAGE_REGISTRY:
                validation_results[language] = "Unknown language"
            else:
                config = LANGUAGE_REGISTRY[language]
                if config.support_level == LanguageSupport.UNSUPPORTED:
                    validation_results[language] = "Unsupported language"
                elif config.support_level == LanguageSupport.EXPERIMENTAL:
                    validation_results[language] = "Warning: Experimental support"
                else:
                    validation_results[language] = ""  # Valid

        return validation_results

    def get_supported_languages(
        self,
        include_experimental: bool = False,
    ) -> list[str]:
        """
        Get list of supported language names.

        Args:
            include_experimental: Whether to include experimental languages

        Returns:
            List of language names
        """
        languages: list[str] = []
        for lang_name, config in LANGUAGE_REGISTRY.items():
            if config.support_level == LanguageSupport.UNSUPPORTED:
                continue
            if config.support_level == LanguageSupport.EXPERIMENTAL and not include_experimental:
                continue
            languages.append(lang_name)

        return sorted(languages)

    def get_language_stats(self, file_paths: list[Path]) -> dict[str, int]:
        """
        Get statistics on languages in a list of files.

        Args:
            file_paths: List of file paths to analyze

        Returns:
            Dict mapping language names to file counts
        """
        stats: dict[str, int] = {}
        for file_path in file_paths:
            language = self.detect_language(file_path)
            if language:
                stats[language] = stats.get(language, 0) + 1

        return stats


def get_recommended_languages_for_repo(repo_path: Path) -> list[str]:
    """
    Analyze a repository and recommend which languages to index.

    Args:
        repo_path: Path to repository

    Returns:
        List of recommended language names
    """
    detector = LanguageDetector()
    files_by_language = detector.get_files_by_language(
        repo_path,
        exclude_dirs=[
            'node_modules', '__pycache__', '.git', 'venv', '.venv',
            'dist', 'build', '.pytest_cache', '.mypy_cache',
        ]
    )

    # Recommend languages with at least 5 files
    recommended = [
        lang for lang, files in files_by_language.items()
        if len(files) >= 5
    ]

    return sorted(recommended)
