import unittest
from shoplib.models import LineItem, Order
from shoplib.reports import top_skus


class T11(unittest.TestCase):
    def test_top(self):
        orders = [
            Order("O1", "C1", [LineItem("MUG-001", 3, 1), LineItem("TEA-010", 4, 1)], status="placed"),
            Order("O2", "C2", [LineItem("BAG-100", 3, 1)], status="placed"),
            Order("O3", "C3", [LineItem("MUG-002", 50, 1)], status="draft"),
        ]
        self.assertEqual(top_skus(orders, 2), [("TEA-010", 4), ("BAG-100", 3)])
        self.assertEqual(top_skus(orders, 10), [("TEA-010", 4), ("BAG-100", 3), ("MUG-001", 3)])
        self.assertEqual(top_skus(orders, 0), [])
