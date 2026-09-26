"""Command-line entry point: python -m shoplib.cli <command> [args...]."""

import sys
from dataclasses import dataclass

from shoplib import registry
from shoplib.catalog import Catalog
from shoplib.inventory import Inventory
from shoplib.models import Product


@dataclass
class Context:
    catalog: Catalog
    inventory: Inventory


def demo_context():
    catalog, inventory = Catalog(), Inventory()
    for sku, name, price, stock in [
        ("MUG-001", "Blue Mug", 1299, 40),
        ("MUG-002", "Red Mug", 1299, 3),
        ("TEA-010", "Green Tea", 850, 12),
    ]:
        catalog.add(Product(sku, name, price, 350))
        inventory.set_stock(sku, stock)
    return Context(catalog, inventory)


def main(argv=None, ctx=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if not argv:
        return "commands: " + ", ".join(registry.names(registry.COMMANDS))
    ctx = ctx or demo_context()
    return registry.command(argv[0])(argv[1:], ctx)


if __name__ == "__main__":
    print(main())
