"""Language detection for source code files."""

from typing import Dict, List, Optional
from pathlib import Path
from collections import Counter


# Extension to language mapping
LANGUAGE_MAP = {
    # Python
    ".py": "Python",
    ".pyw": "Python",
    ".pyx": "Python",

    # JavaScript/TypeScript
    ".js": "JavaScript",
    ".jsx": "JavaScript",
    ".mjs": "JavaScript",
    ".cjs": "JavaScript",
    ".ts": "TypeScript",
    ".tsx": "TypeScript",

    # Web
    ".html": "HTML",
    ".htm": "HTML",
    ".css": "CSS",
    ".scss": "SCSS",
    ".sass": "Sass",
    ".less": "Less",
    ".vue": "Vue",

    # Java/JVM
    ".java": "Java",
    ".kt": "Kotlin",
    ".kts": "Kotlin",
    ".scala": "Scala",
    ".groovy": "Groovy",
    ".gradle": "Gradle",

    # C/C++
    ".c": "C",
    ".h": "C",
    ".cpp": "C++",
    ".cxx": "C++",
    ".cc": "C++",
    ".hpp": "C++",
    ".hxx": "C++",

    # C#/.NET
    ".cs": "C#",
    ".vb": "Visual Basic",
    ".fs": "F#",

    # Go
    ".go": "Go",

    # Rust
    ".rs": "Rust",

    # Ruby
    ".rb": "Ruby",
    ".erb": "Ruby",

    # PHP
    ".php": "PHP",
    ".phtml": "PHP",

    # Swift/Objective-C
    ".swift": "Swift",
    ".m": "Objective-C",
    ".mm": "Objective-C++",

    # Shell
    ".sh": "Shell",
    ".bash": "Bash",
    ".zsh": "Zsh",
    ".fish": "Fish",

    # Markup/Config
    ".md": "Markdown",
    ".markdown": "Markdown",
    ".json": "JSON",
    ".yml": "YAML",
    ".yaml": "YAML",
    ".toml": "TOML",
    ".xml": "XML",
    ".ini": "INI",

    # Database
    ".sql": "SQL",

    # Other
    ".r": "R",
    ".R": "R",
    ".lua": "Lua",
    ".pl": "Perl",
    ".pm": "Perl",
    ".dart": "Dart",
    ".ex": "Elixir",
    ".exs": "Elixir",
    ".erl": "Erlang",
    ".hrl": "Erlang",
    ".clj": "Clojure",
    ".coffee": "CoffeeScript",
    ".vim": "Vim Script",
}

# Special file names
SPECIAL_FILES = {
    "Dockerfile": "Docker",
    "docker-compose.yml": "Docker Compose",
    "Makefile": "Makefile",
    "CMakeLists.txt": "CMake",
    "package.json": "npm",
    "Cargo.toml": "Cargo",
    "go.mod": "Go Module",
    "pom.xml": "Maven",
    "build.gradle": "Gradle",
    "requirements.txt": "pip",
    "Gemfile": "Bundler",
}


class LanguageDetector:
    """Detect programming language of source files."""

    def __init__(self):
        self.language_map = LANGUAGE_MAP
        self.special_files = SPECIAL_FILES

    def detect_by_extension(self, file_path: str) -> Optional[str]:
        """
        Detect language by file extension.

        Args:
            file_path: Path to file

        Returns:
            Language name or None
        """
        path = Path(file_path)

        # Check special file names first
        if path.name in self.special_files:
            return self.special_files[path.name]

        # Check extension
        ext = path.suffix.lower()
        return self.language_map.get(ext)

    def detect_by_content(self, content: str) -> Optional[str]:
        """
        Detect language by analyzing content (shebang, etc.).

        Args:
            content: File content

        Returns:
            Language name or None
        """
        if not content:
            return None

        # Check shebang
        lines = content.split("\n", 1)
        if lines and lines[0].startswith("#!"):
            shebang = lines[0].lower()

            if "python" in shebang:
                return "Python"
            elif "node" in shebang or "javascript" in shebang:
                return "JavaScript"
            elif "ruby" in shebang:
                return "Ruby"
            elif "sh" in shebang or "bash" in shebang:
                return "Shell"
            elif "perl" in shebang:
                return "Perl"
            elif "php" in shebang:
                return "PHP"

        return None

    def detect(self, file_path: str, content: Optional[str] = None) -> str:
        """
        Detect language using multiple methods.

        Args:
            file_path: Path to file
            content: Optional file content

        Returns:
            Language name or "Unknown"
        """
        # Try extension first (fastest)
        lang = self.detect_by_extension(file_path)
        if lang:
            return lang

        # Try content-based detection
        if content:
            lang = self.detect_by_content(content)
            if lang:
                return lang

        return "Unknown"

    def get_language_stats(self, file_languages: List[str]) -> Dict[str, int]:
        """
        Calculate language statistics.

        Args:
            file_languages: List of language names

        Returns:
            Dictionary mapping language to file count
        """
        return dict(Counter(file_languages))

    def get_primary_language(self, language_stats: Dict[str, int]) -> Optional[str]:
        """
        Determine primary language of repository.

        Args:
            language_stats: Language statistics

        Returns:
            Primary language name or None
        """
        if not language_stats:
            return None

        # Exclude non-code languages
        code_langs = {
            lang: count
            for lang, count in language_stats.items()
            if lang not in {"Markdown", "JSON", "YAML", "TOML", "XML", "INI", "Unknown"}
        }

        if not code_langs:
            return None

        # Return language with most files
        return max(code_langs.items(), key=lambda x: x[1])[0]
