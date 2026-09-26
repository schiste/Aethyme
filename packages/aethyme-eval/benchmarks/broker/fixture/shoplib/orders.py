"""Order lifecycle: create, place, cancel."""

import itertools

from shoplib import events
from shoplib.errors import OutOfStock, ShopError
from shoplib.models import LineItem, Order
from shoplib.validation import validate_quantity

_ids = itertools.count(1)


class OrderService:
    def __init__(self, catalog, inventory):
        self.catalog = catalog
        self.inventory = inventory

    def create_order(self, customer_id, lines, region="US", discount_code=""):
        items = []
        for sku, qty in lines:
            product = self.catalog.get(sku)
            items.append(LineItem(sku, validate_quantity(qty), product.price_cents))
        return Order(f"O{next(_ids):05d}", customer_id, items, region, discount_code)

    def place(self, order):
        """Reserve stock for every line, all-or-nothing, and mark the order placed."""
        if order.status != "draft":
            raise ShopError(f"order {order.id} is {order.status}")
        held = []
        try:
            for item in order.items:
                self.inventory.reserve(item.sku, item.qty)
                held.append(item)
        except OutOfStock:
            for item in held:
                self.inventory.release(item.sku, item.qty)
            raise
        order.status = "placed"
        events.publish("order.placed", order)
        return order

    def cancel(self, order):
        if order.status != "placed":
            raise ShopError(f"order {order.id} is {order.status}")
        for item in order.items:
            self.inventory.release(item.sku, item.qty)
        order.status = "cancelled"
        events.publish("order.cancelled", order)
        return order
