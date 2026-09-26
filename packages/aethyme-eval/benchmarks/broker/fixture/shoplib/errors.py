"""Exception types shared across shoplib."""


class ShopError(Exception):
    """Base class for every shoplib error."""


class ValidationError(ShopError):
    """Raised when user-supplied data is malformed."""


class UnknownSku(ShopError):
    """Raised when a SKU is not in the catalog or inventory."""


class OutOfStock(ShopError):
    """Raised when a reservation asks for more units than are available."""

    def __init__(self, sku, requested, available):
        super().__init__(f"{sku}: requested {requested}, only {available} available")
        self.sku = sku
        self.requested = requested
        self.available = available
