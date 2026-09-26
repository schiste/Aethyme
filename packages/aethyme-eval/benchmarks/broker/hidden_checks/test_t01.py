import unittest
from shoplib.money import format_price


class T01(unittest.TestCase):
    def test_symbols(self):
        self.assertEqual(format_price(123456, "USD", symbol=True), "$1234.56")
        self.assertEqual(format_price(5, "EUR", symbol=True), "€0.05")
        self.assertEqual(format_price(-250, "GBP", symbol=True), "-£2.50")
        self.assertEqual(format_price(100, "JPY", symbol=True), "1.00 JPY")

    def test_default_unchanged(self):
        self.assertEqual(format_price(123456), "1234.56 USD")
        self.assertEqual(format_price(-250, "EUR"), "-2.50 EUR")
