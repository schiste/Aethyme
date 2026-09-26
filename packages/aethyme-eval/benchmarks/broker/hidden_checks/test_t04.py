import unittest
from shoplib.money import format_price


class T04(unittest.TestCase):
    def test_grouping(self):
        self.assertEqual(format_price(123456789, "USD", thousands_sep=","), "1,234,567.89 USD")
        self.assertEqual(format_price(-100000, "EUR", thousands_sep=" "), "-1 000.00 EUR")
        self.assertEqual(format_price(99999, thousands_sep=","), "999.99 USD")
        self.assertEqual(format_price(100000, thousands_sep=","), "1,000.00 USD")

    def test_default_unchanged(self):
        self.assertEqual(format_price(123456789), "1234567.89 USD")
