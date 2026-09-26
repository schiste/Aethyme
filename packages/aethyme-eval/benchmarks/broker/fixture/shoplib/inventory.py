"""Stock levels and reservations."""

from shoplib import config
from shoplib.errors import OutOfStock, UnknownSku


class Inventory:
    def __init__(self):
        self._on_hand = {}
        self._reserved = {}

    def set_stock(self, sku, qty):
        self._on_hand[sku] = qty
        self._reserved.setdefault(sku, 0)

    def available(self, sku):
        if sku not in self._on_hand:
            raise UnknownSku(sku)
        return self._on_hand[sku] - self._reserved[sku]

    def reserve(self, sku, qty):
        """Hold `qty` units of `sku` for an order. Raises OutOfStock."""
        free = self.available(sku)
        if qty > free:
            raise OutOfStock(sku, qty, free)
        self._reserved[sku] += qty

    def release(self, sku, qty):
        """Return previously reserved units to the free pool."""
        if sku not in self._reserved:
            raise UnknownSku(sku)
        self._reserved[sku] = max(0, self._reserved[sku] - qty)

    def low_stock(self, threshold=None):
        """SKUs whose available quantity is below `threshold`, sorted."""
        if threshold is None:
            threshold = config.get("low_stock_threshold")
        return sorted(s for s in self._on_hand if self.available(s) < threshold)
