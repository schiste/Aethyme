"""Language detection service for source code files."""

import re
from pathlib import Path
from typing import Optional, Dict, List, Any
from enum import Enum


class Language(str, Enum):
    """Supported programming languages."""

    PYTHON = "python"
    JAVASCRIPT = "javascript"
    TYPESCRIPT = "typescript"
    GO = "go"
    JAVA = "java"
    CSHARP = "csharp"
    RUBY = "ruby"
    RUST = "rust"
    CPP = "cpp"
    C = "c"
    PHP = "php"
    KOTLIN = "kotlin"
    SWIFT = "swift"
    SCALA = "scala"
    UNKNOWN = "unknown"


class LanguageDetector:
    """Detect programming language from file path and content."""

    # File extension to language mapping
    EXTENSION_MAP: Dict[str, Language] = {
        # Python
        ".py": Language.PYTHON,
        ".pyw": Language.PYTHON,
        ".pyx": Language.PYTHON,

        # JavaScript
        ".js": Language.JAVASCRIPT,
        ".jsx": Language.JAVASCRIPT,
        ".mjs": Language.JAVASCRIPT,
        ".cjs": Language.JAVASCRIPT,

        # TypeScript
        ".ts": Language.TYPESCRIPT,
        ".tsx": Language.TYPESCRIPT,

        # Go
        ".go": Language.GO,

        # Java
        ".java": Language.JAVA,

        # C#
        ".cs": Language.CSHARP,

        # Ruby
        ".rb": Language.RUBY,
        ".rake": Language.RUBY,

        # Rust
        ".rs": Language.RUST,

        # C++
        ".cpp": Language.CPP,
        ".cc": Language.CPP,
        ".cxx": Language.CPP,
        ".hpp": Language.CPP,
        ".hxx": Language.CPP,
        ".h": Language.CPP,  # Could be C or C++, content-based detection needed

        # C
        ".c": Language.C,

        # PHP
        ".php": Language.PHP,

        # Kotlin
        ".kt": Language.KOTLIN,
        ".kts": Language.KOTLIN,

        # Swift
        ".swift": Language.SWIFT,

        # Scala
        ".scala": Language.SCALA,
        ".sc": Language.SCALA,
    }

    # Content-based language detection patterns
    CONTENT_PATTERNS: Dict[Language, List[str]] = {
        Language.PYTHON: [
            r"^import\s+\w+",
            r"^from\s+\w+\s+import",
            r"^def\s+\w+\s*\(",
            r"^class\s+\w+\s*:",
        ],
        Language.JAVASCRIPT: [
            r"^const\s+\w+\s*=",
            r"^let\s+\w+\s*=",
            r"^function\s+\w+\s*\(",
            r"console\.log\(",
        ],
        Language.TYPESCRIPT: [
            r"^interface\s+\w+\s*{",
            r"^type\s+\w+\s*=",
            r":\s*\w+\s*[=;]",  # Type annotations
        ],
        Language.GO: [
            r"^package\s+\w+",
            r"^func\s+\w+\s*\(",
            r"^type\s+\w+\s+struct\s*{",
            r"^import\s+\(",
        ],
        Language.JAVA: [
            r"^package\s+[\w.]+\s*;",
            r"^public\s+class\s+\w+",
            r"^import\s+[\w.]+\s*;",
            r"System\.out\.println\(",
        ],
        Language.CSHARP: [
            r"^using\s+[\w.]+\s*;",
            r"^namespace\s+[\w.]+",
            r"^public\s+class\s+\w+",
            r"Console\.WriteLine\(",
        ],
        Language.RUBY: [
            r"^require\s+['\"]",
            r"^def\s+\w+",
            r"^class\s+\w+",
            r"puts\s+",
        ],
        Language.RUST: [
            r"^use\s+[\w:]+\s*;",
            r"^fn\s+\w+\s*\(",
            r"^struct\s+\w+\s*{",
            r"println!\(",
        ],
        Language.CPP: [
            r"^#include\s+<\w+>",
            r"^namespace\s+\w+",
            r"std::",
            r"^class\s+\w+\s*{",
        ],
        Language.C: [
            r"^#include\s+<\w+\.h>",
            r"^int\s+main\s*\(",
            r"printf\(",
        ],
    }

    @classmethod
    def detect_from_path(cls, file_path: str) -> Language:
        """Detect language from file path/extension."""
        path = Path(file_path)
        extension = path.suffix.lower()

        # Check extension map
        if extension in cls.EXTENSION_MAP:
            return cls.EXTENSION_MAP[extension]

        # Check filename (for files like Dockerfile, Makefile)
        filename = path.name.lower()
        if filename in ("dockerfile", "docker-compose.yml", "docker-compose.yaml"):
            return Language.UNKNOWN  # Docker, not a programming language
        if filename in ("makefile", "rakefile"):
            return Language.UNKNOWN  # Build files

        return Language.UNKNOWN

    @classmethod
    def detect_from_content(cls, content: str, hint: Optional[Language] = None) -> Language:
        """Detect language from file content.

        Args:
            content: Source code content
            hint: Optional language hint from file extension

        Returns:
            Detected language
        """
        if not content or not content.strip():
            return hint or Language.UNKNOWN

        # If we have a hint and it's not ambiguous, use it
        if hint and hint != Language.UNKNOWN:
            # For .h files, we need content-based detection
            if hint in (Language.C, Language.CPP):
                pass  # Continue with content-based detection
            else:
                return hint

        # Score each language based on pattern matches
        scores: Dict[Language, int] = {}

        for language, patterns in cls.CONTENT_PATTERNS.items():
            score = 0
            for pattern in patterns:
                matches = re.findall(pattern, content, re.MULTILINE)
                score += len(matches)

            if score > 0:
                scores[language] = score

        # Return language with highest score
        if scores:
            return max(scores, key=scores.get)

        return hint or Language.UNKNOWN

    @classmethod
    def detect(cls, file_path: str, content: Optional[str] = None) -> Language:
        """Detect language from file path and optionally content.

        Args:
            file_path: Path to the source file
            content: Optional file content for content-based detection

        Returns:
            Detected language
        """
        # Start with extension-based detection
        language = cls.detect_from_path(file_path)

        # If we have content and detection is uncertain, use content-based detection
        if content and (language == Language.UNKNOWN or Path(file_path).suffix in (".h", ".hpp")):
            language = cls.detect_from_content(content, hint=language)

        return language

    @classmethod
    def get_language_info(cls, language: Language) -> Dict[str, Any]:
        """Get metadata about a programming language.

        Args:
            language: The programming language

        Returns:
            Dictionary with language metadata
        """
        info = {
            Language.PYTHON: {
                "name": "Python",
                "extensions": [".py", ".pyw", ".pyx"],
                "comment_style": "#",
                "multi_line_comment": ('"""', '"""'),
                "type_system": "dynamic",
                "paradigm": ["object-oriented", "functional", "procedural"],
            },
            Language.JAVASCRIPT: {
                "name": "JavaScript",
                "extensions": [".js", ".jsx", ".mjs"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "dynamic",
                "paradigm": ["object-oriented", "functional", "event-driven"],
            },
            Language.TYPESCRIPT: {
                "name": "TypeScript",
                "extensions": [".ts", ".tsx"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["object-oriented", "functional"],
            },
            Language.GO: {
                "name": "Go",
                "extensions": [".go"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["procedural", "concurrent"],
            },
            Language.JAVA: {
                "name": "Java",
                "extensions": [".java"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["object-oriented"],
            },
            Language.CSHARP: {
                "name": "C#",
                "extensions": [".cs"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["object-oriented", "functional"],
            },
            Language.RUBY: {
                "name": "Ruby",
                "extensions": [".rb", ".rake"],
                "comment_style": "#",
                "multi_line_comment": ("=begin", "=end"),
                "type_system": "dynamic",
                "paradigm": ["object-oriented", "functional"],
            },
            Language.RUST: {
                "name": "Rust",
                "extensions": [".rs"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["functional", "concurrent"],
            },
            Language.CPP: {
                "name": "C++",
                "extensions": [".cpp", ".cc", ".cxx", ".hpp"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["object-oriented", "procedural"],
            },
            Language.C: {
                "name": "C",
                "extensions": [".c", ".h"],
                "comment_style": "//",
                "multi_line_comment": ("/*", "*/"),
                "type_system": "static",
                "paradigm": ["procedural"],
            },
        }

        return info.get(language, {
            "name": language.value.title(),
            "extensions": [],
            "comment_style": "//",
            "type_system": "unknown",
            "paradigm": [],
        })
