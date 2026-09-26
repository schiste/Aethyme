"""t01 x t04: both options must survive the merge and compose."""
import unittest
from shoplib.money import format_price


class OverlapTextual(unittest.TestCase):
    def test_both_options_compose(self):
        self.assertEqual(format_price(123456789, "USD", symbol=True, thousands_sep=","), "$1,234,567.89")
        self.assertEqual(format_price(-100000, "EUR", symbol=True, thousands_sep=" "), "-€1 000.00")
