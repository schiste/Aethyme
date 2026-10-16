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
| `status` | Policy (and `ignored_settings`), config source, the required-capture fence, state root and its source, durability profile and receipt label, schema, compatibility floor and `needs_migration`, captures by state, captures needing attention, reserved bytes, unfinished reclamation, next actions. | Nothing. The fence comes from `broker status`'s own `collaboration_fence_state` on a read-only broker snapshot: `pending` when the committed config requires it, `active` once raised; an unreadable fence is `collaboration_fence_error`, never "absent". The store is read through a read-only snapshot: no pragmas, no migrations, no created files. A refusal (permissions, a newer floor, a locked or foreign database) goes under `state.refusal`. On close SQLite may remove an *empty* WAL and its shared-memory file; no stored byte changes. |
| `enroll [--write]` | Mints a project ID (`proj:<128-bit base32>`, directory key `proj-<base32>`, the #652 proposal) and prints the section enabling advisory capture. `--write` appends it to `.aethyme/config.toml` in the **current worktree** (`--repo` or the current directory) and prints that path. It is refused in the main checkout while broker sessions are live (`main_checkout_in_use`), when that file or the policy in force already mentions collaboration (`already_configured`), and when the file or `.aethyme/` is a symbolic link (`symlinked_config`). The write is atomic. The change takes effect once reviewed and committed. | Only with `--write`, and only that one file. |
| `capture recover` | Resolves crashed captures. Live ones are skipped; per-operation errors are listed and do not stop the rest. | Yes |
| `capture abort --operation <id>` | Aborts an unfinished capture. A committed one is refused (`already_committed`). | Yes |
| `capture receipt --operation <id>` | Shows a receipt and its standing: `retained_local`, `released` or `reclaimed`. | Creates nothing; an existing store is opened writable, which migrates an older one. |
| `gc plan` / `gc apply --confirm <digest>` / `gc resume` | Digest-confirmed reclamation, as the broker's `gc`. `plan` reports `recorded`: only a recorded plan can be applied. | `plan` records; `apply`/`resume` remove. |
| `context --path ...` | Bounded, explained context (cached, #730). Budget flags and `--source`, `--analysis` are passed through; out-of-range budgets are refused. Briefs print as untrusted data. | Index and cache rows. |
| `brief attach --contribution <id> <file>` | Validates a decision file with the #654 rules and attaches it. A file over 64 KiB is refused (`file_too_large`) before it is parsed; `context --analysis` files are capped at 1 MiB. | Yes |

**Only `broker submit` creates collaboration state.** Until the store exists, every
subcommand except `status` and `enroll` refuses with `not_initialized`. Once it exists,
they open it writable, which migrates an older store.

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

### Unknown settings: strict for required only (user decision)

§6.7 says "Unrecognized critical configuration is refused". Under `capture = "required"`
a key this binary does not implement is refused as `unknown_setting`: required gates
the submit, so a newer policy must not be half-applied. Under `advisory` the key is
ignored, so the submit proceeds and capture runs. The capture report (`ignored_settings`)
and `collab status` (`ignored_settings`, plus a warning line) both say so. A typo, or a
key from a newer binary, never stops an advisory submit. Without capture enabled the
key changes nothing. `submit` and `collab` share the reader, so they agree.

### Output an agent can act on

`--json` prints one object with a versioned `schema`, and a refusal prints
`aethyme.collab-error/experimental-v0` with `code` and `next_action`. Text output ends
with `next:` lines.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Done |
| 1 | Failed: I/O, database or Git, or an integrity failure (`corrupt_receipt`, `corrupt_object`, `missing_object`, `failed`) |
| 2 | Usage |
| 3 | Refused: do not retry unchanged |

The error object's `command` holds only the subcommand verbs, read before the first flag.
It never contains a path or an argument value. On Windows, where the broker is not yet
supported, `collab --json` prints `aethyme.collab-error/experimental-v0` with
`unsupported_platform` and exits 3.

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

## Tests

| Plan test | State |
|---|---|
| T33 (default disabled) | No policy, another table only, or `capture = "off"`: `status` says disabled. Every other subcommand exits 3 with `collaboration_disabled` and creates no state. |
| T34 (malformed policy) | An unreadable opt-in, a non-table `collaboration`, an unknown `capture` value and an unknown setting under `required` are each refused with their code; nothing is created. |
| Enrollment target | The main checkout is refused while sessions are live; a session worktree gets its own file and the main checkout is untouched; a section the policy in force already has, or a symlinked config, is refused. |
| Read-only status | After a capture, status changes no stored byte; insecure permissions are reported under `state.refusal`; an unreadable fence is `collaboration_fence_error` with no raise suggested; unit test: inspection reports `needs_migration`, refuses a newer floor and a foreign project, and writes nothing. |
| Forward compatibility | An advisory repository with a key from a newer binary submits, captures, and reports `ignored_settings` in submit and status; required refuses it. |
| State creation and arguments | No subcommand creates state (`not_initialized`); `command` never echoes a path; oversized brief and analysis files are refused. |
| T35 (explicit commands) | Over a real advisory capture made by `broker submit`: status, receipt, a missing receipt, aborting a committed capture, recover, a valid and an invalid brief, context (fresh, then cached, briefs untrusted, a budget out of range), an unrecorded plan refused, an orphan reclaimed by plan and apply, resume. |
| Fence (#660) | Under a committed required policy, status reports `pending` and the next action, does not raise it itself, and reports `active` after `broker status`; a working-copy-only required policy and advisory show none. |
| T57 (version and help) | Schema names asserted for every subcommand. `--help` on every subcommand exits 0 with no side effects; usage errors exit 2. |
