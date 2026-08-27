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

Claude-specific skills and `.claude/hooks/aethyme-load-context.sh` are optional
local integrations. A team may track them when it wants portable Claude
support, but canonical verification does not require them in every clone.
`.claude/settings.local.json` is always machine-local: it can contain private
permission history and absolute paths, must not be committed, and is installed
best-effort by `aethyme deploy` in each checkout.

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
Missing optional `.claude/` integrations are warnings rather than failures, so
a clean clone can verify portable canonical policy before local activation.

Re-run `aethyme deploy --repo .` after changing gates, overrides, repository
structure, or the installed Aethyme version; review and commit the resulting
policy update.

## Private adoption with an inert bridge

When a team is not ready for mandatory repository-wide deployment, commit only
the activation bridge:

```bash
aethyme deploy bridge --repo .
git add AGENTS.md CLAUDE.md
git commit -m "docs: add optional local Aethyme bridge"
```

The managed bridge gives Codex, Claude, and other root-policy readers one
conditional instruction: if `.aethyme/local/enabled` exists, load
`.aethyme/local/AGENTS.md` as mandatory policy. If the marker is absent, agents
continue normally without running `command -v`, probing PATH, installing
Aethyme, emitting a warning, or mentioning the inactive capability.

An individual developer activates the full workflow with:

```bash
aethyme deploy --local-only --repo .
aethyme deploy verify --local-only --repo .
```

Local deployment writes broker configuration, drafted gates, generated
onboarding, product-specific skills, the complete agent policy, and runtime
state under their ordinary repository-relative paths. A managed block in the
clone's `.git/info/exclude` hides exactly those activation paths; tracked
`.gitignore` is not modified. The command refuses to overwrite a tracked agent
policy, preventing local mode from masking canonical team configuration.

Re-running local deployment refreshes Aethyme-owned ignored files. Removing
`.aethyme/local/enabled` immediately makes the committed bridge inert; the
remaining ignored files perform no work and may be removed later. A fresh
clone receives the bridge but none of the activation artifacts.
