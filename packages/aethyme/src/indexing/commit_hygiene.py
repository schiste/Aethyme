from __future__ import annotations

import re
from dataclasses import dataclass

SUBSTANTIVE_TYPES = {"fix", "feat", "refactor", "perf"}
ALLOWED_TYPES = (
    "fix",
    "feat",
    "refactor",
    "perf",
    "test",
    "docs",
    "build",
    "chore",
    "revert",
)
REQUIRED_SECTIONS = ("Problem", "Decision", "Rationale", "Validation")
OPTIONAL_SECTIONS = ("Alternatives considered", "Risks", "Follow-up", "Memory")
KNOWN_SECTIONS = REQUIRED_SECTIONS + OPTIONAL_SECTIONS
SUBJECT_PATTERN = re.compile(
    r"^(?P<type>[a-z]+)(?:\((?P<scope>[^)]+)\))?: (?P<summary>.+)$"
)


@dataclass(frozen=True)
class ParsedSubject:
    commit_type: str
    scope: str | None
    summary: str


def default_template(commit_type: str = "fix", scope: str = "scope") -> str:
    normalized_type = commit_type if commit_type in ALLOWED_TYPES else "fix"
    lines = [
        f"{normalized_type}({scope}): short summary",
        "",
        "Problem:",
        "...",
    ]
    if normalized_type in SUBSTANTIVE_TYPES:
        lines.extend(
            [
                "",
                "Decision:",
                "...",
                "",
                "Rationale:",
                "...",
                "",
                "Validation:",
                "- ...",
            ]
        )
    else:
        lines.extend(
            [
                "",
                "Decision:",
                "...",
                "",
                "Validation:",
                "- ...",
            ]
        )
    lines.extend(
        [
            "",
            "Alternatives considered:",
            "- ...",
            "",
            "Risks:",
            "- ...",
            "",
            "Memory:",
            "...",
        ]
    )
    return "\n".join(lines) + "\n"


def lint_commit_message(message: str) -> dict[str, object]:
    stripped = message.strip("\n")
    errors: list[str] = []
    warnings: list[str] = []

    if not stripped.strip():
        return {
            "ok": False,
            "errors": ["Commit message is empty."],
            "warnings": [],
            "subject": None,
            "sections": {},
            "required_sections": [],
            "recognized_sections": [],
            "body_required": False,
            "memory_candidates": [],
        }

    lines = stripped.splitlines()
    subject_line = lines[0].strip()
    subject = _parse_subject(subject_line, errors)
    sections = _parse_sections(lines[1:])

    if len(subject_line) > 72:
        warnings.append(
            f"Subject is {len(subject_line)} characters; prefer 72 or fewer."
        )

    if subject is not None and subject.scope is None:
        warnings.append(
            "Subject has no scope; prefer `type(scope): summary` for durable routing."
        )

    required_sections: tuple[str, ...] = ()
    body_required = False
    if subject is not None:
        body_required = subject.commit_type in SUBSTANTIVE_TYPES
        if body_required:
            required_sections = REQUIRED_SECTIONS
            if not any(value.strip() for value in sections.values()):
                errors.append(
                    "Structured body is required for substantive commits: "
                    + ", ".join(REQUIRED_SECTIONS)
                    + "."
                )
        else:
            required_sections = ("Problem", "Decision", "Validation")

    for section_name in required_sections:
        if section_name not in sections:
            errors.append(f"Missing required section: {section_name}.")
            continue
        if not sections[section_name].strip():
            errors.append(f"Section `{section_name}` must not be empty.")

    memory_candidates = _memory_candidates(sections)
    return {
        "ok": not errors,
        "errors": errors,
        "warnings": warnings,
        "subject": (
            None
            if subject is None
            else {
                "type": subject.commit_type,
                "scope": subject.scope,
                "summary": subject.summary,
            }
        ),
        "sections": sections,
        "recognized_sections": list(sections.keys()),
        "required_sections": list(required_sections),
        "body_required": body_required,
        "memory_candidates": memory_candidates,
    }


def _parse_subject(subject_line: str, errors: list[str]) -> ParsedSubject | None:
    match = SUBJECT_PATTERN.match(subject_line)
    if not match:
        errors.append(
            "Subject must match `type(scope): summary` or `type: summary`."
        )
        return None
    commit_type = match.group("type")
    if commit_type not in ALLOWED_TYPES:
        errors.append(
            f"Unsupported commit type `{commit_type}`. Allowed types: {', '.join(ALLOWED_TYPES)}."
        )
        return None
    summary = match.group("summary").strip()
    if not summary:
        errors.append("Subject summary must not be empty.")
        return None
    scope = match.group("scope")
    return ParsedSubject(
        commit_type=commit_type,
        scope=scope.strip() if scope else None,
        summary=summary,
    )


def _parse_sections(body_lines: list[str]) -> dict[str, str]:
    sections: dict[str, list[str]] = {}
    current_section: str | None = None
    for raw_line in body_lines:
        line = raw_line.rstrip()
        if line in {f"{section}:" for section in KNOWN_SECTIONS}:
            current_section = line[:-1]
            sections.setdefault(current_section, [])
            continue
        if current_section is None:
            continue
        sections[current_section].append(line)

    normalized: dict[str, str] = {}
    for name, content_lines in sections.items():
        text = "\n".join(content_lines).strip()
        normalized[name] = text
    return normalized


def _memory_candidates(sections: dict[str, str]) -> list[dict[str, str]]:
    candidates: list[dict[str, str]] = []
    if sections.get("Decision"):
        candidates.append(
            {
                "type": "decision",
                "source_section": "Decision",
                "summary": _first_line(sections["Decision"]),
            }
        )
    if sections.get("Memory"):
        candidates.append(
            {
                "type": "memory-note",
                "source_section": "Memory",
                "summary": _first_line(sections["Memory"]),
            }
        )
    if sections.get("Risks"):
        candidates.append(
            {
                "type": "gotcha",
                "source_section": "Risks",
                "summary": _first_line(sections["Risks"]),
            }
        )
    if sections.get("Validation"):
        candidates.append(
            {
                "type": "validation-rule",
                "source_section": "Validation",
                "summary": _first_line(sections["Validation"]),
            }
        )
    return candidates


def _first_line(text: str) -> str:
    for line in text.splitlines():
        stripped = line.strip()
        if stripped:
            return stripped.removeprefix("- ").strip()
    return ""
