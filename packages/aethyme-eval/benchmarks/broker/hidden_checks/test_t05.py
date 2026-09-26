import unittest
from shoplib.errors import OutOfStock
from shoplib.inventory import Inventory
from tests.helpers import make_shop, reset_globals


class T05(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_renamed(self):
        self.assertFalse(hasattr(Inventory, "reserve"))
        inv = Inventory()
        inv.set_stock("MUG-001", 3)
        inv.reserve_stock("MUG-001", 2)
        self.assertEqual(inv.available("MUG-001"), 1)
        with self.assertRaises(OutOfStock):
            inv.reserve_stock("MUG-001", 2)

    def test_orders_still_place(self):
        _, inventory, service = make_shop()
        service.place(service.create_order("C1", [("MUG-001", 4)]))
        self.assertEqual(inventory.available("MUG-001"), 6)
