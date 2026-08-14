# Aethyme Development Guide

## Repository Structure

Monorepo with primary package at `packages/aethyme/`.

- **Rust** (`rust/crates/`): the entire product — engine (repo mapping,
  graph navigation, context packs), router, broker, enhance, quality
  (scorecard + autofix). `cargo install` is the whole install story.
- **No Python.** `src/` was deleted on 2026-08-01 (python-retirement
  Phase 6). **`python -m src.cli` is a HARD BREAK with no shim** — it now
  fails with `No module named src`. Every command is native; run
  `aethyme --help`. If you have old notes or muscle memory pointing at
  `python -m src.cli <anything>`, the replacement is `aethyme <same
  thing>` with the same flags and output.
- **`packages/aethyme` is 100% Rust** since 2026-08-06 (Phase 7): the dev
  pytest harness and `pyproject.toml` are deleted too. `cargo test
  --workspace` is the whole test story. `packages/aethyme-eval`
  stays Python by design — an arm's-length acceptance check should not
  share the measured system's toolchain.

## Cardinal Rules

1. **All evaluations run against Playground repositories, never against Aethyme itself.**
2. **Never modify tools, engine, pipeline, or skills to improve eval scores.** Evals are diagnostics, not targets. If an eval reveals a weakness, fix the generic system — never add task-type-specific accommodations, special-case heuristics targeting known eval scenarios, or output formatting that matches scorer expectations. Ask: "Would I make this change if the eval didn't exist?" If no, do not make it.
3. **Audit cross-process consumers before any CLI rename or delete.** Static analysis does not see shell wrappers, deployed skill scripts, hooks, or CI invocations. Before deleting/renaming any Rust binary subcommand or shell helper, grep [`packages/aethyme/docs/architecture/cross-process-consumers.md`](packages/aethyme/docs/architecture/cross-process-consumers.md) for callers and update each in the same commit (or accept the breakage with explicit reasoning). If you discover an unlisted consumer mid-migration, add it to the registry — that's how it stays complete.

See [`packages/aethyme/docs/guides/eval-protocol.md`](packages/aethyme/docs/guides/eval-protocol.md) for the full eval protocol including detailed examples of forbidden vs allowed changes.

## Broker Coordination (multi-agent sessions on this repo)

This repository dogfoods the Aethyme broker. If you are one of several
agent sessions working here concurrently:

1. Before editing, check activity and register your worktree:
   `aethyme broker status --json`, then `aethyme broker adopt --task "<task>"`
   (install once: `cargo install --path packages/aethyme/rust/crates/aethyme-cli` and `cargo install --path packages/aethyme/rust/crates/aethyme-engine` — the router plus its engine-daemon sibling binary; check with `aethyme --version`).
2. Commit early and small; only committed work integrates.
3. When done, use `aethyme broker submit --session <id>` as the default path
   for verified concurrent integration. Submission promotes only to the local
   `aethyme/integration` branch; it does not publish remote branches, PRs, or
   tags. Then `aethyme broker close --session <id>` to finish, or
   `aethyme broker adopt --reuse --task "..."` for a follow-up task.
4. If `.aethyme/broker-action-required.md` appears in your worktree, your
   submission conflicted: it contains the files, the blocking session, and
   the exact rebase steps. Resolve and resubmit.
5. Git capabilities are not restricted by the broker. When the user's request
   or a documented repository workflow authorizes the resulting state change,
   agents may perform any required Git operation, including clone, fetch,
   pull, switch, branch, add, commit, stash, merge, cherry-pick, rebase,
   revert, reset, tag, push (including force-push when explicitly authorized),
   and exact-ref deletion. Operations that require coordination must go
   through the broker. Use a dedicated broker workflow when one exists;
   otherwise run the authorized Git operation through
   `aethyme broker exec --session <id> -- git <operation> ...`. Direct Git is
   limited to read-only inspection and operations confined to the isolated
   session worktree and session branch that cannot affect other sessions. Any
   command that can move shared refs, the default branch,
   `aethyme/integration`, remote-tracking refs, tags, or remote state must not
   run outside the broker.
   Destructive or remote operations still require their normal authorization
   and exact targets; edit or submit authority alone does not imply publication
   authority. An explicitly authorized operator or release workflow may merge,
   tag, push, force-push, or delete refs through the broker; authorization does
   not permit bypassing coordination.
