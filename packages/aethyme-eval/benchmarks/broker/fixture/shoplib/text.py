"""Text helpers."""

import re

_NON_WORD = re.compile(r"[^a-z0-9]+")


def slugify(text):
    """'Blue Mug (Large)' -> 'blue-mug-large'."""
    return _NON_WORD.sub("-", text.lower()).strip("-")


def truncate(text, width, suffix="..."):
    if len(text) <= width:
        return text
    return text[: max(0, width - len(suffix))] + suffix
