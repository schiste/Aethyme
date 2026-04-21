"""Method-of-moments variance decomposition for multi-run eval batches.

A single batch produces rows indexed by (condition, run_index). We want
to separate the signal (does condition matter?) from the noise
(run-to-run and judge noise), and report a generalizability coefficient
analog of Cronbach's alpha (Eρ²) so the user can say "how reliable is
this measurement?"

Model
-----
For row i with condition c[i] and run r[i], judged with s samples:

    Y_i = μ + α_{c[i]} + γ_{r[i]|c[i]} + δ_{judge|i}

where
    σ²_cond   = Var(α)     — signal (what we care about)
    σ²_run    = Var(γ)     — run-to-run noise at fixed condition
    σ²_judge  = Var(δ)     — judge noise on same candidate

σ²_judge is measured per-row as judge_stdev² (from intra-rater samples)
so it's the one component we observe directly — no estimation needed.

σ²_cond + σ²_run come from 1-way ANOVA (conditions as factor) via MOM:

    MS_between = SS_between / (J - 1)
    MS_within  = SS_within  / (N - J)
    σ²_run     = MS_within
    σ²_cond    = max(0, (MS_between - MS_within) / n̄_per_cond)

Scenario variance σ²_scen is not estimated — most Aethyme batches run
one scenario, so it's not separable from condition-level variance.
When scenarios_per_batch > 1 we could extend this to a 2-way decomposition;
not needed yet.

Generalizability coefficient
----------------------------
    Eρ² = σ²_cond / (σ²_cond + σ²_run/n_runs + σ²_judge/n_samples)

Interpretation:
    >= 0.80 : the scorecard is reliable enough to compare conditions
    >= 0.60 : usable, but wide CIs; treat verdicts as suggestive
    <  0.60 : not reliable; increase N, reduce judge noise, or both

Design recommendations
----------------------
Given the observed components, we compute the N_runs needed to reach a
target reliability (default 0.80):

    N_needed = ceil(σ²_run / (σ²_cond * (1/target - 1) - σ²_judge/n_samples))

If σ²_cond is ~0 (all conditions converge into the noise floor) we can't
reach a positive reliability by adding runs — we flag "condition signal
below noise floor; scenario may not discriminate".

Pure statistics module — no numpy/scipy dependency. Aethyme's typical
batch sizes (~12-36 rows) don't reward REML's sophistication over MOM.
"""

from __future__ import annotations

import math
import statistics
from typing import Any


# Default targets for reliability + the quality metric key.
DEFAULT_TARGET_RELIABILITY = 0.80
QUALITY_KEY = "quality_score"
JUDGE_STDEV_KEY = "judge_stdev"


def _mean(values: list[float]) -> float:
    return statistics.mean(values) if values else 0.0


def _safe_div(num: float, denom: float) -> float:
    return num / denom if denom else 0.0


def decompose_batch(
    rows: list[dict[str, Any]],
    *,
    quality_key: str = QUALITY_KEY,
    judge_stdev_key: str = JUDGE_STDEV_KEY,
    n_judge_samples_default: int = 3,
    target_reliability: float = DEFAULT_TARGET_RELIABILITY,
) -> dict[str, Any]:
    """Return variance components + generalizability info for one batch.

    `rows` is a list of dicts (or sqlite3.Row) with at minimum a
    `condition` field and `quality_score`. `judge_stdev` is optional —
    if absent, σ²_judge is reported as None and excluded from the
    reliability formula.

    Returns:
      {
        sigma2_cond, sigma2_run, sigma2_judge,          # variance components
        sigma2_cond_clamped_to_zero: bool,              # true if MOM gave negative
        generalizability_coeff,                         # Eρ² (may be None)
        dominant_noise_source: "run" | "judge" | None,
        n_conditions, n_rows, mean_runs_per_condition,
        n_runs_needed_for_target,                       # null if unreachable
        notes: [str],                                   # human-readable caveats
      }
    """
    notes: list[str] = []

    # Group quality scores by condition
    by_cond: dict[str, list[float]] = {}
    judge_stdevs: list[float] = []
    for row in rows:
        cond = row["condition"] if "condition" in row.keys() else row.get("condition")
        q = row[quality_key] if quality_key in row.keys() else row.get(quality_key)
        if cond is None or q is None:
            continue
        by_cond.setdefault(cond, []).append(float(q))
        js = row[judge_stdev_key] if judge_stdev_key in row.keys() else row.get(judge_stdev_key)
        if js is not None:
            judge_stdevs.append(float(js))

    n_conditions = len(by_cond)
    n_rows = sum(len(v) for v in by_cond.values())

    if n_conditions < 2 or n_rows < n_conditions + 1:
        # Need at least 2 conditions and more rows than conditions
        # (otherwise MS_within is undefined).
        return {
            "sigma2_cond": None,
            "sigma2_run": None,
            "sigma2_judge": None,
            "sigma2_cond_clamped_to_zero": False,
            "generalizability_coeff": None,
            "dominant_noise_source": None,
            "n_conditions": n_conditions,
            "n_rows": n_rows,
            "mean_runs_per_condition": (
                round(n_rows / n_conditions, 2) if n_conditions else 0.0
            ),
            "n_runs_needed_for_target": None,
            "target_reliability": target_reliability,
            "notes": (
                ["insufficient data: need >=2 conditions and >1 row per condition"]
            ),
        }

    # Grand mean
    grand_mean = _mean([q for qs in by_cond.values() for q in qs])

    # 1-way ANOVA sums-of-squares
    ss_between = 0.0
    ss_within = 0.0
    for qs in by_cond.values():
        m = _mean(qs)
        ss_between += len(qs) * (m - grand_mean) ** 2
        for q in qs:
            ss_within += (q - m) ** 2

    df_between = n_conditions - 1
    df_within = n_rows - n_conditions

    ms_between = _safe_div(ss_between, df_between)
    ms_within = _safe_div(ss_within, df_within)

    # Effective n per condition — unbalanced design adjustment (classical MOM:
    # use mean_runs_per_condition for σ²_cond estimator). For balanced designs
    # this equals the common group size.
    mean_n_per_cond = n_rows / n_conditions

    sigma2_run = ms_within
    sigma2_cond_raw = _safe_div(ms_between - ms_within, mean_n_per_cond)
    sigma2_cond = max(0.0, sigma2_cond_raw)
    sigma2_cond_clamped = sigma2_cond_raw < 0

    # σ²_judge: mean of per-row judge_stdev² (judge_stdev reflects the
    # stdev across the N intra-rater samples for that row, so stdev² is
    # that row's judge-level variance).
    if judge_stdevs:
        sigma2_judge = _mean([s * s for s in judge_stdevs])
    else:
        sigma2_judge = None
        notes.append("no judge_stdev values; σ²_judge excluded from Eρ²")

    # Generalizability coefficient — average n_runs per condition is what
    # the formula uses for the denominator's run term.
    n_runs = mean_n_per_cond
    n_samples = float(n_judge_samples_default)

    judge_component = (sigma2_judge / n_samples) if sigma2_judge is not None else 0.0
    denom = sigma2_cond + _safe_div(sigma2_run, n_runs) + judge_component
    gen_coeff = _safe_div(sigma2_cond, denom) if denom > 0 else None

    # Dominant noise source — compare sigma2_run/n_runs vs judge/n_samples
    run_noise = _safe_div(sigma2_run, n_runs)
    dominant: str | None
    if run_noise == 0.0 and judge_component == 0.0:
        dominant = None
    elif judge_component > run_noise:
        dominant = "judge"
    else:
        dominant = "run"

    # N_runs needed to hit target reliability — solve the Eρ² formula
    # for n_runs given target = σ²_cond / (σ²_cond + σ²_run/n + σ²_judge/s).
    # n = σ²_run / (σ²_cond*(1/target - 1) - σ²_judge/s)
    n_needed: int | None = None
    if sigma2_cond > 0:
        headroom = sigma2_cond * (1.0 / target_reliability - 1.0) - judge_component
        if headroom > 0:
            n_needed = max(1, math.ceil(sigma2_run / headroom))
        else:
            notes.append(
                "judge noise alone exceeds the reliability budget — "
                "reducing judge samples or blinding won't help; shrink σ²_judge"
            )

    if sigma2_cond_clamped:
        notes.append(
            "σ²_cond estimate clamped to 0 (MS_between < MS_within); "
            "condition signal is below the run-noise floor for this batch"
        )
    if n_rows < 5 * n_conditions:
        notes.append(
            f"small batch (n_rows={n_rows}, n_conditions={n_conditions}); "
            f"variance component estimates are imprecise"
        )

    return {
        "sigma2_cond": round(sigma2_cond, 3),
        "sigma2_run": round(sigma2_run, 3),
        "sigma2_judge": (
            round(sigma2_judge, 3) if sigma2_judge is not None else None
        ),
        "sigma2_cond_clamped_to_zero": sigma2_cond_clamped,
        "generalizability_coeff": (
            round(gen_coeff, 3) if gen_coeff is not None else None
        ),
        "dominant_noise_source": dominant,
        "n_conditions": n_conditions,
        "n_rows": n_rows,
        "mean_runs_per_condition": round(mean_n_per_cond, 2),
        "n_judge_samples": int(n_samples),
        "n_runs_needed_for_target": n_needed,
        "target_reliability": target_reliability,
        "notes": notes,
    }
