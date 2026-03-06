# Aethyme Rust Engine

Last Updated: 2026-03-06

This workspace is the start of the Rust engine boundary for Aethyme Core.

## Why It Exists

Aethyme's long-term direction is to make AI agent navigation and code changes as
 deterministic and efficient as possible.

The engine layers that benefit most from Rust are:

- fallback indexing and symbol extraction
- graph expansion and traversal
- deterministic context-pack assembly
- scope and risk classification
- later, advisory and enforceable policy evaluation

## What Stays In Python

For now, Python remains responsible for:

- API delivery
- CLI delivery
- auth and tenant enforcement
- scorecard orchestration
- SDKs
- product experimentation around workflows

## What Moves To Rust First

Phase 1 Rust targets:

1. context-pack data model
2. deterministic pack-building pipeline
3. risk and scope types
4. repository mapping
5. discoverability kernels

## Current Local-First Surface

The Rust workspace now supports the first local-repo proof path behind the Python CLI:

- repository inspection
- symbol lookup
- dependency frontier lookup
- impact frontier lookup
- deterministic task-context pack assembly
- deterministic explain-repo output

The current entrypoint is the local engine binary invoked through Python adapters:

- `cargo run --manifest-path rust/Cargo.toml --bin aethyme-engine-cli -- inspect --repo /path/to/repo`
- `cargo run --manifest-path rust/Cargo.toml --bin aethyme-engine-cli -- pack --repo /path/to/repo --task "Explain this repo"`

At runtime, the Python layer now builds the binary once and executes the binary directly rather than shelling through `cargo run` on every request.

## Rule

Do not move customer-facing orchestration into Rust until the product semantics
are stable.

Move engine components first.
