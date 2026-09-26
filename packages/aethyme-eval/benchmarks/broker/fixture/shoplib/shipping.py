"""Shipping cost estimation."""

# region -> (base cents, cents per started 500 g)
RATES = {"US": (499, 150), "FR": (699, 200), "GB": (699, 220)}


def shipping_cost(weight_grams, region):
    if weight_grams < 0:
        raise ValueError("weight must be non-negative")
    base, per_block = RATES.get(region, (1299, 400))
    blocks = -(-weight_grams // 500)  # ceil
    return base + blocks * per_block
