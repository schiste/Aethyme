import unittest
from shoplib.customers import loyalty_tier
from shoplib.models import LineItem, Order


def placed(cid, cents, status="placed"):
    # region "XX" has no tax rate, so pricing.total == subtotal
    return Order("O", cid, [LineItem("MUG-001", 1, cents)], region="XX", status=status)


class T10(unittest.TestCase):
    def test_tiers(self):
        self.assertEqual(loyalty_tier("C1", []), "bronze")
        self.assertEqual(loyalty_tier("C1", [placed("C1", 19999)]), "bronze")
        self.assertEqual(loyalty_tier("C1", [placed("C1", 10000), placed("C1", 10000)]), "silver")
        self.assertEqual(loyalty_tier("C1", [placed("C1", 50000)]), "gold")

    def test_only_this_customers_placed_orders(self):
        orders = [placed("C1", 15000), placed("C2", 90000), placed("C1", 90000, "draft"),
                  placed("C1", 90000, "cancelled")]
        self.assertEqual(loyalty_tier("C1", orders), "bronze")
