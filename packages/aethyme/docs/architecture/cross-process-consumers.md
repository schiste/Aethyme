# Cross-process consumers of Aethyme entry points

Last Updated: 2026-05-09

When code outside the `packages/aethyme/` Python or Rust source tree
invokes an Aethyme command, it crosses a process boundary. Static
analysis (ruff, cargo check, type checkers) does not see those
invocations. They have to be audited by hand.

This file is the canonical inventory: every shell script, hook, skill
template, deployed wrapper, and CI step that calls an Aethyme entry
point. **Before deleting or renaming any CLI entry point — Python
`cli.py` command, Rust binary subcommand, or shell helper — grep this
file for callers and update each.**

The 2026-05-08 hard-delete of `python -m src.cli explore` broke the
deployed `aethyme-explore` wrapper because we hadn't tracked it as a
consumer. This file exists so that doesn't happen again.

## How to use this file

1. **Adding a new wrapper / hook / external invocation?** Add a row
   to the appropriate section below. Note the source path, the
   invoked Aethyme entry point, and what kind of failure surfaces if
   the entry point disappears.

2. **Removing or renaming an Aethyme entry point?**
   - Search this file for the entry point's name.
   - For each consumer listed: either update the consumer in the same
     commit, or add a backwards-compat shim, or accept the breakage
     with explicit reasoning.
   - If you find a consumer that's NOT listed here, add it before
     proceeding. The miss is itself the bug.

3. **Suspect something is stale?** Run
   `scripts/eval/verify-playground.sh` against any deployed
   playground; it asserts a subset of the contracts below.

## Inventory

### Skill template files (canonical sources, deployed by `enhance.py` / `indexing/skills.py`)

| Source | Deployed to | Invokes | Failure mode if entry point removed |
|---|---|---|---|
| `skills/aethyme/SKILL.md` | `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md` | Documents `aethyme explore`, `aethyme query symbol`, `aethyme graph callers/callees`, `analyze dead-code`, `facts function-usage`, `task scope/anchors`, `intents`, `task context/pack` by name | Agent reads stale guidance, runs commands that no longer exist; user sees `Error: No such command 'X'`. Caught by `verify-playground.sh` greps. |
| `skills/aethyme/AGENTS.md` | `AGENTS.md` (deployed at repo root by `enhance.py`) | Cross-product convention file — no exec |
| `skills/aethyme/aethyme-explore` | `.codex/skills/aethyme/aethyme-explore` (executable) | `exec "{{AETHYME_ROOT}}/rust/target/release/aethyme" explore "$@"` | Wrapper produces `Error: No such command 'explore'`. Class-3 failure (silent until invoked). Rebuilt 2026-05-08 to point at native; previously called `python -m src.cli explore` (deleted). |
| `skills/aethyme/aethyme-load-context.sh` | `.claude/hooks/aethyme-load-context.sh` (executable, wired via `.claude/settings.local.json`) | Reads `AGENTS.md` + `CLAUDE.md` from `$CLAUDE_PROJECT_DIR`; emits SessionStart hook JSON. **Does NOT invoke any Aethyme entry point.** | Hook fails to inject context; agent loses the in-repo discoverability surface. |

### Deployment plumbing

| Source | Deploys what | Notes |
|---|---|---|
| `src/enhance.py:TARGETS` | `AGENTS.md`, `CLAUDE.md`, `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md`, `.claude/hooks/aethyme-load-context.sh`, `.claude/settings.local.json` (merge-aware) | Single canonical deploy pipeline for in-repo Aethyme discoverability. Substitutes `{{AETHYME_ROOT}}`. |
| `src/indexing/skills.py:deploy_skills` | `.codex/skills/<name>/*` for each runtime skill in `skills/` | Different from `enhance.py` — used by `eval/repos.py` during eval prep. As of 2026-05-08 substitutes `{{AETHYME_ROOT}}` in `.md`, `.sh`, AND the `aethyme-explore` wrapper (no extension). |

### Shell scripts in `packages/aethyme/scripts/`

| Script | Invokes | Failure mode |
|---|---|---|
| `scripts/eval/setup-playground.sh:151,172` | `python -m src.cli enhance deploy/verify` | Setup fails if `enhance` Click group is renamed/removed. |
| `scripts/eval/setup-playground.sh:58` | `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli` | Setup fails if Rust binary path changes (e.g. workspace restructure). |
| `scripts/eval/verify-playground.sh:45` | `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli` | Same as above. |
| `scripts/eval/verify-playground.sh:131-138` | Greps deployed SKILL.md for command names (`src.cli intents`, `aethyme explore`, `analyze dead-code`, `facts function-usage`) | False positive/negative on health check if SKILL.md template changes wording but verify-playground doesn't update its grep patterns. **Updated 2026-05-08 after the explore hard-delete to expect native `aethyme explore` not `src.cli explore`.** |
| `scripts/docs/generate-docs.sh:38` | Reads `src/cli.py` to extract command help | Doc generation fails if `cli.py` moved/renamed. |
| `scripts/migrate.sh` | psql / `alembic upgrade head` (no Aethyme entry point) | DB migration; not a cross-process Aethyme consumer. |
| `scripts/start-api.sh` | `uvicorn src.api.main:app` | API server start; depends on `src.api.main` existing. |

### CI / GitHub Actions

| Workflow | Calls | Failure mode |
|---|---|---|
| `.github/workflows/aethyme-local-tests.yml` | `pytest`, `cargo test`, `ruff`, etc. | Standard. Doesn't directly invoke Aethyme CLI. |
| `packages/aethyme/.github/workflows/evals.yml` | Eval harness commands; should be checked when eval-type defaults change | (Audit pending — flag if you find specific entry-point references during a future migration.) |
| `packages/aethyme/.github/workflows/performance.yml` | Performance benchmarks | Same. |
| `packages/aethyme/.github/workflows/cd.yml` | Release/deploy steps | Same. |
| `packages/aethyme/.github/workflows/aethyme-example.yml` | Example invocations | Same. |

### Externally deployed runtime files (under `~/Downloads/Repositories/Playground/`)

These are files in *separate repos* (the playground clones), put there
by `deploy_skills` / `enhance.deploy`. For benchmark clones, static skill
deployment remains the compatibility path; for normal repositories,
`enhance.deploy` is the primary path because it also writes generated
onboarding artifacts.

| Path (under each `<Playground>/<repo> - Aethyme/`) | Source template | Last verified |
|---|---|---|
| `.codex/skills/aethyme/SKILL.md` | `skills/aethyme/SKILL.md` | 2026-05-08 (post-redeploy) |
| `.codex/skills/aethyme/aethyme-explore` | `skills/aethyme/aethyme-explore` | 2026-05-08 (post-redeploy) |
| `AGENTS.md`, `CLAUDE.md` | `skills/aethyme/AGENTS.md` | 2026-05-08 |

## Migration checklist

When deleting or renaming a CLI entry point:

- [ ] Grep this file for the entry-point name; list affected consumers.
- [ ] For each Skill template (`SKILL.md`, etc.): update the wording.
- [ ] For each shell wrapper (`aethyme-explore`, etc.): update the
      `exec` line.
- [ ] For `verify-playground.sh`: update the grep patterns.
- [ ] For `setup-playground.sh`: update if it calls the entry point.
- [ ] Run `aethyme repo deploy-skills --force` against any active
      benchmark playground clone to flush the stale static copy.
- [ ] Run `aethyme enhance deploy --repo <path>` against any normal
      repository enhancement target to refresh generated onboarding.
- [ ] Add a row to the relevant section above documenting the change.
- [ ] If a consumer was missed (i.e. you discovered it during a
      validation eval rather than this audit): **add it to this file
      in the same commit as the fix.** That's how the registry stays
      complete.

## Failure modes (taxonomy)

When a cross-process consumer references a deleted entry point:

- **Class 1: load-time failure** — script syntax-checks fail or imports
  fail. Loud. Caught immediately.
- **Class 2: invocation failure** — script runs but `aethyme <cmd>`
  errors with `No such command`. Loud once invoked. May go undetected
  if the script's invocation path is rare.
- **Class 3: silent stale guidance** — script reads file content and
  matches/passes/fails based on string presence; updated entry point
  passes the wrong checks. The 2026-05-08 wrapper bug AND the
  `verify-playground.sh:135` grep flip-bug were both Class 3. **These
  are the dangerous ones**: nothing visibly fails, but the system's
  health checks are now lying.

Class-3 failures are why this registry exists. Static analysis can't
help; only manual audit of every consumer can.
