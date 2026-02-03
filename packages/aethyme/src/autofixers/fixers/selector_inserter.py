"""Inserts data-ui test selectors into React/Vue components."""

import re
from pathlib import Path
from typing import Optional, List, Tuple
import structlog

from .base import BaseFixer

logger = structlog.get_logger()


class SelectorInserter(BaseFixer):
    """Adds data-ui test selectors to interactive elements."""

    # Elements that should have data-ui attributes
    INTERACTIVE_ELEMENTS = {
        'button', 'a', 'input', 'select', 'textarea',
        'Button', 'Link', 'Input', 'Select', 'TextArea',  # React components
    }

    # Patterns for React/JSX components
    JSX_ELEMENT_PATTERN = re.compile(
        r'<([\w.]+)(\s+[^>]*?)(/?>)',
        re.MULTILINE
    )

    def get_fix_type(self) -> str:
        return "selector_insert"

    def can_fix(self, file_path: Path) -> bool:
        """Only fix React/Vue component files."""
        return file_path.suffix.lower() in {'.tsx', '.jsx', '.vue', '.ts', '.js'}

    def fix(self, file_path: Path, content: str) -> Optional[str]:
        """Add data-ui selectors to elements."""
        # Only process files with interactive elements
        if not any(elem in content for elem in self.INTERACTIVE_ELEMENTS):
            return None

        new_content = content
        changes_made = False

        # Process JSX/TSX
        if file_path.suffix in {'.tsx', '.jsx'}:
            new_content, changed = self._fix_jsx(content, file_path)
            changes_made = changed

        # Process Vue
        elif file_path.suffix == '.vue':
            new_content, changed = self._fix_vue(content, file_path)
            changes_made = changed

        return new_content if changes_made else None

    def _fix_jsx(self, content: str, file_path: Path) -> Tuple[str, bool]:
        """Fix JSX/TSX content."""
        new_content = content
        changes_made = False

        # Find all JSX elements
        for match in self.JSX_ELEMENT_PATTERN.finditer(content):
            element_name = match.group(1)
            attributes = match.group(2) if match.group(2) else ""
            closing = match.group(3)

            # Check if this is an interactive element
            base_element = element_name.split('.')[-1]  # Handle styled.button
            if base_element not in self.INTERACTIVE_ELEMENTS:
                continue

            # Check if already has data-ui attribute
            if 'data-ui=' in attributes or 'data-testid=' in attributes:
                continue

            # Generate selector name
            selector_name = self._generate_selector_name(
                element_name, attributes, file_path
            )

            # Insert data-ui attribute
            new_attributes = attributes.rstrip() + f' data-ui="{selector_name}"'
            new_element = f'<{element_name}{new_attributes}{closing}'
            old_element = match.group(0)

            new_content = new_content.replace(old_element, new_element, 1)
            changes_made = True

            logger.debug(
                "Added data-ui selector",
                file=str(file_path),
                element=element_name,
                selector=selector_name,
            )

        return new_content, changes_made

    def _fix_vue(self, content: str, file_path: Path) -> Tuple[str, bool]:
        """Fix Vue template content."""
        # Extract template section
        template_match = re.search(
            r'<template>(.*?)</template>',
            content,
            re.DOTALL
        )

        if not template_match:
            return content, False

        template_content = template_match.group(1)
        new_template, changed = self._fix_jsx(template_content, file_path)

        if changed:
            new_content = content.replace(template_content, new_template)
            return new_content, True

        return content, False

    def _generate_selector_name(
        self,
        element_name: str,
        attributes: str,
        file_path: Path
    ) -> str:
        """Generate a meaningful selector name."""
        parts = []

        # Use file/component name as prefix
        component_name = file_path.stem
        if component_name and component_name != 'index':
            # Convert from PascalCase to kebab-case
            component_name = re.sub(r'(?<!^)(?=[A-Z])', '-', component_name).lower()
            parts.append(component_name)

        # Add element type
        base_element = element_name.split('.')[-1].lower()
        parts.append(base_element)

        # Try to extract meaningful identifier from attributes
        # Look for common identifying attributes
        for attr_pattern in [
            r'type="([^"]+)"',
            r'name="([^"]+)"',
            r'id="([^"]+)"',
            r'className="([^"]+)"',
            r'class="([^"]+)"',
        ]:
            match = re.search(attr_pattern, attributes)
            if match:
                value = match.group(1)
                # Clean up value
                value = value.split()[0]  # First class if multiple
                value = value.replace('btn-', '').replace('button-', '')
                if value and value not in parts:
                    parts.append(value)
                break

        # Look for text content hint
        text_match = re.search(r'>\s*([A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)\s*<', attributes)
        if text_match and len(parts) < 3:
            text = text_match.group(1).lower().replace(' ', '-')
            parts.append(text)

        # Generate final selector
        if len(parts) == 1:
            parts.append('element')  # Ensure at least 2 parts

        return '-'.join(parts[:3])  # Max 3 parts

    def find_missing_selectors(self) -> List[Dict[str, any]]:
        """Find all interactive elements missing data-ui selectors."""
        missing = []

        for file_path in self.repo_path.rglob("*"):
            if not self.can_fix(file_path):
                continue

            try:
                with open(file_path, 'r', encoding='utf-8') as f:
                    content = f.read()

                # Count interactive elements without selectors
                for match in self.JSX_ELEMENT_PATTERN.finditer(content):
                    element_name = match.group(1)
                    attributes = match.group(2) or ""

                    base_element = element_name.split('.')[-1]
                    if base_element not in self.INTERACTIVE_ELEMENTS:
                        continue

                    if 'data-ui=' not in attributes and 'data-testid=' not in attributes:
                        # Get line number
                        line_num = content[:match.start()].count('\n') + 1

                        missing.append({
                            "file": str(file_path.relative_to(self.repo_path)),
                            "line": line_num,
                            "element": element_name,
                        })

            except Exception as e:
                logger.debug("Could not scan file", file=str(file_path), error=str(e))

        return missing

    def get_coverage_report(self) -> Dict[str, any]:
        """Get coverage report for data-ui selectors."""
        total_elements = 0
        elements_with_selectors = 0

        for file_path in self.repo_path.rglob("*"):
            if not self.can_fix(file_path):
                continue

            try:
                with open(file_path, 'r', encoding='utf-8') as f:
                    content = f.read()

                for match in self.JSX_ELEMENT_PATTERN.finditer(content):
                    element_name = match.group(1)
                    attributes = match.group(2) or ""

                    base_element = element_name.split('.')[-1]
                    if base_element not in self.INTERACTIVE_ELEMENTS:
                        continue

                    total_elements += 1

                    if 'data-ui=' in attributes or 'data-testid=' in attributes:
                        elements_with_selectors += 1

            except Exception:
                pass

        coverage = (
            (elements_with_selectors / total_elements * 100)
            if total_elements > 0
            else 0
        )

        return {
            "total_elements": total_elements,
            "elements_with_selectors": elements_with_selectors,
            "coverage_percent": round(coverage, 2),
        }
