"""Parser registry for managing language-specific parsers."""

from typing import Dict, Optional, Type
from .base_parser import BaseParser, ParseResult
from ..language_detector import Language, LanguageDetector


class ParserRegistry:
    """Registry for language-specific parsers."""

    _parsers: Dict[Language, Type[BaseParser]] = {}
    _instances: Dict[Language, BaseParser] = {}

    @classmethod
    def register(cls, language: Language, parser_class: Type[BaseParser]):
        """Register a parser for a language.

        Args:
            language: The programming language
            parser_class: Parser class (not instance)
        """
        cls._parsers[language] = parser_class

    @classmethod
    def get_parser(cls, language: Language) -> Optional[BaseParser]:
        """Get a parser instance for a language.

        Args:
            language: The programming language

        Returns:
            Parser instance or None if not registered
        """
        if language not in cls._parsers:
            return None

        # Return cached instance if available
        if language in cls._instances:
            return cls._instances[language]

        # Create new instance
        parser_class = cls._parsers[language]
        parser_instance = parser_class()
        cls._instances[language] = parser_instance

        return parser_instance

    @classmethod
    def parse_file(
        cls, file_path: str, content: str, language: Optional[Language] = None
    ) -> Optional[ParseResult]:
        """Parse a file using the appropriate language parser.

        Args:
            file_path: Path to the source file
            content: File content
            language: Optional language hint (will auto-detect if not provided)

        Returns:
            ParseResult or None if no parser available
        """
        # Auto-detect language if not provided
        if language is None:
            language = LanguageDetector.detect(file_path, content)

        # Get parser for this language
        parser = cls.get_parser(language)
        if not parser:
            return None

        # Parse the file
        try:
            result = parser.parse_file(file_path, content)
            return result
        except Exception as e:
            # Return error result
            return ParseResult(
                file_path=file_path,
                language=language.value,
                symbols=[],
                relationships=[],
                imports=[],
                errors=[f"Parser error: {str(e)}"],
            )

    @classmethod
    def get_supported_languages(cls) -> list[Language]:
        """Get list of supported languages.

        Returns:
            List of supported languages
        """
        return list(cls._parsers.keys())

    @classmethod
    def is_supported(cls, language: Language) -> bool:
        """Check if a language is supported.

        Args:
            language: The programming language

        Returns:
            True if supported, False otherwise
        """
        return language in cls._parsers


# Auto-register available parsers
def _register_default_parsers():
    """Register all available parsers."""
    try:
        from .java_parser import JavaParser
        ParserRegistry.register(Language.JAVA, JavaParser)
    except ImportError:
        pass

    try:
        from .ruby_parser import RubyParser
        ParserRegistry.register(Language.RUBY, RubyParser)
    except ImportError:
        pass

    try:
        from .rust_parser import RustParser
        ParserRegistry.register(Language.RUST, RustParser)
    except ImportError:
        pass

    # Note: Python, Go, JavaScript/TypeScript parsers would be added here
    # For now, they might use the existing SCIP-based parsing


# Register parsers on module import
_register_default_parsers()
