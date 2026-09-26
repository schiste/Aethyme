import unittest
from shoplib.errors import OutOfStock
from shoplib.models import LineItem
from shoplib.subscriptions import Subscription, SubscriptionService
from tests.helpers import make_shop, reset_globals


class T08(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_renew_holds_stock(self):
        catalog, inventory, _ = make_shop()
        service = SubscriptionService(catalog, inventory)
        item = service.renew(Subscription("C1", "TEA-010", 3, 30))
        self.assertEqual(item, LineItem("TEA-010", 3, 850))
        self.assertEqual(inventory.available("TEA-010"), 17)

    def test_short_stock_holds_nothing(self):
        catalog, inventory, _ = make_shop()
        service = SubscriptionService(catalog, inventory)
        with self.assertRaises(OutOfStock):
            service.renew(Subscription("C1", "MUG-002", 3, 30))
        self.assertEqual(inventory.available("MUG-002"), 2)
