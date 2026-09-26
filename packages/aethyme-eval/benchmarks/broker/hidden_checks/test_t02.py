import unittest
from shoplib.errors import ValidationError
from shoplib.validation import validate_postal_code


class T02(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(validate_postal_code(" 12345-6789 ", "US"), "12345-6789")
        self.assertEqual(validate_postal_code("02134", "US"), "02134")
        self.assertEqual(validate_postal_code("75001", "FR"), "75001")
        self.assertEqual(validate_postal_code("sw1a 1aa", "GB"), "SW1A 1AA")
        self.assertEqual(validate_postal_code("M1 1AE", "GB"), "M1 1AE")

    def test_invalid(self):
        for code, region in [("1234", "FR"), ("123456", "US"), ("12345-678", "US"),
                             ("SW1A1AA", "GB"), ("1W1A 1AA", "GB"), ("12345", "DE")]:
            with self.subTest(code=code, region=region):
                with self.assertRaises(ValidationError):
                    validate_postal_code(code, region)
