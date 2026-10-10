# Local v3 — L9 slice 1: opt-in collaboration commands (#680)

Last Updated: 2026-10-10

This record covers the first command surface for Local collaboration (plan §6.7–6.8;
D18, D32; T33–T35, T57). It exposes only what the qualified L2/L3 packages provide:
state root (#714), archive (#715), capture (#716), reclamation (#717), context (#718,
#730) and the submit opt-in (#719). Target run, sync, handoff, stop and export come
with L5–L8 and are not listed.

Code: `aethyme-broker::collaboration_cli`, routed by the `aethyme` router. Tests:
`aethyme-cli/tests/collab_cli.rs` through the built binary, plus unit tests.

## Decided here

### Namespace: `aethyme collab`

A top-level group, as plan §6.7 proposes, rather than `aethyme broker collab`:

- The broker verbs manage sessions, queues and publication for this checkout.
  Collaboration state belongs to a project and outlives any session (#656), so it is not
  a broker verb.
- `aethyme broker --help` lists eight public verbs, and keeping that list stable is a
  standing goal (`cli/surface.rs`).
- The router's help guard passes `collab --help` to the command, which answers it before
  doing anything (`help_everywhere` checks every subcommand for side effects).

Windows reports the broker as unavailable for `collab`, as it does for every broker
command.

### Subcommands

| Command | What it does | Writes |
|---|---|---|
| `status` | Policy, config source, state root and its source, durability profile and receipt label, schema and compatibility floor, captures by state, captures needing attention, reserved bytes, unfinished reclamation, next actions. | Never creates state. It opens an existing store, which may migrate it. |
| `enroll [--write]` | Mints a project ID (`proj:<128-bit base32>`, directory key `proj-<base32>`, the #652 proposal) and prints the section enabling advisory capture. `--write` appends it only to a config that does not mention collaboration. | Only with `--write`. |
| `capture recover` | Resolves crashed captures. Live ones are skipped; per-operation errors are listed and do not stop the rest. | Yes |
| `capture abort --operation <id>` | Aborts an unfinished capture. A committed one is refused (`already_committed`). | Yes |
| `capture receipt --operation <id>` | Shows a receipt and its standing: `retained_local`, `released` or `reclaimed`. | No |
| `gc plan` / `gc apply --confirm <digest>` / `gc resume` | Digest-confirmed reclamation, as the broker's `gc`. `plan` reports `recorded`: only a recorded plan can be applied. | `plan` records; `apply`/`resume` remove. |
| `context --path ...` | Bounded, explained context (cached, #730). Budget flags and `--source`, `--analysis` are passed through; out-of-range budgets are refused. Briefs print as untrusted data. | Index and cache rows. |
| `brief attach --contribution <id> <file>` | Validates a decision file with the #654 rules and attaches it. | Yes |

An explicit `collab capture` is not offered yet. Captures come from `broker submit` under
the opt-in (#719), which knows the exact base and result. A standalone capture command
needs a reviewed way to name both, and waits for a consumer.

### Disabled unless policy enables it

Collaboration is enabled exactly when `[collaboration] capture` is `advisory` or
`required` and `project` is a valid key. The config is read by #719's reader, not a
second parser, so `submit` and `collab` cannot disagree. While disabled, every
subcommand except `status` and `enroll` refuses with `collaboration_disabled` and the
next action. The policy is checked before any argument file is read, and before any
state is touched.

### Critical settings fail closed

Every `[collaboration]` setting is critical (§6.7: "Unrecognized critical configuration
is refused"). When capture is enabled, a key this binary does not implement is refused
as `unknown_setting`. This applies to `submit` too, because the reader is shared.
Ignoring the key could half-apply a newer policy. A repository that has not enabled
capture is unaffected, so the legacy submit never changes because of a key it does not
use.

### Output an agent can act on

`--json` prints one object with a versioned `schema`, and a refusal prints
`aethyme.collab-error/experimental-v0` with `code` and `next_action`. Text output ends
with `next:` lines.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Done |
| 1 | I/O, database or Git failure |
| 2 | Usage |
| 3 | Refused |

`status` prints the local state root path. That is the point of the command, and the
output stays on the host. Submit's report, which may be pasted into pull requests,
remains path-free (#719).

### Nothing in the background

Every subcommand runs in the foreground and exits. Nothing starts a daemon, a loop or a
network call (§6.7: "An upgrade must not silently install an always-on daemon").

## Not decided here

- **D18** needs the remaining L5–L8 commands and the T33 comparison of legacy commands
  with collaboration on and off. This slice adds no change to legacy command output.
- **D32:** every schema here is `experimental-v0`. Freezing any of them waits for two
  real consumers.
- **Enrollment and ProjectId (D10):** `enroll` mints the proposed form. It does not yet
  write an enrollment descriptor record or bind two clones (T03).
- **`--write` edits only a config that does not mention collaboration.** Editing an
  existing section safely needs a format-preserving editor.
- **The status of the required-capture fence** (#719's broker.db floor) is shown here
  once that lands.

## Tests

| Plan test | State |
|---|---|
| T33 (default disabled) | No policy, another table only, or `capture = "off"`: `status` says disabled. Every other subcommand exits 3 with `collaboration_disabled` and creates no state. |
| T34 (malformed policy) | An unreadable opt-in, a non-table `collaboration`, an unknown `capture` value and an unknown setting are each refused with their code; nothing is created. |
| T35 (explicit commands) | Over a real advisory capture made by `broker submit`: status, receipt, a missing receipt, aborting a committed capture, recover, a valid and an invalid brief, context (fresh, then cached, briefs untrusted, a budget out of range), an unrecorded plan refused, an orphan reclaimed by plan and apply, resume. |
| T57 (version and help) | Schema names asserted for every subcommand. `--help` on every subcommand exits 0 with no side effects; usage errors exit 2. |
