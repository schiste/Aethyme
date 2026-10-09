# Local v3 — L1 decision brief and token profile (#654)

Last Updated: 2026-10-09

This record covers the decision brief (plan §5.5, N4) and input to **D11** (which
tokenizer defines the 150-token budget). It builds on the record layer of
[`local-v3-l1-records.md`](local-v3-l1-records.md).

Code: `aethyme-contracts::experimental_v0::brief`. Golden vectors:
`aethyme-contracts/fixtures/experimental-v0/{brief_tokens,briefs}.json`, produced by an
independent Python implementation.

## D11: token profile `aethyme-brief-tokens/v0` (decided 2026-10-09)

Aethyme counts tokens with its own written rule, not a model vocabulary:

| Class | Characters | Cost |
|---|---|---|
| letter | ASCII `A`–`Z`, `a`–`z` | 1 per started group of 4 in a run |
| digit | ASCII `0`–`9` | 1 per started group of 2 in a run |
| two-byte | `U+0080`–`U+07FF` (Latin accents, Greek, Cyrillic, Hebrew, Arabic, …) | 1 per started group of 2 in a run |
| space | ASCII space, tab, LF, CR | free |
| other | ASCII punctuation, CJK, emoji, everything else | 1 each |

**Why this and not a model tokenizer.** The plan's close conditions for D11 are
identical counts across Rust and TypeScript, a distributable implementation, and asset
licensing. A model vocabulary (`o200k_base`) means a 2–4 MB licensed asset in the binary
and in any Worker, and two third-party implementations to keep in step. It would still
not be the agent's own tokenizer, because Claude's is not public. This rule is about 30
lines in any language, has no dependency or asset, and uses only code point ranges, so
counts cannot drift with a Unicode version.

**These are Aethyme tokens, and are labelled as such.** D11 forbids relabelling a byte
cap as a token cap. This is neither: it is a token count under a named, versioned
profile, recorded in every brief record (`token_profile`). A reader refuses a brief
counted under a profile it does not implement (`unsupported_profile`).

**Calibration.** On 27 brief-like strings in 12 scripts (English and code-heavy prose,
paths, hashes, version numbers, French, German, Greek, Russian, Hebrew, Arabic, Chinese,
Japanese, Korean, emoji), the profile counted 1.07–2.00× `o200k_base` (mean 1.45×) and
never fewer. So a 150-token brief is at most about 150 model tokens, typically about 100.
Two rules came from calibration: digits cost 1 per 2 (BPE splits numbers and hashes
finely; with 4, hashes undercounted at 0.53×), and two-byte scripts are grouped in pairs
(with 1 per character, Russian overcounted at 3.4×). The calibration is evidence for the
bound, not part of the definition. The corpus and its measured `o200k_base` counts are
stored in `fixtures/experimental-v0/brief_calibration.json`, and a test asserts the
profile never counts fewer, so a profile change that breaks the bound fails CI without
a tokenizer dependency. Re-measure when the profile changes (T61).

## Brief

| Field | Rule |
|---|---|
| `intent` | Required. |
| `decisions[]` | `scope_ref` (identifier: `[a-z0-9][a-z0-9._/-]*`, ≤ 64 bytes), `choice`, `reason`; all required. |
| `preserves[]`, `assumptions[]`, `deferred[]` | Optional lists of strings. |
| Each list | ≤ 8 entries. |
| All agent-authored text, `scope_ref` included | ≤ 150 tokens and ≤ 2048 bytes (whitespace is free, so bytes are capped separately). |
| Every string | Not empty; no control characters other than LF; no bidi controls. |

The count is additive per string, so JSON structure and field order never change it:
this is the "explicit serialization order" D11 asks for, made unnecessary by
construction.

Over a limit is refused, never truncated (§5.5). Every problem is reported at once with
its path (`decisions[1].reason`), and the over-budget message says how many tokens to cut.
Agents are the writers, so the error is written for them to act on in one round.

## Decision file vs record

- **Decision file** (written by the agent): only the brief fields. Unknown members are
  refused: in agent input a typo (`assumption`) would otherwise drop text silently, and
  `schema`, `token_profile` and `requires` are the tool's to set, not the agent's.
- **Brief record** (written by the tool, `Brief::to_record`): adds `schema`
  (`aethyme.decision-brief/experimental-v0`) and `token_profile`, and derives
  `requires` from the schema, as #653 requires of every writer path. Empty lists are
  omitted, so a brief has one encoding. Unknown members from a newer writer are carried
  (#653), and the whole record is capped at 8 KiB, so hidden metadata cannot grow beyond
  that (T02 "oversized hidden metadata").

## Outcomes (acceptance criterion 2)

| Situation | Outcome |
|---|---|
| Valid | Brief record with its own ID. Captured source is untouched. |
| Invalid | Refused with every problem listed. Nothing is truncated or rewritten, and the source capture is not affected. |
| Revised | A new record with a new ID. The earlier record is unchanged, and the contribution revision that references it changes (§5.2: metadata changes make a new revision). |
| Missing | Reported as absent. Whether a policy requires a brief is decided outside this module (#658 capture, acceptance policy). |
| Client cannot count | It submits the decision file for canonical validation and reports that result (§5.5). The Rust implementation is the canonical validator until a CLI surface exists (§6.7, `--decision-file`). |

## Not done here

- **Usefulness experiment (acceptance criterion 3, D30).** Whether a brief improves a
  real hand-off, and whether it causes confusion or harm, is measured separately on
  Playground tasks. It is not inferred from successful storage.
- **CLI surface.** `broker submit --decision-file` and `collab capture` belong to the L2
  capture work (#658).
- **TypeScript implementation.** Waits for a TS consumer. The vector files are its
  acceptance test.

## Tests

| Plan test | State |
|---|---|
| T02 (149/150/151, multilingual, escapes, oversized hidden metadata) | Done in Rust against independent vectors. |
| D11 close condition "Rust/TypeScript parity" | Vectors ready; TS pending a consumer. |
| T04–T06 | Need the contribution records and E1. |
