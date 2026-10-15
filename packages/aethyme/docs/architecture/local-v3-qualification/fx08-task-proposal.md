# FX08 Local brief usefulness: task review proposal

Last Updated: 2026-10-15

> Historical task proposal. The approved tasks were constructed, frozen and run;
> see the [once-final FX08 results](../local-v3-fx08-qualification.md).


Status: proposed task descriptions; no fixtures frozen or trials run.

Issue #709 requires these descriptions to be reviewed before fixture construction.
This proposal covers Local cold-start tasks and real L3 retrieval. It does not
implement E1 or claim structural composition qualification. All source work and
oracle execution use disposable Playground repositories.

## Four declared tasks

1. **Optional feature configuration.** Agent A keeps a distinction between an
   absent setting and an explicitly empty setting because an existing consumer
   uses empty to disable an optional feature. Agent B is asked to simplify the
   parser and improve invalid-value diagnostics. The task must be completed
   while preserving the established consumer's disable behavior. The rationale
   lives in the consumer contract, rather than a new hint in the changed parser.

2. **Stable pagination ordering.** Agent A retains a stable creation-order tie
   break for entries with equal priority because callers resume a page using
   that order. Agent B is asked to extract sorting into a helper and add a new
   priority filter. The filter and refactor must work without silently omitting
   or repeating equal-priority entries across pages. The brief explains the
   cursor consequence; the independent oracle follows the cursor contract.

3. **Historical protocol spelling.** Agent A preserves a particular serialized
   field spelling because an independent old consumer reads it, while internal
   names use a cleaner spelling. Agent B is asked to clean up internal names and
   diagnostics. The updated implementation must satisfy the request and remain
   consumable by the existing reader. The brief must describe compatibility,
   rather than provide a patch or disclose hidden assertions.

4. **Unknown completion after interruption.** Agent A preserves an explicit
   unknown state when a worker loses its completion acknowledgment because the
   operation may still have happened. Agent B is asked to improve status output
   and deduplicate recovery notifications. The result must remain truthful and
   must not replay an unknown external operation. The brief describes why a
   success-shaped or retryable default would change the existing contract.

## Two reserved held-out task descriptions

5. **Opaque identifier escaping.** A prior change preserves raw identifier bytes
   across display escaping because a separate lookup consumes the original
   identifier. The follow-up improves display formatting and filtering while
   preserving exact lookup behavior. The fixture and oracle stay sealed until
   the final held-out stage.

6. **Empty versus unavailable lookup.** A prior change preserves the distinction
   between a completed empty lookup and an unavailable lookup because callers
   make different fallback decisions. The follow-up simplifies a reporting
   adapter and adds a bounded display field without changing those decisions.
   The fixture and oracle stay sealed until the final held-out stage.

## Frozen trial protocol

Use one small Rust fixture family without third-party dependencies. Write each
oracle from a separate written behavioral contract, before implementing the
fixture; the oracle checks both B's requested change and A's surviving decision.
Each task receives correct, absent, incomplete and misleading untrusted briefs.
Keep framing identical and non-absent brief lengths comparable; validate the
existing 150-token profile without changing it.

Freeze source, separate oracle, variants, prompts and runner configuration at one
recorded revision before any model trial. Use the same pinned runner/model,
input/output/turn/time budgets and retry policy for every arm. Deliver through
actual L3 retrieval and record authorized retrieval, whether the agent reads it,
actual behavior, wall time, input/output tokens, retries and full failure types.
Any misleading brief must leave independent policy and passing criteria intact.

The declared matrix is four tasks × four variants × three repeats = 48 trials.
Unseal the two reserved tasks only after those trials and run once per variant =
eight final held-out trials. Keep all raw artifacts outside fixture repositories;
report all 56 rows, shortfalls, null effects and harm, without selecting a best
run. Do not edit fixtures, briefs, policy, retrieval ranking, product code or
oracles in response to outcomes. A necessary correction creates a new frozen
version and retains the earlier results.

Usefulness is measured rather than assumed; no minimum gain is required to
report an honest result. Any task failure or loss of the earlier obligation
counts against the behavior denominator. The report is limited to this model,
budget, retrieval profile and six-task distribution. FX17's twenty-change
sequence and E1's composition qualification remain distinct required evidence.
