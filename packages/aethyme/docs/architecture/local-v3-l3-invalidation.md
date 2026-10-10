# Local v3 — L3 context cache and scoped invalidation (#662)

Last Updated: 2026-10-10

This record covers how contribution-context answers (#661, `local-v3-l3-context.md`) are
cached, and what invalidates them (plan §6.5, §15.1 L3; D23, D27; T14, T15, T82, T84,
T85). The goal is in the plan's words: an unrelated edit must not invalidate every cached
context, while anything that could add a relevant match, change an answer or narrow
access must.

Code: `aethyme-broker::collaboration_context::{retrieve_cached, forget_reader}`. Library
only; a command waits for #680.

## Decided here

### Validity comes from the key

An answer is stored under its cache key, and found again only under the same key. The key
commits to every input that can change the answer:

| Input | Why |
|---|---|
| The query | Scope, exact source, analysis envelope IDs and budget. A different source never reaches another source's answer. |
| Each dependency posting's version | Every posting a match could come from: the scope paths, their directories, every ancestor as a possible brief `scope_ref`, and every path an analysis envelope relates to the scope (dynamic dependencies the envelope models). A version digests the posting's live, visible contributions with their attached brief and retention boundary. |
| Empty postings too | A cached "nothing here" depends on the keys that would have matched, so a new contribution there, or an absent path becoming present, moves the version (negative lookups). |
| The broad-risk bucket | See below. |
| The visible unreadable contributions | A contribution that could not be read may match once it can. |
| The reader's visibility class and epoch | One class never reaches another's answer, and a new epoch reaches none of the old ones. |

Versions are computed when the query runs, from the same index state the answer would be
computed from. So validity needs no push-based invalidation. Expiry of a retention
boundary, a release, a re-brief, a new capture under a dependency, or a visibility policy
that hides a contribution under a dependency, even without an epoch bump, all change a
version the key covers. An unrelated change moves none of them, and the answer stays
cached.

`retrieve_cached` computes the key inside the read transaction, before reading candidates
in detail, and returns a valid row from there. A miss continues exactly as `retrieve`, and
stores the result. A cached answer is byte-for-byte what a fresh query returns at that
moment; a property test checks this after every step of a random sequence.

### Broad-risk bucket

Some changes cannot be scoped to the paths they list. A contribution is **broad risk**
when any of these hold:
- it changes more than 64 paths;
- it changes a repository-wide manifest or configuration file at the root (`Cargo.toml`,
  `Cargo.lock`, `package.json`, lockfiles, `go.mod`, `pyproject.toml`, `Makefile`,
  `.gitattributes`, `.gitmodules` and similar);
- it changes anything under `.aethyme/` or `.github/workflows/`.

The list is provisional (D27).

Such a contribution is also posted under a single broad-risk bucket, which every answer
depends on. A broad change therefore invalidates every cached answer, which is the
plan's "broad invalidation". Answers report how many visible broad-risk contributions
exist (`cache.broad_risk`), so the reason is visible. Its paths still match as usual. Being
broad does not make it a match.

### Serving a row

A row is served only when all of these hold. The key already implies most of them;
the checks are a second line against rows the key would not reach:
- the row is for the reader's class and epoch;
- it is before its refetch bound;
- it decodes as a context record whose ID matches the stored one;
- every contribution it names is still visible to the reader.

Otherwise the answer is computed again and replaces the row (T85: a row another binary
wrote, or a damaged one, is never served).

### Incomplete dependency modelling

An answer whose candidates or related paths were cut (`truncated_by: candidates`
or `related_paths`) has a dependency set that may miss keys. It is already `partial`, with
the cut named in its gaps. It is also stored with a refetch bound of 5 minutes: after that it
is computed again even if its key still matches. The bound is provisional (D27).

An answer whose coverage is partial only because no analysis was given keeps its full
path dependency set and needs no bound. It never claims completeness.

### What is not cached

An answer that raced a concurrent change under its read, for example a brief replaced
between the index and the read, is returned with its gap but not stored: its key does not
describe it (`ContributionContext.cacheable`, `Served::Fresh { stored: false }`).

### What the cache does not keep

Cached rows are derived data, and never retained beyond what they describe:
- **Forgotten contributions.** When the index forgets a contribution (released, expired,
  reclaimed, re-briefed or newly unreadable), every cached answer that carries it is
  deleted. A released contribution's brief does not outlive it in the cache.
- **Old epochs.** Storing an answer for a class's new epoch deletes that class's rows
  from other epochs.
- **Revoked readers.** `forget_reader(store, class)` deletes a class's rows at once, for a
  revocation that must not wait.
- **Size.** At most 1024 rows per project store; the least recently used go first.

Cache absence never proves anything. A cached empty answer is evidence of absence only
when the answer itself says so, exactly as a fresh one (§6.5).

### Schema and compatibility

state.db schema 6 adds `context_cache` and `context_cache_members` (derived), and
rebuilds the derived index so broad-risk postings exist. **The floor rises to 6.** A
schema 5 binary indexes without broad-risk postings and purges no cached answer when a
contribution stops being live. A newer binary could then serve an answer that a broad
change or a release should have invalidated. No release shipped schemas 2 to 5.

## Not decided here

- **D27** closes with a measured load profile on one hot project. This slice fixes the
  invalidation rules; the broad-risk list, the 64-path threshold, the 5-minute bound and
  the 1024-row limit are provisional until then.
- **D23** closes with FX14's leakage and revocation checks. Local v0 still has one reader
  class (`LocalProject`). The cache honours any `Visibility`: per class, per epoch, never
  across classes, with immediate removal on revocation. Membership itself comes with
  Coordination.
- **T84** in full concerns shared analysis summaries under the wrong project or
  audience, which belongs to Coordination and Platform. Here: a cached answer is never
  served to another reader class or a later epoch, and counts computed for one class
  never reach another.

## Tests

| Plan test | State |
|---|---|
| T14 | A burst of unrelated captures leaves a cached answer in place; a relevant capture replaces it, and the next query hits again. |
| T15 | A new match, a re-brief, a release and an absent path becoming present each invalidate exactly the answers that depend on them; the others still hit. |
| T82 | With no analysis, queries and the cache work; a cached empty answer is still not evidence. |
| T84 (local part) | Another reader class never reaches an answer; an epoch change or `forget_reader` removes access and the rows; lookup refuses a row to a reader who cannot see what it names. |
| T85 | A row from another schema, or with a tampered record ID, is computed again, not served. |
| Others | Byte-for-byte equivalence with fresh queries over a random sequence of captures, re-briefs, releases and visibility changes, with and without epoch bumps; broad changes (manifest and over 64 paths); impact-edge dependencies; source changes; cut dependency sets and the refetch bound; raced answers not stored; purge on release; eviction; the schema 6 index rebuild. |
