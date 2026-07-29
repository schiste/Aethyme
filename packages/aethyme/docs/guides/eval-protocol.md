# Eval Protocol (removed)

Last Updated: 2026-07-29

The evaluation harness (`src/eval/`, `evals/tools/*.toml`, the
`aethyme eval` / `aethyme methodology` CLI groups, and
`packages/aethyme-eval-ui/`) was removed on 2026-07-13 as part of the
local-first broker repositioning. The full protocol this document
described (5+1 condition matrix, playground setup, Chau7 execution,
scoring, methodology fingerprinting) is preserved in distilled form at
[`../architecture/eval-mining-notes.md`](../architecture/eval-mining-notes.md).

The Cardinal Rules in the repository root `CLAUDE.md` still apply to any
future evaluation work: evals run only against Playground repositories,
and nothing is ever tuned to improve an eval score. The audit trail of
rejected eval-tuning proposals remains at
[`../architecture/eval-tuning-rejected.md`](../architecture/eval-tuning-rejected.md).

## Live Playground A/B Contract

The remaining live eval surface is the playground A/B runner under
`scripts/eval/`. Any Control vs Aethyme comparison must hold this contract:

- The target repository is a Playground clone, never the Aethyme checkout.
- The Control arm has no `.aethyme/`, `.chau7/`, `.codex/`, `.claude/`,
  generated `AGENTS.md`, generated `CLAUDE.md`, graph store, skill, or
  Aethyme environment leakage.
- The Aethyme arm exposes only the intended enhancement surface: generated
  root guidance, the Aethyme skill/reference files, fragment graph, and local
  Redb graph store. Internal eval skills are forbidden.
- Both arms use the same Codex runner settings. The bundled runner uses
  `codex exec --ignore-user-config --json` by default so global MCP servers,
  hooks, and local user config do not become the measured variable.
- Runs preserve `events.jsonl`, `stderr.log`, `last-message.json`,
  `command.json`, and `contract.json`, and emit wall time, token usage,
  command-output chars, event-log chars, and stderr chars in the runner JSON.
- Generated artifact leakage in selected files, snippets, command output, or
  the final answer is a hard regression. The bundled runner checks `.aethyme/`,
  `.chau7/`, `.codex/`, `.claude/`, generated `AGENTS.md`, generated
  `CLAUDE.md`, and `graph_store.redb`; it writes `leakage.json` and exits
  non-zero when the leak gate trips.
- The regression gate compares stable budget and hygiene metrics, not selected
  file identity: token estimate delta, selected file count delta, snippet count
  delta, command-output char delta, generated-artifact leakage, Aethyme
  invocation, deterministic repeat output, Surface/Flow coverage reporting, and
  reviewer-rubric final answer quality.

### Required V2 Surface/Flow Fixtures

A V2 evaluation suite is not valid unless it covers all of these Playground
fixture families:

| Fixture id | Required behavior family |
|---|---|
| `django_backend_auth` | Django backend-only auth |
| `edge_proxy_backend_auth` | edge proxy + backend auth |
| `oidc_session_auth` | OIDC + session auth |
| `webhook_secret_auth` | webhook secret auth |
| `queue_job_behavior` | queue/job behavior |
| `config_owned_middleware_behavior` | config-owned middleware behavior |
| `frontend_backend_route_behavior` | frontend-to-backend route behavior |

Each suite row must compare Control and Aethyme runner JSON for the same
fixture id. The strict regression gate requires repeat result JSON for both
arms so it can compare deterministic output fingerprints. For fixtures with
known incomplete graph coverage, declare `expected_missing_coverage`; the gate
fails unless Aethyme observability reports those missing Surface/Flow families
instead of hiding them behind a generic freshness status.

Set `AETHYME_EVAL_ARM=control` or `AETHYME_EVAL_ARM=aethyme` explicitly when
using `scripts/eval/run_codex_eval.py`. For reproducible archives, set
`AETHYME_EVAL_ARTIFACT_DIR` to a run-specific directory outside the target
repository.

For multi-agent playground load, isolate every worker's mutable state. Do not
share a test database name across workers; suffix it with a worker/session id
or, inside broker gates, `$AETHYME_TEST_DB_SUFFIX`. If workers are expected to
touch the same files, use broker leases or separate worktrees before starting
the run so git indexes, test DBs, and generated artifacts do not become the
measured variable.

Sources are recoverable at git ref `16cfa5e`
(`packages/aethyme/src/eval/`, `packages/aethyme/evals/`,
`packages/aethyme-eval-ui/`). Frozen baseline reports remain at
`docs/reports/evals/`. The playground environment scripts
(`scripts/eval/`) remain live — they set up paired control /
tool-enhanced clones and are the natural testbed for future
multi-agent broker load scenarios.
