"""Regression tests for `get_model` reasoning override.

The bug being guarded against:

    return ModelConfig(
        cfg.name, cfg.provider, cfg.backend, cfg.backend_args, reasoning,
    )

`ModelConfig` field order is
``name, provider, backend, backend_args, input_cost_per_m,
output_cost_per_m, reasoning``. The five-positional-arg call lands
`reasoning` in the `input_cost_per_m` slot, leaves
`output_cost_per_m` and `reasoning` at their defaults (0.0 and
``"default"``), and silently zeros every cost-based metric for any
eval that overrode reasoning.

The fix uses keyword args. These tests:

1. Confirm cost fields survive a reasoning override (the regression).
2. Confirm reasoning lands where it should.
3. Confirm the no-override path returns the original config unchanged
   (so we haven't inadvertently broken the happy path).
4. Confirm an unknown model raises (other invariants of the function).
"""

from __future__ import annotations

import pytest

from src.eval.orchestrator import MODELS, ModelConfig, get_model


def test_reasoning_override_preserves_cost_fields():
    """Regression: `get_model("gpt-5.4", reasoning="low")` must return
    a config with the original cost fields intact, not zeros."""
    base = MODELS["gpt-5.4"]
    overridden = get_model("gpt-5.4", reasoning="low")
    assert overridden.input_cost_per_m == base.input_cost_per_m, (
        f"input cost zeroed by reasoning override: "
        f"{overridden.input_cost_per_m} (expected {base.input_cost_per_m})"
    )
    assert overridden.output_cost_per_m == base.output_cost_per_m, (
        f"output cost zeroed by reasoning override: "
        f"{overridden.output_cost_per_m} (expected {base.output_cost_per_m})"
    )


def test_reasoning_override_actually_overrides():
    """The reasoning value must land in the reasoning field — earlier
    bug had it landing in `input_cost_per_m` instead."""
    overridden = get_model("haiku", reasoning="low")
    assert overridden.reasoning == "low"


@pytest.mark.parametrize("model_key", sorted(MODELS))
def test_no_override_returns_canonical_config_unchanged(model_key: str):
    """When the requested reasoning matches the canonical default
    (`"default"` or whatever the model declares), `get_model`
    should return the exact same instance — no copy, no field drift."""
    canonical = MODELS[model_key]
    assert get_model(model_key) is canonical
    assert get_model(model_key, reasoning="default") is canonical
    assert (
        get_model(model_key, reasoning=canonical.reasoning) is canonical
    )


@pytest.mark.parametrize("model_key", sorted(MODELS))
def test_reasoning_override_preserves_every_other_field(model_key: str):
    """Defense in depth: copy every non-reasoning field forward.
    A field added to `ModelConfig` later will surface here when
    the override branch fails to forward it."""
    base = MODELS[model_key]
    overridden = get_model(model_key, reasoning="custom-test-mode")
    # Every field except `reasoning` must round-trip.
    assert overridden.name == base.name
    assert overridden.provider == base.provider
    assert overridden.backend == base.backend
    assert overridden.backend_args == base.backend_args
    assert overridden.input_cost_per_m == base.input_cost_per_m
    assert overridden.output_cost_per_m == base.output_cost_per_m
    assert overridden.reasoning == "custom-test-mode"


def test_unknown_model_raises_keyerror_with_available_list():
    """Sanity: invariants of the function around unknown models
    are unchanged by the fix."""
    with pytest.raises(KeyError) as excinfo:
        get_model("not-a-real-model")
    msg = str(excinfo.value)
    # The error message lists every known model so the user can
    # correct the typo without rummaging through source.
    for known in MODELS:
        assert known in msg


def test_modelconfig_field_order_pinned():
    """Pin the dataclass field order so a future reordering can't
    silently reintroduce the original bug. If a contributor needs
    to reorder fields, they'll trip this test and have to update
    `get_model` to use keyword args (which it now does, but a
    refactor that switches it back to positional would crash here)."""
    field_names = [f.name for f in ModelConfig.__dataclass_fields__.values()]
    assert field_names == [
        "name",
        "provider",
        "backend",
        "backend_args",
        "input_cost_per_m",
        "output_cost_per_m",
        "reasoning",
    ], (
        f"ModelConfig field order changed: {field_names}. "
        "Update get_model to keep using keyword args; the original "
        "bug was a positional-arg miscount, and a reorder makes it "
        "easier to reintroduce."
    )
