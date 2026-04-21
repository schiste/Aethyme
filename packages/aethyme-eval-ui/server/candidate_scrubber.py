"""Scrub model-identifying artifacts from candidate text before judging.

Self-preference bias in LLM judges operates through stylistic familiarity.
Even when the judge prompt never names the generator model, terminal noise
in the candidate (tool-use markers, "Thinking…" blocks, REPL prompts, chat
scaffolding) acts as a signature: a judge that recognizes its own family's
style tends to score it higher (Panickssery 2024, Stureborg 2024).

This module strips those envelope signals without touching content. Each
pattern is applied in one of two modes:

  - STRIP_PREFIX: remove the matched prefix, keep the rest of the line.
    Used for decorative UI ("● Running tool: Read" → "Running tool: Read",
    "Claude: I found the bug" → "I found the bug").

  - DROP_LINE: remove the whole line. Used for lines whose content IS the
    identity signal ("Thinking…", "I am an AI language model...",
    "Bypass Permissions dialog", REPL prompt-only lines).

Kept intentionally conservative: all patterns are line-anchored and do not
scrub mid-line text, so code blocks that mention "claude" / "codex" as
keywords are preserved.

Returns (scrubbed_text, stats) so callers can log how much envelope was
stripped — a sanity check that the scrubber isn't eating content.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum


class Mode(Enum):
    STRIP_PREFIX = "strip-prefix"  # Remove the match, keep rest of line.
    DROP_LINE = "drop-line"        # Remove the whole line.


# Line-anchored patterns, evaluated in order. First match wins per line.
# (regex, mode, label)
_PATTERNS: tuple[tuple[re.Pattern[str], Mode, str], ...] = (
    # Provider boilerplate — identity content, not envelope. Drop entirely.
    (re.compile(
        r"^\s*(?:i\s+am\s+an\s+ai|as\s+an\s+ai\s+(?:language\s+)?model|"
        r"i'?m\s+chatgpt|based\s+on\s+openai|powered\s+by\s+anthropic|"
        r"i\s+am\s+claude|i'?m\s+claude)\b.*",
        re.IGNORECASE,
    ), Mode.DROP_LINE, "provider-boilerplate"),

    # Trust-dialog / Claude Code chrome — pure envelope, drop.
    (re.compile(
        r"^\s*(?:bypass\s+permissions|trust\s+this\s+folder|"
        r"safe\s+to\s+run|continue\s+\(y/n\)|"
        r"\[.*?skip.*?permissions.*?\]|"
        r"press\s+enter\s+to\s+continue)",
        re.IGNORECASE,
    ), Mode.DROP_LINE, "trust-chrome"),

    # Standalone "Thinking…" / "Reasoning…" / "Planning…" lines.
    (re.compile(
        r"^\s*(?:thinking|reasoning|planning)[\s…\.]*$",
        re.IGNORECASE,
    ), Mode.DROP_LINE, "thinking-block"),

    # Model-name chat prefixes: "Claude:", "ChatGPT:", "Codex> ", etc.
    # Keep the content after the prefix — that's what the agent said.
    (re.compile(
        r"^\s*(?:claude|chatgpt|gpt-?\d*(?:\.\d+)?|codex|anthropic|openai|assistant|agent)"
        r"\s*[:>\]]\s+",
        re.IGNORECASE,
    ), Mode.STRIP_PREFIX, "model-prefix"),

    # Claude Code UI bullets: "● Running tool: Read", "⏺ Bash".
    (re.compile(r"^\s*[●⏺∙•]\s+"), Mode.STRIP_PREFIX, "claude-bullet"),

    # REPL prompts — keep the command, strip the prompt glyph.
    (re.compile(r"^\s*[$%>❯#]\s+"), Mode.STRIP_PREFIX, "repl-prompt"),
)


@dataclass
class ScrubStats:
    original_chars: int
    scrubbed_chars: int
    lines_dropped: int
    lines_prefix_stripped: int
    match_counts: dict[str, int]

    @property
    def removed_chars(self) -> int:
        return self.original_chars - self.scrubbed_chars


def scrub_candidate(candidate: str) -> tuple[str, ScrubStats]:
    """Return (scrubbed_candidate, stats).

    Patterns apply in order; first match per line wins. Modes decide
    whether the line is dropped entirely or has its prefix removed.
    """
    if not candidate:
        return "", ScrubStats(0, 0, 0, 0, {})

    original_chars = len(candidate)
    match_counts: dict[str, int] = {}
    kept: list[str] = []
    lines_dropped = 0
    lines_prefix_stripped = 0

    for line in candidate.splitlines():
        matched = False
        for pattern, mode, label in _PATTERNS:
            m = pattern.match(line)
            if not m:
                continue
            match_counts[label] = match_counts.get(label, 0) + 1
            if mode is Mode.DROP_LINE:
                lines_dropped += 1
            else:  # STRIP_PREFIX
                remainder = line[m.end():]
                # Only keep if the remainder has content; otherwise treat
                # as an empty line (drop).
                if remainder.strip():
                    kept.append(remainder)
                    lines_prefix_stripped += 1
                else:
                    lines_dropped += 1
            matched = True
            break

        if not matched:
            kept.append(line)

    scrubbed = "\n".join(kept)
    return scrubbed, ScrubStats(
        original_chars=original_chars,
        scrubbed_chars=len(scrubbed),
        lines_dropped=lines_dropped,
        lines_prefix_stripped=lines_prefix_stripped,
        match_counts=match_counts,
    )
