"""Insert data-ui test selectors into React and Vue components."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from .._log import get_logger
from .base import BaseFixer

logger = get_logger()


class SelectorInserter(BaseFixer):
    """Add data-ui attributes to interactive UI elements."""

    INTERACTIVE_ELEMENTS = {
        "button",
        "a",
        "input",
        "select",
        "textarea",
        "Button",
        "Link",
        "Input",
        "Select",
        "TextArea",
    }
    JSX_ELEMENT_PATTERN = re.compile(r"<([\w.]+)(\s+[^>]*?)?(/?>)", re.MULTILINE)

    def get_fix_type(self) -> str:
        return "selector_insert"

    def can_fix(self, file_path: Path) -> bool:
        return file_path.suffix.lower() in {".tsx", ".jsx", ".vue", ".ts", ".js"}

    def fix(self, file_path: Path, content: str) -> str | None:
        if not any(elem in content for elem in self.INTERACTIVE_ELEMENTS):
            return None
        if file_path.suffix in {".tsx", ".jsx"}:
            new_content, changed = self._fix_jsx(content, file_path)
            return new_content if changed else None
        if file_path.suffix == ".vue":
            new_content, changed = self._fix_vue(content, file_path)
            return new_content if changed else None
        return None

    def _fix_jsx(self, content: str, file_path: Path) -> tuple[str, bool]:
        new_content = content
        changes_made = False
        for match in self.JSX_ELEMENT_PATTERN.finditer(content):
            element_name = match.group(1)
            attributes = match.group(2) or ""
            closing = match.group(3)
            base_element = element_name.split(".")[-1]
            if base_element not in self.INTERACTIVE_ELEMENTS:
                continue
            if "data-ui=" in attributes or "data-testid=" in attributes:
                continue

            selector_name = self._generate_selector_name(element_name, attributes, file_path)
            prefix = f"{attributes.rstrip()} " if attributes else " "
            old_element = match.group(0)
            new_element = f"<{element_name}{prefix}data-ui=\"{selector_name}\"{closing}"
            new_content = new_content.replace(old_element, new_element, 1)
            changes_made = True
            logger.debug("Added data-ui selector", file=str(file_path), element=element_name, selector=selector_name)

        return new_content, changes_made

    def _fix_vue(self, content: str, file_path: Path) -> tuple[str, bool]:
        template_match = re.search(r"<template>(.*?)</template>", content, re.DOTALL)
        if not template_match:
            return content, False
        template_content = template_match.group(1)
        new_template, changed = self._fix_jsx(template_content, file_path)
        if changed:
            return content.replace(template_content, new_template), True
        return content, False

    def _generate_selector_name(self, element_name: str, attributes: str, file_path: Path) -> str:
        parts: list[str] = []
        component_name = file_path.stem
        if component_name and component_name != "index":
            parts.append(re.sub(r"(?<!^)(?=[A-Z])", "-", component_name).lower())

        base_element = element_name.split(".")[-1].lower()
        parts.append(base_element)

        for attr_pattern in [
            r'type="([^"]+)"',
            r'name="([^"]+)"',
            r'id="([^"]+)"',
            r'className="([^"]+)"',
            r'class="([^"]+)"',
        ]:
            match = re.search(attr_pattern, attributes)
            if not match:
                continue
            value = match.group(1).split()[0].replace("btn-", "").replace("button-", "")
            if value and value not in parts:
                parts.append(value)
            break

        if len(parts) == 1:
            parts.append("element")
        return "-".join(parts[:3])

    def find_missing_selectors(self) -> list[dict[str, Any]]:
        missing: list[dict[str, Any]] = []
        for file_path in self.repo_path.rglob("*"):
            if not self.can_fix(file_path):
                continue
            try:
                with open(file_path, encoding="utf-8") as handle:
                    content = handle.read()
                for match in self.JSX_ELEMENT_PATTERN.finditer(content):
                    element_name = match.group(1)
                    attributes = match.group(2) or ""
                    base_element = element_name.split(".")[-1]
                    if base_element not in self.INTERACTIVE_ELEMENTS:
                        continue
                    if "data-ui=" in attributes or "data-testid=" in attributes:
                        continue
                    missing.append(
                        {
                            "file": str(file_path.relative_to(self.repo_path)),
                            "line": content[: match.start()].count("\n") + 1,
                            "element": element_name,
                        }
                    )
            except Exception as err:
                logger.debug("Could not scan file", file=str(file_path), error=str(err))
        return missing
