"""Fixes absolute links in documentation by converting to relative links."""

import re
from pathlib import Path
from typing import Optional
import structlog

from .base import BaseFixer

logger = structlog.get_logger()


class LinkFixer(BaseFixer):
    """Converts absolute links to relative links in documentation."""

    # Markdown link pattern: [text](url)
    MARKDOWN_LINK_PATTERN = re.compile(r'\[([^\]]+)\]\(([^)]+)\)')

    # Common domains that should be converted to relative
    INTERNAL_DOMAINS = [
        'localhost',
        '127.0.0.1',
        'github.com',  # Repository links
    ]

    def get_fix_type(self) -> str:
        return "link_fix"

    def can_fix(self, file_path: Path) -> bool:
        """Only fix markdown and documentation files."""
        return file_path.suffix.lower() in {'.md', '.mdx', '.rst', '.txt'}

    def fix(self, file_path: Path, content: str) -> Optional[str]:
        """Convert absolute links to relative links."""
        if '[' not in content or '(' not in content:
            return None  # No links

        new_content = content
        changes_made = False

        # Find all markdown links
        for match in self.MARKDOWN_LINK_PATTERN.finditer(content):
            link_text = match.group(1)
            link_url = match.group(2)

            # Skip external links, anchors, and already relative links
            if (link_url.startswith('#') or
                link_url.startswith('./') or
                link_url.startswith('../') or
                link_url.startswith('http://') and not any(d in link_url for d in self.INTERNAL_DOMAINS) or
                link_url.startswith('https://') and not any(d in link_url for d in self.INTERNAL_DOMAINS)):
                continue

            # Convert absolute path to relative
            new_url = self._convert_to_relative(file_path, link_url)

            if new_url and new_url != link_url:
                old_link = f'[{link_text}]({link_url})'
                new_link = f'[{link_text}]({new_url})'
                new_content = new_content.replace(old_link, new_link)
                changes_made = True

                logger.debug(
                    "Converted link",
                    file=str(file_path),
                    old=link_url,
                    new=new_url,
                )

        return new_content if changes_made else None

    def _convert_to_relative(self, current_file: Path, link_url: str) -> Optional[str]:
        """Convert an absolute link to relative."""
        # Remove protocol if present
        url = link_url
        for protocol in ['http://', 'https://', 'file://']:
            if url.startswith(protocol):
                url = url[len(protocol):]

        # Remove domain for internal links
        for domain in self.INTERNAL_DOMAINS:
            if url.startswith(domain):
                # Remove domain and get path
                parts = url.split('/', 1)
                if len(parts) > 1:
                    url = '/' + parts[1]
                break

        # If it's an absolute path, make it relative
        if url.startswith('/'):
            # Get path relative to current file
            try:
                # Assume url is relative to repo root
                target_path = self.repo_path / url.lstrip('/')

                if not target_path.exists():
                    # Try without repo path
                    return None

                # Calculate relative path from current file to target
                current_dir = current_file.parent
                relative = Path(os.path.relpath(target_path, current_dir))

                return str(relative).replace('\\', '/')

            except Exception as e:
                logger.debug("Could not convert link", url=link_url, error=str(e))
                return None

        return None

    def fix_html_links(self, content: str) -> str:
        """Fix HTML links in addition to markdown."""
        # HTML link pattern: <a href="url">text</a>
        html_pattern = re.compile(r'<a\s+href="([^"]+)"([^>]*)>(.*?)</a>', re.IGNORECASE)

        def replace_html_link(match):
            url = match.group(1)
            attrs = match.group(2)
            text = match.group(3)

            # Skip external and relative links
            if (url.startswith('#') or
                url.startswith('./') or
                url.startswith('../') or
                (url.startswith('http') and not any(d in url for d in self.INTERNAL_DOMAINS))):
                return match.group(0)

            new_url = url  # TODO: Implement HTML link conversion
            return f'<a href="{new_url}"{attrs}>{text}</a>'

        return html_pattern.sub(replace_html_link, content)


# Need os for path operations
import os
