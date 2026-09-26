import unittest

from shoplib.errors import ValidationError
from shoplib.money import allocate, format_price, parse_price


class MoneyTest(unittest.TestCase):
    def test_format(self):
        self.assertEqual(format_price(123456), "1234.56 USD")
        self.assertEqual(format_price(5, "EUR"), "0.05 EUR")
        self.assertEqual(format_price(-250), "-2.50 USD")

    def test_format_rejects_non_int(self):
        with self.assertRaises(TypeError):
            format_price(1.5)

    def test_parse(self):
        self.assertEqual(parse_price("12.34"), 1234)
        self.assertEqual(parse_price("12.3 USD"), 1230)
        self.assertEqual(parse_price("-7"), -700)
        with self.assertRaises(ValidationError):
            parse_price("1.234")

    def test_allocate(self):
        self.assertEqual(allocate(100, 3), [34, 33, 33])
        self.assertEqual(sum(allocate(1001, 7)), 1001)


if __name__ == "__main__":
    unittest.main()
