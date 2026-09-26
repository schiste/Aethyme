"""Discount codes."""

from shoplib.errors import ValidationError

# code -> (kind, value); kind is "percent" or "fixed" (cents)
DISCOUNTS = {
    "WELCOME10": ("percent", 10),
    "FIVEOFF": ("fixed", 500),
}


def apply_discount(subtotal_cents, code):
    """Return the discount amount in cents (never more than the subtotal)."""
    if not code:
        return 0
    try:
        kind, value = DISCOUNTS[code.upper()]
    except KeyError:
        raise ValidationError(f"unknown discount code: {code!r}") from None
    if kind == "percent":
        amount = subtotal_cents * value // 100
    else:
        amount = value
    return min(amount, subtotal_cents)
