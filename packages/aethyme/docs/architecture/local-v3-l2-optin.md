# Local v3 — L2 opt-in capture beside legacy submit (#660)

Last Updated: 2026-10-09

This record covers how contribution capture (#658) reaches users: explicitly,
beside the existing `broker submit`, without changing what submit does for anyone
who has not asked for it (plan §6.4, §6.7–6.8). It is input to **D18** (what stays
compatible when optional capture fails or old binaries run) and **D32** (which
records and ports are stable).

Code: `aethyme-broker::collaboration_submit`, one call site in `cli/submit.rs`, and
the promotion gate in `Broker::promote`.
Tests: the module's unit tests and `aethyme-broker/tests/collaboration_submit_cli.rs`.

## Decided here

### Configuration

```toml
[collaboration]
capture = "advisory"   # "off" (default), "advisory" or "required"
project = "proj-7k2m"  # the project's collaboration directory key
```

It is read like `[promote]`. When the repository has a fetched default branch
(`origin/HEAD` or the tracked upstream) and that commit holds
`.aethyme/config.toml`, the committed copy decides, and an uncommitted edit in one
checkout cannot change it. Otherwise (no remote, nothing fetched, or the file is
not committed) the main checkout's working copy decides. Every report names which
one applied: `config_source` is `committed` or `working_copy`.

A malformed file fails closed only when it visibly opts in. If the text does not
parse but has a `[collaboration...]` header or a `collaboration` key, the policy
is `config_unreadable`. If `collaboration` parses as something other than a table,
it is `unsupported_policy`. Both are treated as a required capture that cannot be
satisfied. A malformed file that never mentions collaboration keeps the legacy
behaviour, which is what every other setting does today.

`project` is a configured key until ProjectId enrollment exists (#652's
proposal). Enrollment will replace it; until then it must be the same in every
clone that should share a collaboration store, and must never be derived from a
path.

### What each setting does

| Setting | Before the legacy submit | After it | Exit code and verdict |
|---|---|---|---|
| Off: no file, no section, no `capture`, or `"off"` | nothing | nothing | legacy |
| `"advisory"` | nothing | after the verdict is printed: capture, one report line or JSON field | legacy, always |
| `"required"` | capture; if not acknowledged, submit is refused | the report is shown; promotion is gated (below) | 3 on refusal, otherwise legacy |
| Any other value, or an unreadable opt-in | refused (`unsupported_policy`, `config_unreadable`) | promotion refused | 3 |

- **Off** runs none of this code past reading the config. No collaboration state
  is created, nothing is fetched, and JSON and text output are the legacy ones.
  Tests compare the legacy key order against the advisory output with its one
  added key.
- **Advisory** runs after the legacy verdict exists. In text mode the verdict is
  printed first, and the capture only adds a line after it; in JSON mode the
  report is the final field. Nothing it does can change the verdict or the exit
  code:
  - it never returns an error, so no `?` can propagate it;
  - it catches panics and reports them as `failed` (`panicked`);
  - it never waits for another capture's lock: a held lock is reported as
    `in_progress`;
  - opening the state database waits at most SQLite's 5 s busy timeout.

  Every failure is a report with a `code`, a path-free `detail` and a
  `next_action`: missing project, refused state root (`overlaps_cleanup_root`,
  `ephemeral_repository`), incomplete source, refused source, local failure. A
  rejected or conflicted submit is still captured: capture keeps the contribution
  whatever the legacy validation decided (§6.4).
- **Required** captures first. Until the capture is acknowledged there is no queue
  entry and no gate run. Promotion is gated separately, in the broker (next
  section). Verification is not gated. Pushing a branch and opening a pull request
  are separate commands and are not gated.
- **Unknown values fail closed.** A newer policy (`"required-v2"`) or a typo is
  treated as a required capture that cannot be satisfied. A binary must never
  treat a policy it does not implement as met. This is the forward half of "older
  clients cannot acknowledge a policy they do not implement".

### Which promotions required capture gates

Every promotion in the broker goes through `Broker::promote(entry)`. That
function refuses, with `CaptureRequiredForPromotion` (exit 3, next action:
resubmit), an entry whose head has no acknowledged capture recorded with policy
`required` in this project's store. It also refuses when the store cannot be
opened or the policy is unsupported or unreadable. With capture off or advisory
the check reads the config and returns: those paths are unchanged.

The paths this covers:

- automatic promotion after a successful submit: the CLI, and the library's
  `Broker::submit`, `submit_with_intent` and `submit_with_policy`. A library
  submit that bypassed the capture verifies, then refuses to promote, and the
  entry stays `verified`;
- `aethyme broker promote --entry N`;
- `reverify_and_promote`, when the base moved after verification;
- the queue drain inside `promote`, which re-simulates other entries after a
  promotion. Its errors are already swallowed, so an uncaptured entry stays
  unpromoted rather than failing the promotion that ran;
- `aethyme-cli`'s repository enrollment, which submits and promotes through the
  same functions.

Not gated: verification without promotion (including every `verify-only`
repository, which never promotes), and publication by pushing a branch or opening
a pull request. In a `verify-only` repository, required capture is therefore
enforced only by the CLI submit path.

### Inputs and idempotency

The captured base is the merge base of the submitted head and the commit the
submit verifies against. Required capture refreshes integration and reads the
submission base exactly as submit does (the fetched default branch under
`verify-only`, otherwise integration) before capturing. Advisory capture uses the
outcome's `verified_against.commit` (else `entry.base_commit`). A resubmit after
integration has been refreshed onto upstream is therefore the same operation.
Both commits are full object ids, so a branch that moves afterwards changes
nothing (T07).

The operation ID is `submit:` plus 40 hex digits of a SHA-256 over the session,
base, result and policy. A resubmit of the same commits against the same
verification base (after a crash, a lost response, a rejected gate, or under
`verify-only` while upstream stays put) is the same operation and answers with
the same receipt (#658). New commits, a new verification base or a changed policy
give a new operation. Retention is `until_released` until #659 defines classes.

### The receipt names the submitted commit

A capture and a submit that read the session head separately could disagree. A
commit, amend or reset in between would submit code the receipt does not cover,
and a required policy would be "satisfied" for a commit that was never captured.
They are therefore bound to one commit:

- **Required:** the captured head is passed to `Broker::submit_expecting_head`.
  Submit compares it with the head at the exact point it pins the session
  (compare-and-swap, not an earlier re-check). A mismatch is refused as
  `CapturedHeadMoved` (exit 3) before any queue entry exists. `submit --json`
  then prints `{"submitted": false, "error": {"code": "captured_head_moved",
  ...}, "collaboration_capture": <the acknowledged report>}`. That receipt retains
  a commit that was not submitted. It is harmless: it promises only that those
  bytes are kept, it answers again if that commit is ever submitted, and #659
  reclaims it like any other unreferenced capture. The existing
  identity re-check keeps the head pinned until the queue entry records it.
  Simulation, gates, promotion and later re-simulation all work from that
  recorded `head_commit` and never re-read the worktree.
- **Advisory:** the capture runs after the submit, on the `head_commit` the
  submit recorded, not on an earlier reading. Advisory never refuses, so legacy
  behaviour (submit whatever the head is) is unchanged.

Tests move the session between capture and submit in both modes, and each
binding has a neuter check.

### Output carries no host paths

`detail` is path-free. Known roots are replaced with placeholders: `<session
worktree>`, `<repository>`, `<host state>`, `<host cache>`, `<home>` and `<temp>`,
in both the given and the canonical spelling. Any remaining absolute path becomes
`<path>`. The `code` identifies the problem. There is no verbose mode with full
paths yet.

### Versioning

- The JSON field carries `schema: "aethyme.submit-capture/experimental-v0"` and
  is the last key. Every legacy field keeps its name, value and order
  (`docs/json-contracts.md`).
- The receipt inside it is #658's record, with its durability label, so an
  unsupported filesystem never reads the same as a supported one.

## Old binaries (D18): the fence

A binary that predates #660 does not know `[collaboration]` and ignores it,
because each config section is parsed independently. Left alone, it would
submit to a `required` repository without capturing. The user decided that
required capture must shut such binaries out. The only fence they honour is
`broker.db`'s compatibility floor, so the fence is built on it:

- **Schema v50** is a no-op migration, declared compatible: `MIN_COMPATIBLE_SCHEMA`
  stays 47, so repositories that do not require capture keep working with every
  0.8.2+ binary. Its number, `COLLABORATION_FENCE_SCHEMA`, is the first schema
  whose binaries implement required capture.
- **A #660+ binary raises the repository's floor to 50** when its config says
  `capture = "required"`, and records `meta.collaboration_fence =
  "collaboration capture required"`. Every pre-#660 binary (schema 49) then
  refuses the database with `SchemaTooNew`, for every command.
- **When:**
  - On every broker open, as one meta query. If the repository is not yet
    fenced, a plain read of the main checkout's working-copy config follows,
    with no Git, because opens run on every hook call.
  - On `broker status`, before a required capture, and in the promotion gate,
    under the committed-first rule those paths already read.

  The fence therefore engages at the first open by a new binary whose main
  checkout shows `required`, or at its first status, submit or promotion,
  whichever comes first.
- **Never lowered.** Raising uses the same "only upward" write as
  `record_min_compatible_schema`, and no code lowers it. **Turning `required`
  off later does not let older binaries back in.**
- **Only an explicit `required` raises it.** An unsupported or unreadable value
  still refuses submits and promotions in #660+ binaries, but does not raise the
  floor: a typo must not lock older binaries out for good.
- `broker status` reports it: `collaboration_fence: {"min_compatible_schema": 50,
  "reason": "collaboration capture required"}` in JSON, and one
  `Collaboration fence:` line in text.

What this means for operators:

- **Every binary that touches the repository must be #660 or newer** before
  `required` is committed: agents, plugin hooks, CI, other machines sharing the
  checkout.
- **Until the first new-binary open after `required` is committed, an older
  binary can still submit uncaptured work.** The fence is not retroactive.
- The floor belongs to this repository's `broker.db`. Other repositories, and
  the host-level databases, are unaffected.
- The schema number is positional. If another schema-50 migration lands first
  (open PRs #722 and #725 both claim 50; #722 also raises the global minimum to
  50), this migration and `COLLABORATION_FENCE_SCHEMA` move up together. A test
  pins the pair, so a rebase that moves one without the other fails. The fence
  must always name the first schema that implements required capture.

## Not decided here

- **D18 closes** with T33–T35 comparisons across old and new binaries on real
  commands (L9, #681), not only this binary's off mode.
- **D32:** the `collaboration_capture` field and its schema stay experimental. It
  becomes stable only when a second consumer (L3 retrieval, #661, or the opt-in
  commands, #680) reads the same receipts.
- **Explicit release and retention classes** are #659's.
- **Capture cost on large trees:** capture is synchronous inside submit. If it is
  slow, advisory mode could move to a background step, but only with a visible
  pending state, never a silent skip.

## Tests

| Plan test | State |
|---|---|
| T33 (legacy outputs unchanged when disabled) | Four off spellings: legacy exit, verdict, key order, no capture line, no state written. |
| T34 (capture failure keeps the legacy verdict) | No project; refused root; a failing gate keeps its exit code while the capture succeeds. |
| T35 (old binaries) | A required repository's floor is 50 after an open, and a schema-49 binary is refused (`schema_is_compatible_with(…, 49)`); off and advisory leave it at 47 and a schema-49 binary opens; required then off keeps the fence; an unsupported value does not raise it; status reports it. A real cross-binary run is #681's. |
| T57 (required policy) | Missing project and an unsupported policy refuse with no queue entry or ref movement; an acknowledged required capture proceeds. |
| Idempotency | A resubmit under `verify-only` returns the same operation and receipt, including after integration is refreshed onto an upstream that moved (both policies). |
| Promotion gate | A library submit without capture verifies and is refused at promotion; `promote --entry` is refused; advisory never gates. |
| Advisory isolation | Verdict printed before the capture line; a panicking capture is a `failed` report; a held lock is `in_progress` within seconds. |
| Config | Committed `required` wins over a working-copy `off`; a malformed opt-in fails closed; a malformed file without one stays off. |
