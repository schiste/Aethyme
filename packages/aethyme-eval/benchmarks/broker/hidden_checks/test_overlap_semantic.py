"""t05 x t08: the rename holds and the new caller works with it."""
import unittest
from shoplib.inventory import Inventory
from shoplib.subscriptions import Subscription, SubscriptionService
from tests.helpers import make_shop, reset_globals


class OverlapSemantic(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_rename_and_caller_agree(self):
        self.assertFalse(hasattr(Inventory, "reserve"))
        catalog, inventory, _ = make_shop()
        SubscriptionService(catalog, inventory).renew(Subscription("C1", "MUG-001", 2, 7))
        self.assertEqual(inventory.available("MUG-001"), 8)
