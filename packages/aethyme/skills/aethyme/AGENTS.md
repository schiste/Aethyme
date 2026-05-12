# Agent Instructions

This repository is **Aethyme-enhanced**. For navigation, caller tracing,
dead-code analysis, or task localization, prefer Aethyme's high-level
Explore surface before brute-force grep. It is bounded for responsiveness,
returns answer-json with confidence and verification steps, and degrades
gracefully on large repos.

If `.codex/skills/repo-onboarding/SKILL.md` or
`.claude/skills/repo-onboarding/SKILL.md` exists, load that first when the
repository is unfamiliar or the request asks for repo overview, setup,
entrypoints, or where to begin.

## Quick start (any agent)

```bash
AETHYME_ROOT="{{AETHYME_ROOT}}"
"$AETHYME_ROOT/rust/target/release/aethyme" explore \
    --repo "$PWD" --request "<your task>" --format answer-json
```

Do not run `python -m src.cli explore`; the Python `explore` subcommand was
removed. Use the native binary above for Explore, and use the Python CLI for
`graph`, `task`, `intents`, `facts`, and `analyze`.

Read `trust_policy` and `safe_to_use_as_answer` first. Use `answer[]` as the
primary result only when `safe_to_use_as_answer` is true. Otherwise treat
`answer[]` and `navigation_hints[]` as a ranked investigation plan and
follow `verification_steps[]` before concluding.

The default detail is `compact`. Use `--detail standard` or `--detail full`
only when the task needs more evidence payload — they trade tokens for
breadth.

## Detailed reference

Same content lives at both of these per-product skill paths:

- `.claude/skills/aethyme/SKILL.md` — Claude Skills convention
- `.codex/skills/aethyme/SKILL.md` — Codex skills convention
- `.claude/skills/repo-onboarding/SKILL.md` — generated repo-specific orientation
- `.codex/skills/repo-onboarding/SKILL.md` — generated repo-specific orientation

Read whichever your agent surface auto-loads. The files are identical and
contain: full intent catalog, output schema, dead-code workflow, scope-first
analyzer paths, and trust-policy semantics.

## Verifying this enhancement

To confirm the enhancement is intact in this repository:

```bash
"$AETHYME_ROOT/.venv/bin/python" -m src.cli enhance verify --repo "$PWD"
```

Returns nonzero if any of `AGENTS.md`, `CLAUDE.md`, `.claude/skills/aethyme/SKILL.md`,
`.codex/skills/aethyme/SKILL.md`, `.claude/skills/repo-onboarding/SKILL.md`, or
`.codex/skills/repo-onboarding/SKILL.md` is missing or has unsubstituted placeholders.
