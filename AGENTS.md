# Aethyme Development Guide

## Repository Structure

Monorepo with primary package at `packages/aethyme/`.

- **Python** (`src/`): CLI, API, auth, graph store, indexing, scorecard, eval harnesses
- **Rust** (`rust/crates/aethyme-engine/`): deterministic repo mapping, graph navigation, context-pack assembly
- Python shells out to the Rust engine binary via `src/indexing/engine.py`

## Cardinal Rules

1. **All evaluations run against Playground repositories, never against Aethyme itself.**
2. **Never modify tools, engine, pipeline, or skills to improve eval scores.** Evals are diagnostics, not targets. If an eval reveals a weakness, fix the generic system — never add task-type-specific accommodations, special-case heuristics targeting known eval scenarios, or output formatting that matches scorer expectations. Ask: "Would I make this change if the eval didn't exist?" If no, do not make it.

See [`packages/aethyme/docs/guides/eval-protocol.md`](packages/aethyme/docs/guides/eval-protocol.md) for the full eval protocol including detailed examples of forbidden vs allowed changes.

## Broker Coordination (multi-agent sessions on this repo)

This repository dogfoods the Aethyme broker. If you are one of several
agent sessions working here concurrently:

1. Broker entry point, before editing: check activity, create an isolated
   broker worktree, and work from that checkout:
   `aethyme broker status --json`, then `aethyme broker start --task "<task>"`
   and `cd` into the reported worktree. If you are already in a dedicated
   worktree, use `aethyme broker adopt --task "<task>"` instead.
   (install once: `cargo install --path packages/aethyme/rust/crates/aethyme-cli` and `cargo install --path packages/aethyme/rust/crates/aethyme-engine` — the router plus its engine-daemon sibling binary; check with `aethyme --version`).
2. If you know you will touch a shared file before it appears in your diff,
   claim it explicitly: `aethyme broker leases claim <path> --session <id>`.
   Use a trailing `/` for directory leases, and release with
   `aethyme broker leases release <path> --session <id>` when done.
3. Run broad rewrite tools through the guard:
   `aethyme broker exec --session <id> -- <command>`. The guard fails if
   the command leaves dirty paths outside your explicit leases or in files
   that were untracked before your session began.
4. Commit early and small; only committed work integrates.
5. Gates run with path-scoped owner locks. Gate commands that need external
   state must use per-worker names, e.g. suffix test databases with
   `$AETHYME_TEST_DB_SUFFIX` instead of sharing one fixed DB.
6. When done, `aethyme broker submit --session <id>` — never merge or push
   yourself, and never touch the `aethyme/integration` branch directly.
   Then `aethyme broker close --session <id>` to finish, or
   `aethyme broker adopt --reuse --task "..."` for a follow-up task.
7. If `.aethyme/broker-action-required.md` appears in your worktree, your
   submission conflicted: it contains the files, the blocking session, and
   the exact rebase steps. Resolve and resubmit.
