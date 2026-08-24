# Repository deployment contract

Last Updated: 2026-08-24

Aethyme has two distinct installation scopes:

- install `aethyme` and `aethyme-engine-cli` once for the user;
- deploy and commit Aethyme policy separately in every repository where agents
  must use the broker.

From the target repository, run:

```bash
aethyme deploy --repo .
```

This canonical command scaffolds broker configuration, drafts applicable
gates, deploys agent instructions and skills, verifies their shape, and
certifies the complete repository contract. It does not install another copy
of the executable inside the repository.

## Commit the policy

Review and commit:

- `.gitignore`;
- `.aethyme/config.toml` and `.aethyme/gates.toml` when generated;
- `.aethyme/overrides/` when present;
- `.aethyme/generated/onboarding.json` and
  `.aethyme/generated/act-starter.json`;
- `AGENTS.md` and `CLAUDE.md`;
- `.codex/skills/`;
- `.claude/skills/`, `.claude/hooks/aethyme-load-context.sh`, and
  `.claude/settings.local.json`.

The two JSON artifacts under `.aethyme/generated/` are portable canonical
inputs, not caches. They use repository-relative identity and allow another
clone to reproduce and verify the rendered agent guidance.

## Ignore runtime state

The managed `.gitignore` block excludes machine-local broker state:

- `.aethyme/broker.db*`;
- `.aethyme/logs/`, `.aethyme/reports/`, `.aethyme/run/`, and
  `.aethyme/worktrees/`;
- `.aethyme/broker-action-required.md`;
- `.aethyme/generated/experience-telemetry.jsonl`;
- `.aethyme/generated/experience-status.json` and
  `.aethyme/generated/experience-status.md`.

These files describe one checkout or active broker process. They are rebuilt
locally and must not travel between clones.

## Enforce deployment in CI

After committing the policy, make this a required check:

```bash
aethyme deploy verify --repo .
```

The command is read-only. It fails when the repository is configured for the
broker but its mandatory agent protocol is missing or invalid. Normal agent
work then begins with `aethyme broker status --json` followed by
`aethyme broker start --task "..."`, as required by the committed root policy.

Re-run `aethyme deploy --repo .` after changing gates, overrides, repository
structure, or the installed Aethyme version; review and commit the resulting
policy update.
