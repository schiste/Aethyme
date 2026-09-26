import unittest

from shoplib import reports
from tests.helpers import make_shop, reset_globals


class ReportsTest(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_units_and_revenue(self):
        _, _, service = make_shop()
        a = service.place(service.create_order("C1", [("MUG-001", 2)]))
        b = service.place(service.create_order("C2", [("MUG-001", 1), ("TEA-010", 4)]))
        draft = service.create_order("C3", [("TEA-010", 9)])
        self.assertEqual(reports.units_by_sku([a, b, draft]), {"MUG-001": 3, "TEA-010": 4})
        self.assertEqual(reports.revenue([a, b, draft]), round(2598 * 1.07) + round(4699 * 1.07))
