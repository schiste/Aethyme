"""Scaffold i18n translation calls for JSX-based UI code."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from .._log import get_logger
from .base import BaseFixer

logger = get_logger()


class I18nScaffolder(BaseFixer):
    """Add i18n scaffolding to hardcoded JSX strings."""

    JSX_TEXT_PATTERN = re.compile(r">\s*([A-Z][^<>{}\n]{3,50})\s*<")
    STRING_LITERAL_PATTERN = re.compile(
        r"(?:title|label|placeholder|text|message|description|alt)\s*[=:]\s*[\"']([^\"']{3,})[\"']"
    )
    I18N_PATTERNS = [
        re.compile(r"\bt\s*\("),
        re.compile(r"\btranslate\s*\("),
        re.compile(r"\bi18n\b"),
        re.compile(r"\$t\s*\("),
        re.compile(r"\bformatMessage\s*\("),
    ]

    def get_fix_type(self) -> str:
        return "i18n_scaffold"

    def can_fix(self, file_path: Path) -> bool:
        return file_path.suffix.lower() in {".tsx", ".jsx", ".vue"}

    def fix(self, file_path: Path, content: str) -> str | None:
        if self._has_i18n(content):
            return None
        new_content, changes_made = self._fix_jsx_strings(content, file_path)
        return new_content if changes_made else None

    def _fix_jsx_strings(self, content: str, file_path: Path) -> tuple[str, bool]:
        new_content = content
        changes_made = False
        replacements: list[tuple[str, str, str, str]] = []

        for match in self.JSX_TEXT_PATTERN.finditer(content):
            text = match.group(1).strip()
            if self._is_likely_code(text):
                continue
            key = self._generate_i18n_key(text, file_path)
            old_text = f">{match.group(1)}<"
            replacements.append((old_text, f">{{t(\"{key}\")}}<", key, text))

        for match in self.STRING_LITERAL_PATTERN.finditer(content):
            text = match.group(1).strip()
            if self._is_likely_code(text):
                continue
            key = self._generate_i18n_key(text, file_path)
            old_literal = match.group(0)
            prop_name = old_literal.split("=")[0].strip()
            replacements.append((old_literal, f'{prop_name}={{t("{key}")}}', key, text))

        for old, new, key, text in replacements:
            if old not in new_content:
                continue
            new_content = new_content.replace(old, new, 1)
            changes_made = True
            logger.debug("Added i18n call", file=str(file_path), key=key, text=text[:50])

        if changes_made and "import" in new_content:
            import_statement = "import { useTranslation } from 'react-i18next';\n"
            first_import = re.search(r"^import\s", new_content, re.MULTILINE)
            if first_import and import_statement not in new_content:
                new_content = new_content[: first_import.start()] + import_statement + new_content[first_import.start() :]

            func_match = re.search(
                r"(export\s+(?:default\s+)?function\s+\w+|const\s+\w+\s*=\s*\([^)]*\)\s*=>)",
                new_content,
            )
            if func_match:
                start = func_match.end()
                brace_match = re.search(r"\{", new_content[start:])
                if brace_match:
                    pos = start + brace_match.end()
                    hook_statement = "\n  const { t } = useTranslation();\n"
                    if hook_statement not in new_content:
                        new_content = new_content[:pos] + hook_statement + new_content[pos:]

        return new_content, changes_made

    def _is_likely_code(self, text: str) -> bool:
        if len(text) < 3 or len(text) > 100:
            return True
        code_indicators = ["${", "{{", "{", "}", "(", ")", "function", "const", "let", "var", "===", "!==", "&&", "||"]
        if any(indicator in text for indicator in code_indicators):
            return True
        if text.islower() and " " not in text:
            return True
        if re.match(r"^[a-z]+[A-Z]", text) or "_" in text:
            return True
        return False

    def _has_i18n(self, content: str) -> bool:
        return any(pattern.search(content) for pattern in self.I18N_PATTERNS)

    def _generate_i18n_key(self, text: str, file_path: Path) -> str:
        namespace = file_path.stem.lower()
        key = re.sub(r"[^\w\s]", "", text.lower())
        key = re.sub(r"\s+", "_", key)
        if len(key) > 40:
            key = "_".join(key.split("_")[:4])
        return f"{namespace}.{key}"

    def find_hardcoded_strings(self) -> list[dict[str, Any]]:
        hardcoded: list[dict[str, Any]] = []
        for file_path in self.repo_path.rglob("*"):
            if not self.can_fix(file_path):
                continue
            try:
                with open(file_path, encoding="utf-8") as handle:
                    content = handle.read()
                if self._has_i18n(content):
                    continue
                for match in self.JSX_TEXT_PATTERN.finditer(content):
                    text = match.group(1).strip()
                    if self._is_likely_code(text):
                        continue
                    hardcoded.append(
                        {
                            "file": str(file_path.relative_to(self.repo_path)),
                            "line": content[: match.start()].count("\n") + 1,
                            "text": text,
                            "type": "jsx_text",
                        }
                    )
                for match in self.STRING_LITERAL_PATTERN.finditer(content):
                    text = match.group(1).strip()
                    if self._is_likely_code(text):
                        continue
                    hardcoded.append(
                        {
                            "file": str(file_path.relative_to(self.repo_path)),
                            "line": content[: match.start()].count("\n") + 1,
                            "text": text,
                            "type": "prop",
                        }
                    )
            except Exception:
                continue
        return hardcoded
