import unittest
from shoplib.shipping import shipping_cost


class T06(unittest.TestCase):
    def test_express(self):
        self.assertEqual(shipping_cost(501, "US", express=True), 2 * (499 + 2 * 150) + 500)
        self.assertEqual(shipping_cost(0, "XX", express=True), 2 * 1299 + 500)

    def test_standard_unchanged(self):
        self.assertEqual(shipping_cost(0, "FR"), 699)
        self.assertEqual(shipping_cost(501, "US"), 799)
