"""Fixer modules for common codebase issues."""

from .docs_regenerator import DocsRegenerator
from .format_fixer import FormatFixer
from .i18n_scaffolder import I18nScaffolder
from .link_fixer import LinkFixer
from .selector_inserter import SelectorInserter

__all__ = [
    "DocsRegenerator",
    "LinkFixer",
    "SelectorInserter",
    "I18nScaffolder",
    "FormatFixer",
]
