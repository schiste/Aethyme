# Per-worktree Git configuration: migration controls

Last Updated: 2026-10-25

For #906, defer a production broker migration. Sparse checkout is a concrete
consumer, but is not currently an enabled broker feature. There is no demonstrated
production setting that requires enabling the repository extension now. This
follow-up fills primary/linked migration controls; it does not qualify every
client or authorize changing an existing repository.

## Current placement dependencies

| Surface | Placement dependency |
|---|---|
| `git.rs::config_get` / `config_value` | Read effective configuration in the checkout, so per-worktree overrides would change returned values. |
| `hooks.rs` installation and hook diagnostics | Inspect effective `core.hooksPath`, its origin and relative checkout paths; migration cannot assume one shared hooks path. |
| `attribution.rs` | Uses effective `user.name` / `user.email`; moving identity keys would change commit attribution. |
| `git.rs::push_query_config` | Replays effective push configuration into a separate query repository but excludes `core.bare`, `core.worktree`, repository format and `extensions.*`. Replay is not a migration algorithm. |
| Git checkout identity | Explicit primary `core.worktree` and `core.bare` need primary-only handling; copying them to linked checkouts can redirect or invalidate their identity. |
| Sparse checkout | `core.sparseCheckout` must be scoped consistently with the actual index/pattern state. The controls below use explicit false, not an active sparse-index migration. |

This is a focused source inventory, not a claim that every external Git client or
third-party configuration include is covered. No production key is moved.

## Reproducible offline controls

Run `packages/aethyme-eval/benchmarks/performance/git_worktree_config.py` with an
existing `--fixture-parent`, a new `--output` directory and optionally
`--other-git` and `--legacy-git`. It creates a new synthetic Playground only. Commands have a
20-second deadline and a 750-command budget; complete receipts and exact harness
digest are retained. Failure preserves its explicitly recorded fixture path;
success removes only that fixture. No package installation or native build runs.

The matrix covers a normal primary, a primary with explicit `core.worktree`, and
a bare primary, each with two existing linked checkouts and a new linked checkout.
Before enabling the extension, an actual single-worktree `config --worktree`
write is checked in shared local configuration. After linked checkouts exist, the
installed client must refuse that spelling without changing shared config. Staging uses explicit
`config --file <git-dir>/config.worktree`, not `--worktree`.

The experimental transition prepares every per-worktree file before atomically
replacing the shared config with a staged version that enables the extension and
removes the migrated shared keys. The same staged file sets
`core.repositoryformatversion=1`: Git's mandatory refusal of an unknown
extension depends on format 1. The original format-0 experiment did not establish
this old-client safety fence. No production repository format is changed. After each prepared file, all original effective
settings, heads and bare identities must remain unchanged. After enable, checkout
identity and clean status must hold. A synthetic session-only setting must stay
out of siblings and newly created checkouts. The alternate client reads the same
configuration and refreshes linked indexes without changing status.

Each topology temporarily adds an intentionally unknown extension and requires
all three existing checkouts to refuse a valid HEAD lookup with the extension
in the diagnostic. A `finally` block restores exact enabled config bytes, then
heads, identities and effective settings must still agree. An optional actual
`--legacy-git` must first read the original layout, refuse `worktreeConfig`
after enable, and read the original layout again after rollback. A modern Git
passed as the legacy client must fail this control; changing its version text
is not qualification.

The additive report fields are `legacy_git` (the invoked client's version text,
or null when omitted), and per-case `format_one_refusal_fence` and
`unknown_extension_refusal_checked` booleans. `legacy_client` is `not_run` when
omitted, or `before_refuse_after_rollback_passed` after all three phases pass.
Version text is provenance, not proof that an executable is a genuine old client.

Rollback atomically restores the original shared bytes before removing staged
per-worktree files. This deliberately discards the synthetic session setting; it
is not a safe automatic rollback once real independent session settings exist.
The experiment assumes quiescence, does not fsync a transaction journal and does
not claim power-loss durability or safety with concurrent writers. A production
migration would need exclusion, expected-byte checks, recovery intent, config
permissions preservation and a defined policy for new session settings.

## Local observation

On macOS, the final synthetic matrix passed all three topologies and 450 Git
commands using Git 2.55.0 and Apple Git 2.54.0 as the alternate reader/writer.
All twelve staged/enable checkpoints preserved the asserted identities and
placement. Every successful owned Playground was removed. These are correctness
observations, not broker latency measurements or old-client qualification.
In-memory deliberate changes that put the session setting in shared config or
omit shared-config restoration both fail at their intended assertions.

## Qualification boundary

The local runs use the two installed recent Git clients only. Older-client
refusal, the declared minimum Git version, IDE/CI/GitHub workflows, real include
chains, active sparse checkout and live interrupted migration remain unqualified.
Do not infer compatibility from a version string or simulate an old-client pass.
The extension is a configuration control plane, with no performance gain claimed.
Keep #906 open until its remaining compatibility and adoption criteria are met.

## Refusal-fence follow-up

The repository-format requirement follows the [Git repository format contract](https://git-scm.com/docs/gitrepository-layout#_git_repository_format_versions)
and is explicit in [Git 2.19's format verifier](https://github.com/git/git/blob/v2.19.0/setup.c#L470-L489).
The new format-one and unknown-extension controls passed all three synthetic
Playground topologies with Git 2.55.0 and Apple Git 2.54.0: 513 commands and
successful fixture removal. Removing the format setter fails the explicit
format-one assertion; bypassing the refusal guard makes the modern-as-legacy
negative control fail as intended. Both positive and restored controls pass,
and every owned control Playground was removed. These are two in-memory
mutations, not native Aethyme test runs. Remote CI is pending; the 450-command
observation above belongs to the earlier experiment. An actual
unsupported client and the supported minimum are still unqualified unless their
separate run receipts are present. The production recommendation remains defer.
