"""Input validation."""

import re

from shoplib import config
from shoplib.errors import ValidationError

_SKU = re.compile(r"^[A-Z]{3}-\d{3,5}$")
_EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[a-z]{2,}$", re.IGNORECASE)


def validate_sku(sku):
    sku = sku.strip().upper()
    if not _SKU.match(sku):
        raise ValidationError(f"invalid SKU: {sku!r}")
    return sku


def validate_email(email):
    email = email.strip()
    if not _EMAIL.match(email):
        raise ValidationError(f"invalid email: {email!r}")
    return email.lower()


def validate_quantity(qty):
    if not isinstance(qty, int) or qty <= 0:
        raise ValidationError(f"quantity must be a positive int, got {qty!r}")
    if qty > config.get("max_line_quantity"):
        raise ValidationError(f"quantity {qty} exceeds the per-line maximum")
    return qty
