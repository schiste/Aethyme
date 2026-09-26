import unittest

from shoplib import pricing
from shoplib.discounts import apply_discount
from shoplib.errors import ValidationError
from tests.helpers import make_shop, reset_globals


class PricingTest(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_discounts(self):
        self.assertEqual(apply_discount(2000, "welcome10"), 200)
        self.assertEqual(apply_discount(300, "FIVEOFF"), 300)
        self.assertEqual(apply_discount(300, ""), 0)
        with self.assertRaises(ValidationError):
            apply_discount(300, "BOGUS")

    def test_total(self):
        _, _, service = make_shop()
        order = service.create_order("C1", [("MUG-001", 2)], region="FR", discount_code="WELCOME10")
        self.assertEqual(pricing.subtotal(order), 2598)
        self.assertEqual(pricing.total(order), 2807)  # 2598 - 259 = 2339, + 20% tax 468
