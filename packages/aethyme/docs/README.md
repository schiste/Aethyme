# Aethyme Core Docs

Last Updated: 2026-08-23

This directory documents the code that is active in `packages/aethyme`.
The public product surface is broker-first and is documented canonically in
[`../../../docs/product-surface.md`](../../../docs/product-surface.md). Use that
page before reaching for lower-level graph, eval, or architecture references.

## Canonical Docs

- [`../../../docs/product-surface.md`](../../../docs/product-surface.md)
- [`../../../docs/project-plan.md`](../../../docs/project-plan.md)
- [`../README.md`](../README.md)
- [`vision.md`](vision.md)
- [`agent-navigation-spec.md`](agent-navigation-spec.md)
- [`architecture/research-informed-architecture-memo.md`](architecture/research-informed-architecture-memo.md)
- [`architecture/research-lessons-revised-after-implementation.md`](architecture/research-lessons-revised-after-implementation.md)
- [`architecture/graphability-and-navigability-signals.md`](architecture/graphability-and-navigability-signals.md)
- [`architecture/core-architecture.md`](architecture/core-architecture.md)
- [`architecture/rust-transition.md`](architecture/rust-transition.md)
- [`architecture/distribution-tool-spike-2026-08-23.md`](architecture/distribution-tool-spike-2026-08-23.md)
- [`getting-started/quickstart.md`](getting-started/quickstart.md)
- [`guides/broker-workflows.md`](guides/broker-workflows.md)
- [`guides/report-capture.md`](guides/report-capture.md)
- [`reference/cli.md`](reference/cli.md)
- [`architecture/eval-mining-notes.md`](architecture/eval-mining-notes.md)
- [`guides/testing.md`](guides/testing.md)
- [`guides/troubleshooting.md`](guides/troubleshooting.md)

## First Local Proof

For the current local-first product proof path, start with:

1. [`../../../docs/product-surface.md`](../../../docs/product-surface.md)
2. [`guides/broker-workflows.md`](guides/broker-workflows.md) for safe session
   reuse, gate cache, lease planning, and durable handoffs
3. [`guides/report-capture.md`](guides/report-capture.md) for offline,
   allowlist-only report capture and reviewed GitHub filing
4. [`reference/cli.md`](reference/cli.md)
5. [`getting-started/quickstart.md`](getting-started/quickstart.md) for the
   graph-engine path only

## Scope Rule

If a feature is not active in the current core loop or explicit engine plan, it should not have a long-form guide here.
