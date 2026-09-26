import unittest

from shoplib import events
from shoplib.errors import OutOfStock, ShopError
from tests.helpers import make_shop, reset_globals


class OrdersTest(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_place_reserves_stock(self):
        _, inventory, service = make_shop()
        seen = []
        events.subscribe("order.placed", seen.append)
        order = service.place(service.create_order("C1", [("MUG-001", 3), ("TEA-010", 1)]))
        self.assertEqual(order.status, "placed")
        self.assertEqual(inventory.available("MUG-001"), 7)
        self.assertEqual(seen, [order])

    def test_place_is_all_or_nothing(self):
        _, inventory, service = make_shop()
        order = service.create_order("C1", [("MUG-001", 3), ("MUG-002", 5)])
        with self.assertRaises(OutOfStock):
            service.place(order)
        self.assertEqual(inventory.available("MUG-001"), 10)
        self.assertEqual(order.status, "draft")

    def test_cancel_releases(self):
        _, inventory, service = make_shop()
        order = service.place(service.create_order("C1", [("BAG-100", 5)]))
        service.cancel(order)
        self.assertEqual(inventory.available("BAG-100"), 5)
        with self.assertRaises(ShopError):
            service.cancel(order)
