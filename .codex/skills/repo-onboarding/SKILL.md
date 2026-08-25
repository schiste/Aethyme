---
name: repo-onboarding
description: Use when starting work in an unfamiliar repository, when the task asks for repo overview, setup, architecture, entrypoints, test commands, or where to begin. Skip for narrow file-scoped edits once the relevant paths are already known.
---

# Repo Onboarding: repair-aethyme-canonical-repository-depl

## When to Use

- Load this skill first when the repository is unfamiliar or the request is broad.
- Recommended when: first task in repo, repo overview, setup or run instructions, architecture or entrypoints, where should I start, broad debugging or feature-localization request.
- Skip when: known file-scoped edit, follow-up inside already identified area, task already localized to concrete files.
- Use `.codex/skills/aethyme/SKILL.md` or `.claude/skills/aethyme/SKILL.md` for Aethyme's short operating contract after orientation; load its `references/` files only when needed.

## Repo Identity

- Kind: `monorepo`
- Languages: `unknown`
- Package manager: `unknown`

## Start Here

- `fast_test`: `cargo test`
- `build`: `cargo build`

## Supporting Commands

- `cargo test` (fast_test; medium confidence from `github-actions`)
- `pytest` (fast_test; medium confidence from `github-actions`)
- `cargo build` (build; medium confidence from `github-actions`)

## Repo Map

- `.github` (automation; automation and CI configuration; high confidence)
- `docs` (docs; documentation area; high confidence)
- `packages` (workspace; workspace-style package container; high confidence)

## Aethyme Recipes

- `aethyme explore --repo "$PWD" --request "<task>" --format answer-json`
  Purpose: Broad repository orientation for a user request
- `aethyme repo inspect "$PWD" --mode brief --json-output`
  Purpose: Quick deterministic repo summary
- `aethyme graph callers "$PWD" "<symbol-or-file>" --json-output`
  Purpose: Trace likely impact before editing

## Freshness

- Snapshot key: `4dc754575977431e11dadcc1fc66604b9fbce935d04d7531bb57e66c23ead622`
- Commit: `9df22342c7d34ed5f4898b50ec68483d63b04109`
- Repo dirty: `True`
- Generated at: `2026-08-25T08:57:30+00:00`
- Overrides applied: `False`
- Sections generated: `repo, commands, areas, entrypoints, caution_zones, navigation_recipes, summon, freshness`
