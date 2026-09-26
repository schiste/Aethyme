# Explore re-baseline, September 2026

Last Updated: 2026-09-26

The Phase 3–4 ranking work (#331–#346) did not make Explore a better ranker
than a strong lexical baseline. On a private held-out set of 40 questions,
current main scores a span MRR of .384, statistically tied with a
few-dozen-line BM25+ ripgrep script (.284, p=.20) and not better than the
BM25F build it started from (.393, p=.71). It also lost ground on one metric:
the primary-answer path MRR fell from .519 to .424 (p=.037), and that loss
was never recovered. What did improve is what an agent pays to use it. After
#339, an agent answered 12 development questions in fewer turns (4.62 against
5.50) and for less money ($0.054 against $0.073 per question) than it did with
the previous build, and in fewer turns than with ripgrep alone.

This report gives the before and after, including where results got worse.

## How it was measured

| Set | Questions | Repos | Status |
| --- | --- | --- | --- |
| Held-out v1 (Phase 3) | 20 | 3 | Spent: read during the miss analysis, now in the development set |
| Development set | 123 | 15 | For iteration; see `packages/aethyme-eval/devset/README.md` |
| Held-out v2 (private) | 40 | 4 | Run twice, now spent. Aggregates only in this report |

- **Metrics.** Span overlap is the headline: a hit counts only when a returned
  span overlaps an accepted line range. Path recall and MRR are the older,
  more lenient metric. Every comparison between two systems is paired by
  question (exact McNemar for recall, paired bootstrap for MRR).
- **Baselines.** naive rg (a strawman), BM25 over ripgrep counts, and BM25+
  (stemmed terms, code files only, a path bonus). BM25+ is the bar Explore
  has to clear.
- **Power.** At N=123 the minimal detectable paired difference is about 12.6
  points of recall. By the same formula it is about 22 points at N=40.
- **Held-out v2 audit.** The 40 questions were audited independently before
  the first run: 12 answer fixes and 10 rewordings.

## Held-out v1: the bar was not met

Top-3 went from 3/20 on v0.8.1 to 12/20, against a pre-set bar of 14/20.
Analysing the eight misses meant reading the questions, so the set stopped
measuring generalization and became part of the development set
(`"origin": "heldout-v1-spent"`).

## Held-out v2: span MRR

| System | Commit | Run 1 | Run 2 |
| --- | --- | --- | --- |
| Explore, pre-program | 53b0e884 | .381 | |
| Explore, BM25F (#337) | efc9536f | .393 | .393 |
| Explore, current main | cca923da | | .384 |
| BM25+ rg | | .284 | |
| BM25 rg | | .098 | |

Paired tests for current main (cca923da):

| Against | p |
| --- | --- |
| efc9536f (BM25F) | .71 |
| BM25+ rg | .20 |
| BM25 rg | <.001 |

Explore clearly beats plain BM25. It does not separate from BM25+ at this
sample size, and nothing merged after #337 moved the held-out number.

## Held-out v2: the regression

| Metric | 53b0e884 | efc9536f | cca923da |
| --- | --- | --- | --- |
| Primary-answer path MRR | .519 | .426 (p=.032) | .424 (p=.037) |

Both p-values are paired against 53b0e884. The BM25F ranking (#337) pushed
primary answers down on the held-out repositories, and no later change
brought them back.

## Pre-registered criteria

| Run | Criterion | Result |
| --- | --- | --- |
| 1 | Beat pre-program Explore (53b0e884) | Not met |
| 1 | Tie BM25+ rg | Met |
| 1 | Answer-rule precision ≥95% | Not met (4/6) |
| 2 | Beat efc9536f | Not met |
| 2 | Tie BM25+ rg | Met |
| 2 | Answer-rule precision ≥95% | Not met |
| 2 | Recover the primary-answer regression | Not met |

## Development set: 123 questions, main a520b0ca

Full tables, with 95% intervals and per-repo, per-kind and per-origin
breakdowns, are in
`packages/aethyme-eval/devset/results/2026-09-24-main-a520b0ca.md`.

| System | Span R@1/3/8 | Span MRR | Path R@1/3/8 | Path MRR |
| --- | --- | --- | --- | --- |
| Explore | 29/45/62 | .319 | 42/66/89 | .464 |
| BM25+ rg | 29/44/57 | .310 | 48/74/96 | .522 |
| BM25 rg | 18/32/45 | .210 | 24/50/80 | .339 |
| naive rg | 1/7/15 | .039 | 4/12/34 | .112 |

On spans, Explore and BM25+ are level. On paths, BM25+ leads: path MRR
−.058 for Explore, p=.076. That is not significant, but it points the wrong
way.

## What changed, and what it did

| Change | Effect | Merged |
| --- | --- | --- |
| #334: stop promoting graph-free hits to `answer[]` | The rule's precision was 2/9 on the development set and 4/6 on held-out v2, far below the 95% bar | Yes |
| #337: BM25F ranking | Development-set gains; the held-out primary-answer regression above | Yes |
| #339: lean default output | Same ranking; fewer agent turns and lower cost (below) | Yes |
| #340: corpus-wide average document length | On the 61-question development set of the time: span MRR .306→.328 (p=.027), path MRR .424→.455 (p=.009) | Yes |
| Doc→code pivot | Gentle variant neutral; aggressive variant worse (path MRR −.031, p=.042) | No |

The answer rule is still evaluated and reported in
`observability.source_fallback.answer_safety`, so its precision stays
measurable, but it promotes nothing.

## Agent turns

12 development questions, 2 runs each, Sonnet:

| Arm | Correct | Turns | Tokens | Cost per question |
| --- | --- | --- | --- | --- |
| Explore after the #339 trim | 24/24 | 4.62 | 75.0k | $0.054 |
| Explore before the trim | 23/24 | 5.50 | 101.7k | $0.073 |
| ripgrep only | 23/24 | 7.62 | 113.4k | $0.061 |

Paired cost against the pre-trim build: −$0.019 per question (95% CI −.031
to −.010). The trimmed build is also cheaper than the ripgrep arm and needs
three fewer turns. Accuracy was at the ceiling for all
three arms, so this benchmark says nothing about ranking quality.

## Where it got worse

- **Primary answers on held-out v2.** Path MRR for the primary answer fell
  from .519 to .424 (p=.037) and stayed there through current main.
- **`config_read` questions.** On the development set, Explore's path MRR
  for the 22 `config_read` questions is .411 against .632 for BM25+, and its
  R@3 is 10/22 against 17/22. This is the largest per-kind gap.
- **Path-level ranking.** On the full development set BM25+ leads on every
  path cut (R@1 48 vs 42, R@3 74 vs 66, R@8 96 vs 89) and on path MRR (.522
  vs .464).
- **The answer rule.** It was switched off, not fixed: agents get no
  `safe_to_use_as_answer=true` from graph-free Explore.

## Lesson

Development-set wins of 3 to 10 points did not transfer to a held-out set of
about 40 questions, and #337, which the development set rated a gain, cost
primary answers on the held-out set. Explore's demonstrated value today is
fewer and cheaper agent turns, not better ranking than a strong lexical
baseline, and claims should say so.

## Next

- Build a held-out set of at least 100 questions, so that a 10-point
  difference is detectable, and keep it unread until it is run.
- Evaluate at pinned SHAs of every target repository, so reruns compare the
  same code.
- Treat BM25+ as the bar for any future ranking change, on both span and
  path metrics, and investigate `config_read` and the primary-answer
  regression first.
