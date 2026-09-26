import unittest

from shoplib.shipping import shipping_cost


class ShippingTest(unittest.TestCase):
    def test_cost(self):
        self.assertEqual(shipping_cost(0, "US"), 499)
        self.assertEqual(shipping_cost(501, "US"), 499 + 2 * 150)
        self.assertEqual(shipping_cost(500, "XX"), 1299 + 400)
        with self.assertRaises(ValueError):
            shipping_cost(-1, "US")
