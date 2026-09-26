"""Sales reports over placed orders."""

from collections import Counter

from shoplib import pricing


def placed(orders):
    return [o for o in orders if o.status == "placed"]


def units_by_sku(orders):
    counts = Counter()
    for order in placed(orders):
        for item in order.items:
            counts[item.sku] += item.qty
    return dict(counts)


def revenue(orders):
    return sum(pricing.total(o) for o in placed(orders))
