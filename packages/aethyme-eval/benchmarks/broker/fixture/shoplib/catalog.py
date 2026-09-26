"""Product catalog."""

from shoplib.errors import UnknownSku
from shoplib.validation import validate_sku


class Catalog:
    def __init__(self):
        self._products = {}

    def add(self, product):
        product.sku = validate_sku(product.sku)
        self._products[product.sku] = product
        return product

    def get(self, sku):
        try:
            return self._products[sku]
        except KeyError:
            raise UnknownSku(sku) from None

    def all(self):
        return sorted(self._products.values(), key=lambda p: p.sku)

    def search(self, query):
        """Products whose name contains `query`, ordered by SKU."""
        return [p for p in self.all() if query in p.name]
