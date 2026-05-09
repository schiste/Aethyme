# Rejected eval-tuning changes

Last Updated: 2026-05-09

## Purpose

This document records changes to the eval framework, prompts, scoring,
or skill that were **proposed and rejected** because they would have
been forms of eval-overfitting. The cardinal rule is in
[`CLAUDE.md`](../../../CLAUDE.md):

> **Never modify tools, engine, pipeline, or skills to improve eval
> scores.** Evals are diagnostics, not targets.

Recording rejections matters as much as recording approvals. A future
contributor (or a future me) running into a similar idea needs the
"why not" — without it, the same change re-surfaces every six months
and someone eventually merges it.

Each entry follows the shape:

- **Proposal:** what was suggested.
- **Why it would have helped a score:** the eval-overfitting risk.
- **Why we rejected it:** the principle violated.
- **What to do instead:** the legitimate alternative, if any.

---

## 2026-05-08 — Tighten the leverage prompt's intent guidance

**Proposal.** The pre-2026-05-08 leverage prompts named the canonical
intent (`usage_boundary_query` / `behavior_localization_query`) and
included a fenced bash block showing the exact `aethyme-explore`
invocation, plus a list of response fields to read. Several
optimizations on top were proposed: tighten the bash block, narrow
the `--scope` flag, list the verification steps to run, hint at
which output adapter to read first.

**Why it would have helped a score.** Each tightening pushes the
leverage condition's cost downward by handing the agent a shorter
path to the answer. The discoverability gap (explore vs leverage)
narrows correspondingly, making the leverage condition look more
effective.

**Why we rejected it.** The leverage-vs-explore comparison measures
a specific phenomenon: the cost of "agent told the tool exists"
relative to "skill present, no instruction." A prompt that names the
intent, scopes the call, and lists fields is not measuring tool
availability — it's measuring great prompt engineering. We were
biasing the discoverability gap toward "small," which made the
deployed skill look more discoverable than it actually is.

**What we did instead.** 2026-05-08 commit `104c045` trimmed all six
per-eval-type leverage hints to a single shared pointer at
`SKILL.md` and the wrapper script. The agent must read the skill
(or experiment) to learn how to invoke it — same as a real user.
This widens the gap measurement, but the wider gap is the honest
signal. If the gap is too wide, fix the SKILL by making it more
discoverable (legitimate); do not narrow it by handing the agent
the answer in the prompt (eval-tuning).

---

## 2026-04-23 — Bulk-tighten all eval prompts to use the schema as scaffolding

**Proposal.** Restructure every eval-type prompt (bug-fix-1,
dead-code, impact-analysis, etc.) to lead with the JSON output
schema as a scaffolding example, then ask the agent to fill it in.
Rationale: clearer formatting yields cleaner JSON, fewer parse
failures, higher quality scores.

**Why it would have helped a score.** Quality scoring is exact-string
based on parsed JSON fields. Prompts that scaffold the schema
upfront produce more parseable output. Across 4 conditions × 9 eval
types, we'd expect ~5–10% quality bump per eval just from cleaner
parsing.

**Why we rejected it.** This wasn't tuning the *score* — it was
tuning the *output format the scorer expects*. Same effect: making
the eval easier without changing what the agent can do. Worse, it
would have masked a real diagnostic: agents that produce malformed
JSON in the wild are also producing malformed JSON in production
flows. The eval surfacing this is the eval working.

**What we did instead.** Kept the prompts schema-aware but minimal
(one example shape). When parse failures crop up in eval output, we
either accept them as the diagnostic ("this model is bad at
schemas") or fix the underlying tooling (e.g., `prompts_writer.py`
shell-quoting bugs, fixed in commit history under "shell-quoting").

---

## 2026-04-12 — Match Python's 24-line `--scope` widening as the "right answer"

**Proposal.** During the MediaWiki dead-code eval, the Rust
`aethyme-engine` produced a `--scope`-narrowed answer that was
narrower than the Python `aethyme explore` answer by a few files.
Proposal: widen the Rust default scope by ~24 lines of context to
match Python's behavior, since "Python is the reference."

**Why it would have helped a score.** Quality scoring compares
agent-listed files to a reference list. A narrower Rust answer
matches fewer reference entries and scores lower.

**Why we rejected it.** The reference list was built on Python's
output. Modifying Rust to match Python's default scope would have
been retrofitting one implementation to look like the other for
*scoring purposes only*, with no underlying engineering reason.
Worse, the Python default scope was itself accidental — picked at
implementation time, never load-bearing — so locking Rust to match
would have entrenched a value nobody had reviewed.

**What we did instead.** Re-built the reference list from scratch
based on a hand-curated walk of `includes/Watchlist/`, treating
both implementations as candidates against the same reference. Both
diverged from the new reference in different directions, which was
the actual diagnostic.

---

## 2026-03-28 — Add a "recall-boost" mode to the explore agent during evals

**Proposal.** Detect when the agent is mid-eval (via env var or
prompt sentinel) and switch the explore engine into a more
recall-heavy mode: lower trust thresholds, more candidate files
returned, longer answer arrays. Rationale: more candidates = higher
recall = better score on enumeration-style evals (impact-analysis,
migration).

**Why it would have helped a score.** Recall-heavy evals (migration,
impact-analysis) score on the fraction of reference symbols found.
Returning more candidates strictly improves recall.

**Why we rejected it.** This is the textbook eval-overfitting
pattern: detect the eval, change the behavior. In production, an
explore agent that returns 50 candidates per query is worse than
one that returns 10 well-ranked ones (read-noise, attention budget,
etc.). Tuning for the eval would have made the eval green while
making the production tool worse. Also: the env-var sentinel itself
is a leakage path — once it exists, agents in the wild can set it
to get the "more recall" behavior, defeating the production
defaults.

**What we did instead.** Left the explore behavior identical for
eval and production. When recall-heavy evals score low, that's
the diagnostic — *production agents will face the same recall
ceiling*. The fix path is: change the trust/ranking algorithm
*globally*, validate it on both eval and production traffic, and
ship if both improve. (We did this in 2026-04 with the dynamic
trust escalation in `explore.rs:516-523`.)

---

## How to add an entry

When you find yourself proposing a change to prompts, scoring,
skill content, or engine defaults, ask the cardinal-rule question:

> Would I make this change if the eval didn't exist?

If the answer is "no" — record it here. Each entry should include:

- The exact proposal (the smaller and more specific the better;
  vague entries don't help future contributors recognize the same
  pattern).
- The score that would have moved (with rough magnitude if known).
- The principle violated.
- The legitimate alternative path, if there is one.

This file is the audit trail for cardinal-rule enforcement. Empty
entries are worse than none.
