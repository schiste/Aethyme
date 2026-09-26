import unittest

from shoplib.errors import OutOfStock, UnknownSku
from tests.helpers import make_shop, reset_globals


class InventoryTest(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_reserve_and_release(self):
        _, inventory, _ = make_shop()
        inventory.reserve("MUG-001", 4)
        self.assertEqual(inventory.available("MUG-001"), 6)
        inventory.release("MUG-001", 4)
        self.assertEqual(inventory.available("MUG-001"), 10)

    def test_out_of_stock(self):
        _, inventory, _ = make_shop()
        with self.assertRaises(OutOfStock) as ctx:
            inventory.reserve("MUG-002", 3)
        self.assertEqual(ctx.exception.available, 2)

    def test_unknown(self):
        _, inventory, _ = make_shop()
        with self.assertRaises(UnknownSku):
            inventory.available("NOP-000")

    def test_low_stock(self):
        _, inventory, _ = make_shop()
        self.assertEqual(inventory.low_stock(), ["MUG-002"])
        self.assertEqual(inventory.low_stock(6), ["BAG-100", "MUG-002"])
