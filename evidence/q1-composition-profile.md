# Q1 composition profile (#650)

**Status: evidence recorded, maintainer review pending.** Reconciled on 2026-11-03
from E1 evidence committed on 2026-10-12. No campaign, model call or held-out case
was rerun to build this record. The machine-readable record is
[`q1-composition-profile.json`](q1-composition-profile.json); the narrative is in
[`local-v3-e1-profile.md`](../packages/aethyme/docs/architecture/local-v3-e1-profile.md).

## Selected profile

Opt-in **Mergiraf 0.19.1** (external GPL-3.0-only executable, binary SHA-256
`cda51754…20cc3c`, not linked or bundled) for HTML. Other paths keep the observed
text profile. The default `git-text-baseline` is unchanged. Every declared order
runs twice, and claimed-commutative output must be byte-identical. Only common
bases and recorded causal chains are admissible; unknown bases refuse.

## Results

| Campaign | Source | Result |
|---|---|---|
| Original FX01–FX07 | `d1910b14` | structural 16/17, text 15/17 (34 rows) |
| Browser matrix | `f2c52f8b` | structural 8/9, text 8/9, unmanaged Git 4/9 (168 observations) |
| FX04-browser synthesis | `97a152f1` | first attempt synthesized and accepted; consumer 4/4; subtraction 2/2; keeping the consumer refused 2/2 |
| Sealed SH01 | `27a1b6e9` | 4/4, byte-identical |

T16, T17, T18, T19 and T22 map to these rows in the JSON `test_mapping`.
T20 and T21 are excluded: they belong to Q3 (#650 AC4).

## Limitations

- HTML structural profile only.
- **Original FX04 is unsupported by composition.** Both composers returned a
  text candidate that failed 8 oracle checks. That candidate is unverified, is not
  counted as success, and is routed to semantic resolution.
- The semantic positive is one bounded synthesized repair (FX04-browser), not
  arbitrary semantic repair.
- Inherited-base support rests on the FX03 family only.
- HX held-out variants stayed sealed and were not rerun.
- The reports' `retained_playground` paths were under `/private/tmp` and no longer
  exist. The record cites commit, tree, snapshot and SHA-256 digests instead.
- Exact campaign argv lines were not retained; the committed `#[ignore]`
  entrypoints are listed.
- Synthesis cost: 25,971 input tokens (11,008 cached), 1,212 output tokens,
  19,338 ms. Invoiced charge and provider retries are unknown.

## Review

The maintainer confirmed the Q1 record and the D03–D06 statuses on 2026-11-03, including
D04 as `accepted_with_evidence` with its FX04 limitation stated. No independent review of
the evidence itself has been performed. The decision records are in the profile doc.
