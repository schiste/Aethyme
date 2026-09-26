"""Shared test fixtures."""

from shoplib import config, events
from shoplib.catalog import Catalog
from shoplib.inventory import Inventory
from shoplib.models import Product
from shoplib.orders import OrderService


def reset_globals():
    config.reset()
    events.clear()


def make_shop(stock=None):
    catalog, inventory = Catalog(), Inventory()
    products = [
        Product("MUG-001", "Blue Mug", 1299, 350, ("kitchen", "blue")),
        Product("MUG-002", "Red Mug", 1299, 350, ("kitchen",)),
        Product("TEA-010", "Green Tea", 850, 100, ("food",)),
        Product("BAG-100", "Canvas Bag", 2400, 600, ("outdoor", "blue")),
    ]
    stock = stock or {"MUG-001": 10, "MUG-002": 2, "TEA-010": 20, "BAG-100": 5}
    for product in products:
        catalog.add(product)
        inventory.set_stock(product.sku, stock.get(product.sku, 0))
    return catalog, inventory, OrderService(catalog, inventory)
