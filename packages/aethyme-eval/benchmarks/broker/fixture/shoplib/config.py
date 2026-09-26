"""Static settings. Read them through `get` so tests can override them."""

DEFAULTS = {
    "currency": "USD",
    "tax_rates": {"US": 0.07, "FR": 0.20, "GB": 0.20},
    "low_stock_threshold": 5,
    "max_line_quantity": 100,
}

_overrides = {}


def get(key):
    if key in _overrides:
        return _overrides[key]
    return DEFAULTS[key]


def override(key, value):
    _overrides[key] = value


def reset():
    _overrides.clear()
