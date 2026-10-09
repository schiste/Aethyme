# Local v3 — L1 record encoding and compatibility (#653)

Last Updated: 2026-10-09

This record covers the part of #653 that does not depend on E1: the canonical encoding
every record uses, how a record is identified, and how old and new clients read each
other's records. It is input to **D32** (which records and ports are stable), **D47**
(how schemas extend without breaking old clients) and **D18** (what stays compatible),
and builds on the common encoding rules accepted for D10
([`local-v3-l1-identity.md`](local-v3-l1-identity.md)).

The concrete records of plan §5.4 (ContributionRevision, Candidate, the receipts) are
**not** defined here. Their fields wait for E1 (#650: lineage, FX03, FX07) and for two
real consumers (§5.1). Each will be a `RecordSchema` on top of this layer.

Code: `aethyme-contracts::experimental_v0::{canonical_json, record}`. Golden vectors:
`aethyme-contracts/fixtures/experimental-v0/{canonical_json,records}.json`, produced by
an independent Python implementation built on the standard `json` module's hooks.

## Canonical JSON profile (v0)

RFC 8785 (JCS) restricted to an I-JSON subset (RFC 7493):

| Rule | Why |
|---|---|
| UTF-8, no BOM; ≤ 1 MiB; depth ≤ 32; ≤ 4096 entries per container; strings ≤ 64 KiB | Bounded input (§5.3 "finite bounded fields"). |
| **No `null`** | An optional value is absent. One meaning, one encoding. |
| **Integers only**, within ±(2^53 − 1); no fraction, exponent or `-0` | No precision loss in a JavaScript consumer (T01 "large integers"). Larger quantities and counters are decimal strings. This also removes ECMAScript float formatting, the hardest part of JCS to reproduce. |
| Duplicate keys refused, compared after unescaping | Parsers disagree on duplicates (most keep the last), so two consumers could read different records from the same bytes. |
| Lone surrogate escapes and noncharacters refused | I-JSON; a lone surrogate cannot round-trip through UTF-8. |
| Members sorted by UTF-16 code units; minimal escaping; no whitespace | JCS. The vectors include a key set whose UTF-16 order differs from code point order. |
| No normalization of content | §5.3: no Unicode or line-ending normalization. |

## Record envelope

| Member | Rule |
|---|---|
| `schema` (required) | Exact type and version, e.g. `aethyme.contribution-revision/experimental-v0`. Unknown → `unsupported_schema`. No "closest version" fallback. |
| `requires` (optional set) | Capabilities a reader must understand. Unknown → `unsupported_capability`. This is how a newer writer stops an older reader from acting on a record it would misread (§5.3 "reject unknown required capabilities"). |
| `extensions` (optional object) | Carried without interpretation and included in the digest (§5.3 "opaque extension"). |
| Declared fields | Typed by the schema: boolean, integer, string, decimal string, string set, state, opaque. |

**Identity.** `sha256:` over `"aethyme record v0" NUL` followed by the canonical bytes.
Before hashing, string sets are sorted and refused if they repeat a member. A member whose
meaning equals its absence (an empty `requires`, a state written as `"unknown"`) is
dropped, so equivalent encodings always share an ID (T01). Arrays keep their order. A
record never contains its own ID, a signature, a locator or an observation time.

## Compatibility rules (D18, D47, T85)

| Situation | Reader behaviour |
|---|---|
| State field absent (an old writer, or an optional capture that failed) | Reads as `Unknown`. |
| State value the reader does not know (a newer writer) | Reads as `Unrecognized`. |
| Either of the above | Can never match `accepted`, `complete`, `trusted` or any other value: `StateReading::is` is true only for `Known`. |
| Record requires a capability the reader lacks | Refused, never partially read. |
| Different schema version | Refused. |
| Top-level member the schema does not declare | **Carried without interpretation** (decided 2026-10-09): it decodes, and stays in the canonical bytes and the ID. |
| A field the reader knows is gated, present without its capability in `requires` | Refused (`undeclared_capability`), naming the field and the capability to add. |

The vector file defines an "old" and a "new" reader of one illustrative schema; the
cases record what each reader accepts, carries, reads as unknown or unrecognized, and
refuses. They include the risk itself: an old reader carries a gated field whose writer
forgot `requires`. The new reader refuses the same record.

## `requires` is derived, never remembered

Carrying unknown fields keeps old clients working when a field is added, but it puts all
the safety in `requires`. An old reader cannot tell a harmless new field from one it
must understand. A missing `requires` entry means it silently acts on a record it
misreads.

Aethyme's writers are agents, and an agent will not reliably remember to fill a field
that has no visible effect on its own run. So the rule is built so that nobody has to
remember:

1. **The schema decides, once.** Each field declares the capability needed to interpret
   it (`FieldSpec::capability`), or `None` when ignoring it cannot mislead (a note, a
   diagnostic). This is decided by whoever adds the field, and reviewed with the schema
   change.
2. **Writers derive `requires`.** Every writer path takes it from
   `RecordSchema::required_capabilities`, not from agent input. A field an agent supplies
   (a decision file, a brief) never sets `requires` directly.
3. **Every current reader checks.** A record with a gated field and no matching
   capability is refused with `undeclared_capability`, and the error says what to add.
   A writer that bypassed rule 2 is caught by the first current reader, before any old
   reader sees the record.

A new *value* of an existing state field needs no capability: old readers read it as
`Unrecognized`, which never matches anything.

Writer paths that must follow rule 2: the brief and decision file (#654), capture records
and receipts (#658), and lineage records (#657).

## Tests

| Plan test | State |
|---|---|
| T01 (golden vectors: duplicate keys, large integers, set order, absent/null) | Canonical profile and record envelope: done in Rust against independent vectors. TypeScript side pending a TS consumer. Concrete records wait for their schemas. |
| T85 (unknown fields through old/new consumers) | Done for the envelope: old/new reader fixtures cover unknown and unrecognized states, unknown capabilities, carried fields, and gated fields without `requires`. |
| T02 (brief profile) | #654. |
| T03–T06 | Need the concrete records and E1. |
