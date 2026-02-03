"""Scaffolds i18n translation stubs for hardcoded strings."""

import re
from pathlib import Path
from typing import Optional, List, Dict, Tuple
import json
import structlog

from .base import BaseFixer

logger = structlog.get_logger()


class I18nScaffolder(BaseFixer):
    """Adds i18n translation scaffolding for hardcoded user-facing strings."""

    # Patterns for finding hardcoded strings in JSX
    JSX_TEXT_PATTERN = re.compile(r'>\s*([A-Z][^<>{}\n]{3,50})\s*<')

    # String literals that should be translated
    STRING_LITERAL_PATTERN = re.compile(
        r'(?:title|label|placeholder|text|message|description|alt)\s*[=:]\s*["\']([^"\']{3,})["\']'
    )

    # Common i18n function names
    I18N_FUNCTIONS = ['t', 'translate', 'i18n', '$t', 'formatMessage']

    def get_fix_type(self) -> str:
        return "i18n_scaffold"

    def can_fix(self, file_path: Path) -> bool:
        """Only fix source code files."""
        return file_path.suffix.lower() in {
            '.tsx', '.jsx', '.ts', '.js', '.vue', '.py'
        }

    def fix(self, file_path: Path, content: str) -> Optional[str]:
        """Add i18n scaffolding to hardcoded strings."""
        # Check if file already uses i18n
        if any(func in content for func in self.I18N_FUNCTIONS):
            # File already uses i18n, be conservative
            return None

        new_content = content
        changes_made = False

        # Process based on file type
        if file_path.suffix in {'.tsx', '.jsx', '.vue'}:
            new_content, changed = self._fix_jsx_strings(content, file_path)
            changes_made = changed

        elif file_path.suffix in {'.ts', '.js'}:
            new_content, changed = self._fix_js_strings(content, file_path)
            changes_made = changed

        elif file_path.suffix == '.py':
            new_content, changed = self._fix_python_strings(content, file_path)
            changes_made = changed

        return new_content if changes_made else None

    def _fix_jsx_strings(self, content: str, file_path: Path) -> Tuple[str, bool]:
        """Fix hardcoded strings in JSX."""
        new_content = content
        changes_made = False
        replacements = []

        # Find JSX text content
        for match in self.JSX_TEXT_PATTERN.finditer(content):
            text = match.group(1).strip()

            # Skip if looks like code or variable
            if self._is_likely_code(text):
                continue

            # Generate translation key
            key = self._generate_i18n_key(text, file_path)

            # Replace with i18n call
            old_text = f'>{match.group(0).split(">")[1].split("<")[0]}<'
            new_text = f'>{{t("{key}")}}<'

            replacements.append((old_text, new_text, key, text))

        # Find string literals in props
        for match in self.STRING_LITERAL_PATTERN.finditer(content):
            text = match.group(1).strip()

            if self._is_likely_code(text):
                continue

            key = self._generate_i18n_key(text, file_path)

            # Replace with i18n call
            old_literal = match.group(0)
            prop_name = old_literal.split('=')[0].strip()
            new_literal = f'{prop_name}={{t("{key}")}}'

            replacements.append((old_literal, new_literal, key, text))

        # Apply replacements
        for old, new, key, text in replacements:
            if old in new_content:
                new_content = new_content.replace(old, new, 1)
                changes_made = True

                logger.debug(
                    "Added i18n call",
                    file=str(file_path),
                    key=key,
                    text=text[:50],
                )

        # Add import if changes were made
        if changes_made and 'import' in new_content:
            # Add i18n import at top
            import_statement = "import { useTranslation } from 'react-i18next';\n"

            # Find first import
            first_import = re.search(r'^import\s', new_content, re.MULTILINE)
            if first_import and import_statement not in new_content:
                # Insert before first import
                pos = first_import.start()
                new_content = (
                    new_content[:pos] +
                    import_statement +
                    new_content[pos:]
                )

            # Add hook usage in component
            # Look for function component
            func_match = re.search(
                r'(export\s+(?:default\s+)?function\s+\w+|const\s+\w+\s*=\s*\([^)]*\)\s*=>)',
                new_content
            )
            if func_match:
                # Find opening brace
                start = func_match.end()
                brace_match = re.search(r'\{', new_content[start:])
                if brace_match:
                    pos = start + brace_match.end()
                    hook_statement = "\n  const { t } = useTranslation();\n"
                    if hook_statement not in new_content:
                        new_content = (
                            new_content[:pos] +
                            hook_statement +
                            new_content[pos:]
                        )

        return new_content, changes_made

    def _fix_js_strings(self, content: str, file_path: Path) -> Tuple[str, bool]:
        """Fix hardcoded strings in JS/TS."""
        # Similar to JSX but more conservative
        return content, False

    def _fix_python_strings(self, content: str, file_path: Path) -> Tuple[str, bool]:
        """Fix hardcoded strings in Python."""
        new_content = content
        changes_made = False

        # Look for user-facing strings
        # This is a simplified version - production would be more sophisticated
        string_pattern = re.compile(
            r'(message|text|label|title|description)\s*=\s*["\']([^"\']{5,})["\']'
        )

        for match in string_pattern.finditer(content):
            text = match.group(2)

            if self._is_likely_code(text):
                continue

            key = self._generate_i18n_key(text, file_path)

            # Replace with gettext call
            old = match.group(0)
            prop = match.group(1)
            new = f'{prop} = _("{key}")'

            if old in new_content:
                new_content = new_content.replace(old, new, 1)
                changes_made = True

        # Add gettext import if changes made
        if changes_made:
            import_statement = "from django.utils.translation import gettext as _\n"
            if import_statement not in new_content and 'import' in new_content:
                # Add at top
                first_import = re.search(r'^(?:from|import)\s', new_content, re.MULTILINE)
                if first_import:
                    pos = first_import.start()
                    new_content = (
                        new_content[:pos] +
                        import_statement +
                        new_content[pos:]
                    )

        return new_content, changes_made

    def _is_likely_code(self, text: str) -> bool:
        """Check if text is likely code rather than user-facing string."""
        # Skip very short or very long strings
        if len(text) < 3 or len(text) > 100:
            return True

        # Skip if contains code-like characters
        code_indicators = [
            '${', '{{', '{', '}', '(', ')',  # Template/code syntax
            'function', 'const', 'let', 'var',  # Keywords
            '===', '!==', '&&', '||',  # Operators
        ]

        if any(ind in text for ind in code_indicators):
            return True

        # Skip if all lowercase (likely a key or identifier)
        if text.islower() and ' ' not in text:
            return True

        # Skip if camelCase or snake_case
        if re.match(r'^[a-z]+[A-Z]', text) or '_' in text:
            return True

        return False

    def _generate_i18n_key(self, text: str, file_path: Path) -> str:
        """Generate an i18n key from text."""
        # Use file/component name as namespace
        namespace = file_path.stem.lower()

        # Convert text to key format
        key = text.lower()

        # Remove punctuation
        key = re.sub(r'[^\w\s]', '', key)

        # Replace spaces with underscores
        key = re.sub(r'\s+', '_', key)

        # Truncate if too long
        if len(key) > 40:
            words = key.split('_')
            key = '_'.join(words[:4])  # First 4 words

        return f"{namespace}.{key}"

    def generate_translation_file(
        self,
        output_path: Path,
        language: str = 'en'
    ) -> Dict[str, str]:
        """Generate a translation JSON file with all keys found."""
        translations = {}

        for file_path in self.repo_path.rglob("*"):
            if not self.can_fix(file_path):
                continue

            try:
                with open(file_path, 'r', encoding='utf-8') as f:
                    content = f.read()

                # Extract all i18n keys
                key_pattern = re.compile(r't\(["\']([^"\']+)["\']\)')
                for match in key_pattern.finditer(content):
                    key = match.group(1)
                    if key not in translations:
                        # Use key as default translation
                        translations[key] = key.split('.')[-1].replace('_', ' ').title()

            except Exception:
                pass

        # Write to file
        if translations:
            with open(output_path, 'w', encoding='utf-8') as f:
                json.dump({language: translations}, f, indent=2, ensure_ascii=False)

            logger.info(
                "Generated translation file",
                path=str(output_path),
                keys=len(translations),
            )

        return translations

    def find_hardcoded_strings(self) -> List[Dict[str, any]]:
        """Find all hardcoded user-facing strings."""
        hardcoded = []

        for file_path in self.repo_path.rglob("*"):
            if not self.can_fix(file_path):
                continue

            try:
                with open(file_path, 'r', encoding='utf-8') as f:
                    content = f.read()

                # Skip if already uses i18n
                if any(func in content for func in self.I18N_FUNCTIONS):
                    continue

                # Find JSX text
                for match in self.JSX_TEXT_PATTERN.finditer(content):
                    text = match.group(1).strip()
                    if not self._is_likely_code(text):
                        line_num = content[:match.start()].count('\n') + 1
                        hardcoded.append({
                            "file": str(file_path.relative_to(self.repo_path)),
                            "line": line_num,
                            "text": text,
                            "type": "jsx_text",
                        })

                # Find string literals
                for match in self.STRING_LITERAL_PATTERN.finditer(content):
                    text = match.group(1).strip()
                    if not self._is_likely_code(text):
                        line_num = content[:match.start()].count('\n') + 1
                        hardcoded.append({
                            "file": str(file_path.relative_to(self.repo_path)),
                            "line": line_num,
                            "text": text,
                            "type": "prop",
                        })

            except Exception:
                pass

        return hardcoded
