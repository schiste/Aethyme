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
| `CACHE_FORMAT` | The version of the selection, the result encoding, the dependency-key rules and the broad-risk rules. Another binary's rules never reach this binary's rows (below). |
| The query | Scope, exact source, analysis envelope IDs and budget. A different source never reaches another source's answer. |
| Whether each envelope is about the scope | Computed per envelope, in query order, from retained snapshots. When one is reclaimed the envelope is unchanged but this bit can flip, so a complete answer that relied on it is never served again. |
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

Otherwise the answer is computed again and replaces the row. A row another binary's
format wrote is never reached at all, because the format is in the key. The decode and ID
checks catch a damaged row (T85).

A served row reports the capture generation at which its answer was computed
(`Served::Cache`), not the one current when it was stored.

### Format version

`CACHE_FORMAT` (`aethyme-context-cache/1`) must be bumped on any change to selection, the
result encoding, dependency keys, or the broad-risk list or threshold. It is part of every
key. It is also stamped on the derived index (`meta.context_index_format`), and a store
whose stamp differs, or that has none, is rebuilt from the archive on its next query, with
its cache dropped. So a change to the broad-risk rules also reaches contributions indexed
before it.

### Storing re-checks what the answer carries

Storing is a separate transaction from the read. Under its write lock, an answer is
stored only if both of these still hold:
- every contribution it returns is indexed with the brief the answer used, and is live;
- its reader class was not revoked by `forget_reader` at or after the answer's compute
  time (a `meta` marker per class).

Otherwise nothing is stored, and `Served::Fresh { stored: false }` says so. A release, a
re-brief or a revocation that lands between the read and the store can therefore never
bring a purged answer back. A change to anything else in between only leaves a row that no
later key reaches, carrying data that is still valid. That is why the member check is
enough, and the whole key is not recomputed under the lock.

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

state.db schema 6 adds `context_cache` and `context_cache_members` (derived). The
derived index is rebuilt on first use, because it carries no format stamp yet, so
broad-risk postings exist. **The floor rises to 6.** A
schema 5 binary indexes without broad-risk postings and purges no cached answer when a
contribution stops being live. A newer binary could then serve an answer that a broad
change or a release should have invalidated. No release shipped schemas 2 to 5.

## Not decided here

- **D27** closes with a measured load profile on one hot project. This slice fixes the
  invalidation rules; the broad-risk list, the 64-path threshold, the 5-minute bound and
  the 1024-row limit are provisional until then. Notes for that measurement:
  - In a Rust repository every `Cargo.lock` bump is broad risk. Dependency updates
    are frequent, so the hit rate may collapse. The profile should measure it before the
    list is fixed, and might narrow lockfiles to "broad only when a manifest also
    changed".
  - Two processes holding different epochs for one reader class keep deleting each
    other's rows, because storing for one epoch drops the class's other epochs. They are
    still correct, but the cache thrashes. Epochs should be monotonic per class across
    processes, which comes with Coordination's membership.
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
| T85 | Another format's rows are unreachable (the format is in the key), and an index from another format is rebuilt. A row with a damaged record or record ID is computed again, not served. |
| Review fixes | Reclaiming an analysis snapshot invalidates a complete answer. A release, a re-brief (re-indexed in between) or a revocation between read and store prevents the store. A served row reports its compute generation. |
| Others | Byte-for-byte equivalence with fresh queries over a random sequence of captures, re-briefs, releases and visibility changes, with and without epoch bumps; broad changes (manifest and over 64 paths); impact-edge dependencies; source changes; cut dependency sets and the refetch bound; raced answers not stored; purge on release; eviction; the schema 6 index rebuild. |
