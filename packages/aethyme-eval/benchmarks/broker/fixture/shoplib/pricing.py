"""Order totals."""

from shoplib import config
from shoplib.discounts import apply_discount


def subtotal(order):
    return sum(item.total_cents for item in order.items)


def tax(order, taxable_cents):
    rate = config.get("tax_rates").get(order.region, 0.0)
    return round(taxable_cents * rate)


def total(order):
    """Subtotal minus discount plus tax on the discounted amount."""
    sub = subtotal(order)
    discounted = sub - apply_discount(sub, order.discount_code)
    return discounted + tax(order, discounted)
