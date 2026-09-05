# Repository deployment contract

Last Updated: 2026-09-01

Aethyme has two distinct installation scopes:

- install `aethyme` and `aethyme-engine-cli` once for the user;
- deploy and commit Aethyme policy separately in every repository where agents
  must use the broker.

For the first shared enrollment, review the exact remote-based plan and then
authorize that plan by digest:

```bash
aethyme deploy plan --repo . --diff
aethyme deploy execute --repo . --confirm <plan-sha256>
```

`plan` is read-only. It fetches the current remote default branch into a
disposable checkout and proposes the complete tracked policy tree from that
exact commit, without reading ignored or untracked content from the active
checkout. Its digest binds the remote and local refs, generated file hashes and
modes, dirty overlap, live broker state, shared hook state, local activation,
and exact preservation refs. A foreign `core.hooksPath`, overlapping dirty
policy, active session, nonterminal queue entry, ambiguous integration history,
or remote movement blocks execution instead of being overwritten.

`execute` first creates the reviewed preservation refs. It then prepares the
ignored runtime boundary, activates shared hooks, creates an isolated broker
session with exact path leases, applies and commits only the reviewed outputs,
submits them through the broker, publishes the exact promoted SHA, and verifies
the remote. The primary default branch is synchronized only when it is clean,
unchanged since planning, and fast-forwardable; otherwise publication remains
successful and the report gives the unsynchronized state. It does not install
another copy of the executable inside the repository.

Execution is phase-journaled under the Git common directory. Repeating the same
confirmed command resumes after interruption and does not duplicate an
already-verified publication. A changed plan digest or remote default SHA is a
hard refusal: re-plan and review the new state. Preservation refs and the
completed journal remain as local evidence.

For an intentionally offline/manual enrollment, `aethyme deploy --repo .`
continues to scaffold, draft, deploy, verify, and certify the working tree
without publishing it. The operator is then responsible for reviewing,
committing, and publishing that tree.

## Review the policy

The plan and its local diff cover the files that execution will commit:

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

Confirmed execution commits exactly this reviewed write set. If maintainers
need to customize generated policy, stop before execution, add the supported
override, and create a new plan rather than editing the proposed output.

The two JSON artifacts under `.aethyme/generated/` are portable canonical
inputs, not caches. They use repository-relative identity and allow another
clone to reproduce and verify the rendered agent guidance.

The onboarding artifact is generated from tracked repository inputs only.
Nested workspace manifests, members, lockfiles, source files, CI references,
and executable entrypoints provide auditable evidence for the selected primary
workspace. Commands are scoped so they run from the repository root; a nested
Cargo product therefore uses `cargo ... --manifest-path <path> --workspace`
instead of relying on an implicit working directory. Supporting tools and
evaluation packages remain visible but do not replace missing commands in the
primary product contract.

Ignored dependencies, virtual environments, build output, and untracked local
manifests never influence canonical onboarding. If maintainers intentionally
need a local/generated surface or want to replace an inferred workspace,
command, entrypoint, or path classification, record that decision in
`.aethyme/overrides/onboarding.json` and regenerate. Overrides win over
inference and are validated with a closed schema; generated files should not be
edited directly.

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

`.aethyme/worktrees/` remains ignored for backward-compatible cleanup and the
reported constrained fallback. New broker-managed sessions normally live in a
private, clone-keyed per-user host-state directory outside the checkout. Use
`aethyme broker worktree-root --json` to inspect that decision without creating
the directory. This placement keeps broad repository scanners from traversing
another complete checkout.

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
