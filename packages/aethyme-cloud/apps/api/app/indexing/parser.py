"""File parsing and analysis utilities."""

import hashlib
from typing import List, Optional
from dataclasses import dataclass
from pathlib import Path

from app.core.git import FileInfo, GitRepository
from app.indexing.language_detector import LanguageDetector


@dataclass
class ParsedFile:
    """Parsed file with metadata and content."""
    path: str
    relative_path: str
    language: str
    size_bytes: int
    line_count: int
    code_lines: int
    comment_lines: int
    blank_lines: int
    content: Optional[str]
    content_hash: str
    extension: str


class FileParser:
    """Parse files and extract metadata."""

    def __init__(self):
        self.git_repo = GitRepository()
        self.language_detector = LanguageDetector()

    def parse_file(
        self,
        file_info: FileInfo,
        content: Optional[str] = None
    ) -> Optional[ParsedFile]:
        """
        Parse a single file.

        Args:
            file_info: File information from Git
            content: Optional file content (will be read if not provided)

        Returns:
            ParsedFile object or None if parsing fails
        """
        try:
            # Skip binary files
            if file_info.is_binary:
                return None

            # Read content if not provided
            if content is None:
                content = self.git_repo.get_file_content(Path(file_info.path))

            # Skip if content couldn't be read
            if content is None:
                return None

            # Detect language
            language = self.language_detector.detect(
                file_info.relative_path,
                content
            )

            # Calculate line metrics
            lines = content.split("\n")
            line_count = len(lines)

            code_lines, comment_lines, blank_lines = self._analyze_lines(
                lines, language
            )

            # Calculate content hash
            content_hash = hashlib.sha256(content.encode("utf-8")).hexdigest()

            return ParsedFile(
                path=file_info.path,
                relative_path=file_info.relative_path,
                language=language,
                size_bytes=file_info.size_bytes,
                line_count=line_count,
                code_lines=code_lines,
                comment_lines=comment_lines,
                blank_lines=blank_lines,
                content=content,
                content_hash=content_hash,
                extension=file_info.extension,
            )

        except Exception:
            return None

    def parse_files(self, file_infos: List[FileInfo]) -> List[ParsedFile]:
        """
        Parse multiple files.

        Args:
            file_infos: List of file information

        Returns:
            List of ParsedFile objects
        """
        parsed_files = []

        for file_info in file_infos:
            parsed = self.parse_file(file_info)
            if parsed:
                parsed_files.append(parsed)

        return parsed_files

    def _analyze_lines(
        self,
        lines: List[str],
        language: str
    ) -> tuple[int, int, int]:
        """
        Analyze lines to count code, comments, and blank lines.

        Args:
            lines: List of code lines
            language: Programming language

        Returns:
            Tuple of (code_lines, comment_lines, blank_lines)
        """
        code_lines = 0
        comment_lines = 0
        blank_lines = 0

        # Get comment syntax for language
        single_line_comment, multi_line_start, multi_line_end = self._get_comment_syntax(language)

        in_multi_line_comment = False

        for line in lines:
            stripped = line.strip()

            # Blank line
            if not stripped:
                blank_lines += 1
                continue

            # Multi-line comment
            if multi_line_start and multi_line_end:
                if multi_line_start in stripped:
                    in_multi_line_comment = True
                if in_multi_line_comment:
                    comment_lines += 1
                    if multi_line_end in stripped:
                        in_multi_line_comment = False
                    continue

            # Single-line comment
            if single_line_comment and stripped.startswith(single_line_comment):
                comment_lines += 1
                continue

            # Code line
            code_lines += 1

        return code_lines, comment_lines, blank_lines

    def _get_comment_syntax(self, language: str) -> tuple[Optional[str], Optional[str], Optional[str]]:
        """
        Get comment syntax for a language.

        Args:
            language: Programming language

        Returns:
            Tuple of (single_line_comment, multi_line_start, multi_line_end)
        """
        # Language-specific comment syntax
        syntax_map = {
            "Python": ("#", '"""', '"""'),
            "JavaScript": ("//", "/*", "*/"),
            "TypeScript": ("//", "/*", "*/"),
            "Java": ("//", "/*", "*/"),
            "C": ("//", "/*", "*/"),
            "C++": ("//", "/*", "*/"),
            "C#": ("//", "/*", "*/"),
            "Go": ("//", "/*", "*/"),
            "Rust": ("//", "/*", "*/"),
            "Swift": ("//", "/*", "*/"),
            "Kotlin": ("//", "/*", "*/"),
            "Scala": ("//", "/*", "*/"),
            "PHP": ("//", "/*", "*/"),
            "Ruby": ("#", "=begin", "=end"),
            "Shell": ("#", None, None),
            "Bash": ("#", None, None),
            "Perl": ("#", "=pod", "=cut"),
            "R": ("#", None, None),
            "SQL": ("--", "/*", "*/"),
            "HTML": (None, "<!--", "-->"),
            "XML": (None, "<!--", "-->"),
            "CSS": (None, "/*", "*/"),
            "SCSS": ("//", "/*", "*/"),
        }

        return syntax_map.get(language, (None, None, None))

    def calculate_repository_stats(self, parsed_files: List[ParsedFile]) -> dict:
        """
        Calculate repository-wide statistics.

        Args:
            parsed_files: List of parsed files

        Returns:
            Dictionary with repository stats
        """
        if not parsed_files:
            return {
                "total_files": 0,
                "total_lines": 0,
                "code_lines": 0,
                "comment_lines": 0,
                "blank_lines": 0,
                "total_size_bytes": 0,
                "languages": {},
                "primary_language": None,
            }

        # Aggregate statistics
        total_files = len(parsed_files)
        total_lines = sum(f.line_count for f in parsed_files)
        code_lines = sum(f.code_lines for f in parsed_files)
        comment_lines = sum(f.comment_lines for f in parsed_files)
        blank_lines = sum(f.blank_lines for f in parsed_files)
        total_size_bytes = sum(f.size_bytes for f in parsed_files)

        # Language statistics
        languages = self.language_detector.get_language_stats(
            [f.language for f in parsed_files]
        )
        primary_language = self.language_detector.get_primary_language(languages)

        return {
            "total_files": total_files,
            "total_lines": total_lines,
            "code_lines": code_lines,
            "comment_lines": comment_lines,
            "blank_lines": blank_lines,
            "total_size_bytes": total_size_bytes,
            "languages": languages,
            "primary_language": primary_language,
        }
