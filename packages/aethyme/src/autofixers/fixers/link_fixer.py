"""Fix absolute documentation links by converting them to repo-relative paths."""

from __future__ import annotations

import os
import re
from pathlib import Path

from .._log import get_logger
from .base import BaseFixer

logger = get_logger()


class LinkFixer(BaseFixer):
    """Convert internal absolute links to relative links in text docs."""

    MARKDOWN_LINK_PATTERN = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
    INTERNAL_DOMAINS = ["localhost", "127.0.0.1", "github.com"]

    def get_fix_type(self) -> str:
        return "link_fix"

    def can_fix(self, file_path: Path) -> bool:
        return file_path.suffix.lower() in {".md", ".mdx", ".rst", ".txt"}

    def fix(self, file_path: Path, content: str) -> str | None:
        if "[" not in content or "(" not in content:
            return None

        new_content = content
        changes_made = False
        for match in self.MARKDOWN_LINK_PATTERN.finditer(content):
            link_text = match.group(1)
            link_url = match.group(2)
            if (
                link_url.startswith("#")
                or link_url.startswith("./")
                or link_url.startswith("../")
                or link_url.startswith("http://") and not any(domain in link_url for domain in self.INTERNAL_DOMAINS)
                or link_url.startswith("https://") and not any(domain in link_url for domain in self.INTERNAL_DOMAINS)
            ):
                continue

            new_url = self._convert_to_relative(file_path, link_url)
            if not new_url or new_url == link_url:
                continue

            old_link = f"[{link_text}]({link_url})"
            new_link = f"[{link_text}]({new_url})"
            new_content = new_content.replace(old_link, new_link)
            changes_made = True
            logger.debug("Converted link", file=str(file_path), old=link_url, new=new_url)

        return new_content if changes_made else None

    def _convert_to_relative(self, current_file: Path, link_url: str) -> str | None:
        url = link_url
        for protocol in ["http://", "https://", "file://"]:
            if url.startswith(protocol):
                url = url[len(protocol) :]

        for domain in self.INTERNAL_DOMAINS:
            if url.startswith(domain):
                parts = url.split("/", 1)
                if len(parts) > 1:
                    url = "/" + parts[1]
                break

        if not url.startswith("/"):
            return None

        try:
            target_path = self.repo_path / url.lstrip("/")
            if not target_path.exists():
                return None
            relative = Path(os.path.relpath(target_path, current_file.parent))
            return str(relative).replace("\\", "/")
        except Exception as exc:
            logger.debug("Could not convert link", url=link_url, error=str(exc))
            return None
