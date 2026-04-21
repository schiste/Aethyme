"""Unit tests for variance_decomp.

Covers:
  - synthetic data with known variance components
  - edge cases (too few rows/conditions, zero run-variance)
  - σ²_cond clamp to zero when MS_between < MS_within
  - generalizability coefficient behavior vs n_runs
  - dominant noise source detection
  - judge-excluded path (no judge_stdev)
"""

from __future__ import annotations

from variance_decomp import decompose_batch


def _row(cond: str, q: float, judge_stdev: float | None = 2.0) -> dict:
    return {"condition": cond, "quality_score": q, "judge_stdev": judge_stdev}


def test_insufficient_data_returns_none_components() -> None:
    # Only one condition — can't do ANOVA
    result = decompose_batch([
        _row("control", 60.0),
        _row("control", 65.0),
    ])
    assert result["sigma2_cond"] is None
    assert result["generalizability_coeff"] is None


def test_strong_signal_low_noise() -> None:
    """Large condition differences + tight within-condition → high Eρ²."""
    rows = []
    for q in [70, 71, 69]:
        rows.append(_row("A", q))
    for q in [50, 51, 49]:
        rows.append(_row("B", q))
    for q in [30, 31, 29]:
        rows.append(_row("C", q))

    result = decompose_batch(rows, target_reliability=0.80)
    assert result["sigma2_cond"] > 0
    assert result["generalizability_coeff"] > 0.9  # very reliable
    assert result["dominant_noise_source"] in ("judge", "run", None)


def test_condition_signal_below_noise_clamps_to_zero() -> None:
    """Same means, lots of within-condition spread → MOM gives σ²_cond < 0."""
    rows = []
    for q in [50, 70, 30]:
        rows.append(_row("A", q))
    for q in [30, 50, 70]:
        rows.append(_row("B", q))
    for q in [70, 30, 50]:
        rows.append(_row("C", q))

    result = decompose_batch(rows)
    assert result["sigma2_cond"] == 0.0
    assert result["sigma2_cond_clamped_to_zero"] is True
    assert any("clamped" in n for n in result["notes"])
    # When clamped, n_runs_needed can't be computed — it's None
    assert result["n_runs_needed_for_target"] is None


def test_judge_excluded_when_missing_stdev() -> None:
    rows = []
    for q in [70, 71]:
        rows.append(_row("A", q, judge_stdev=None))
    for q in [50, 51]:
        rows.append(_row("B", q, judge_stdev=None))

    result = decompose_batch(rows)
    assert result["sigma2_judge"] is None
    assert any("judge_stdev" in n for n in result["notes"])


def test_n_needed_grows_when_run_noise_grows() -> None:
    """Increase within-condition spread → more runs needed for same reliability."""
    def _scenario(spread: float) -> int | None:
        rows = []
        for q in [60 + spread, 60, 60 - spread]:
            rows.append(_row("A", q))
        for q in [40 + spread, 40, 40 - spread]:
            rows.append(_row("B", q))
        result = decompose_batch(rows, target_reliability=0.90)
        return result["n_runs_needed_for_target"]

    n_low = _scenario(2)
    n_high = _scenario(10)
    # Both are ints, with n_high >= n_low (higher run noise needs more N)
    assert n_low is not None and n_high is not None
    assert n_high >= n_low


def test_dominant_noise_source_judge_when_judge_stdev_large() -> None:
    rows = []
    for q in [70, 70, 70]:
        rows.append(_row("A", q, judge_stdev=20.0))
    for q in [40, 40, 40]:
        rows.append(_row("B", q, judge_stdev=20.0))
    result = decompose_batch(rows)
    # Run variance is ~0, judge variance is 400 → judge dominates
    assert result["dominant_noise_source"] == "judge"


def test_dominant_noise_source_run_when_runs_spread() -> None:
    rows = []
    for q in [50, 90, 30]:
        rows.append(_row("A", q, judge_stdev=1.0))
    for q in [40, 80, 20]:
        rows.append(_row("B", q, judge_stdev=1.0))
    result = decompose_batch(rows)
    assert result["dominant_noise_source"] == "run"


def test_empty_rows_insufficient() -> None:
    result = decompose_batch([])
    assert result["sigma2_cond"] is None
    assert result["n_rows"] == 0


def test_realistic_aethyme_batch_4_conditions_3_runs() -> None:
    """Realistic shape: 4 conditions × 3 runs each, moderate noise."""
    # control-cto-off: ~50, control-cto-on: ~55, explore: ~65, leverage: ~60
    rows = []
    for q in [48, 52, 50]:
        rows.append(_row("control-cto-off", q, judge_stdev=2.5))
    for q in [54, 57, 54]:
        rows.append(_row("control-cto-on", q, judge_stdev=2.0))
    for q in [62, 68, 65]:
        rows.append(_row("explore", q, judge_stdev=3.0))
    for q in [58, 62, 60]:
        rows.append(_row("leverage", q, judge_stdev=2.5))

    result = decompose_batch(rows)
    assert result["n_conditions"] == 4
    assert result["n_rows"] == 12
    assert result["mean_runs_per_condition"] == 3.0
    # Should be a meaningful (positive) signal with decent reliability
    assert result["sigma2_cond"] > 10  # condition spread is real
    assert result["generalizability_coeff"] > 0.5


if __name__ == "__main__":
    tests = [v for k, v in list(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"PASS {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL {t.__name__}: {e}")
        except Exception as e:
            failed += 1
            print(f"ERROR {t.__name__}: {type(e).__name__}: {e}")
    raise SystemExit(1 if failed else 0)
