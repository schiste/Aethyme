"""Money helpers. Amounts are always integer cents."""

from shoplib.errors import ValidationError


def format_price(cents, currency="USD"):
    """Format an integer amount of cents, e.g. 123456 -> '1234.56 USD'."""
    if not isinstance(cents, int) or isinstance(cents, bool):
        raise TypeError("cents must be an int")
    sign = "-" if cents < 0 else ""
    whole, frac = divmod(abs(cents), 100)
    return f"{sign}{whole}.{frac:02d} {currency}"


def parse_price(text):
    """Parse '12.34' or '12' (an optional trailing currency code is ignored) into cents."""
    token = text.strip().split()[0] if text.strip() else ""
    if not token:
        raise ValidationError("empty price")
    negative = token.startswith("-")
    token = token.lstrip("-")
    if "." in token:
        whole, frac = token.split(".", 1)
        if len(frac) > 2 or not frac.isdigit():
            raise ValidationError(f"bad price: {text!r}")
        frac = frac.ljust(2, "0")
    else:
        whole, frac = token, "00"
    if not whole.isdigit():
        raise ValidationError(f"bad price: {text!r}")
    cents = int(whole) * 100 + int(frac)
    return -cents if negative else cents


def allocate(total, parts):
    """Split `total` cents into `parts` shares that differ by at most one cent."""
    if parts <= 0:
        raise ValueError("parts must be positive")
    base, remainder = divmod(total, parts)
    return [base + 1 if i < remainder else base for i in range(parts)]
