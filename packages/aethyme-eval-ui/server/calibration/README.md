# Judge Calibration Set

A calibration set is a small collection of **hand-scored candidate outputs**
used to detect drift in the LLM-judge over time. The judge may be
intra-rater consistent (same input → same score within ~5 points) but
still drift across weeks as the underlying model changes or prompts
are tweaked. Intra-rater stability ≠ accuracy; calibration against
human-anchor scores catches the blind spot.

## Layout

```
calibration/
├── README.md                       (this file)
├── mediawiki-bug-fix-1/
│   ├── 001-high-quality.json       (human_score = 85)
│   ├── 002-middle-quality.json     (human_score = 55)
│   └── 003-low-quality.json        (human_score = 20)
├── mediawiki-dead-code/
│   └── …
└── aethyme-dead-code/
    └── …
```

One directory per `eval_type`. Each item is a single JSON file with the
candidate text and a human-assigned reference score. Use 3–10 items per
eval type spanning the score range (at least one each of high/mid/low).

## Item schema

```json
{
  "calibration_id":   "mediawiki-bug-fix-1-001",
  "eval_type":        "bug-fix-1",
  "target":           "mediawiki",
  "candidate":        "Full text of the agent output we're scoring…",
  "human_score":      75,
  "notes":            "Well-structured analysis; hits 4/5 files but misses the RevisionRecord substitution detail",
  "task_override":    null,
  "reference_override": null
}
```

- `candidate`: the agent output text as it would appear to the judge.
- `human_score`: 0-100, our anchor. One trusted reviewer sets this; the
  judge's mean should land within ~15 points to be considered calibrated.
- `task_override` / `reference_override`: optional overrides if this
  calibration item uses a specialized task or custom reference; leave
  null to use the scenario's standard task + reference.

## Running a drift check

```bash
# Score every calibration item for this eval_type and compare to human anchors
curl -X POST http://localhost:8000/api/judge/calibration-check \
  -H "Content-Type: application/json" \
  -d '{"eval_type": "bug-fix-1"}'
```

Response:
```json
{
  "eval_type": "bug-fix-1",
  "items_scored": 3,
  "max_drift": 8.3,
  "mean_drift": 5.1,
  "passes": true,
  "threshold": 15,
  "per_item": [
    {"calibration_id": "…", "human_score": 85, "judge_mean": 79.7, "drift": 5.3},
    …
  ]
}
```

If `passes == false`, the judge has drifted beyond the threshold and
**scores from recent batches should be treated as suspect**. Either the
judge prompt changed, the model changed under us, or the calibration
set is itself stale. Investigate before trusting new comparisons.

## Adding calibration items

1. Pick a past eval run's agent output worth anchoring on (ideally one
   with a clearly defined quality level — a confidently-great answer, a
   clearly-broken answer, and a few in between).
2. Hand-score it on the 0–100 scale using the same rubric the judge sees.
3. Write a JSON file under `calibration/<eval-type>/` following the
   schema above.
4. Commit the file. The next drift check will include it automatically.

**Scoring guidance** (matches judge instructions):
- 0–20: wrong/irrelevant
- 21–40: on-topic but missing key elements
- 41–60: some correct elements with significant gaps
- 61–80: most correct elements, minor gaps
- 81–100: comprehensive and accurate
