import unittest

from shoplib import config
from shoplib.errors import ValidationError
from shoplib.validation import validate_email, validate_quantity, validate_sku
from tests.helpers import reset_globals


class ValidationTest(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_sku(self):
        self.assertEqual(validate_sku(" mug-001 "), "MUG-001")
        with self.assertRaises(ValidationError):
            validate_sku("MUG1")

    def test_email(self):
        self.assertEqual(validate_email("Ann@Example.com"), "ann@example.com")
        with self.assertRaises(ValidationError):
            validate_email("nope")

    def test_quantity(self):
        self.assertEqual(validate_quantity(3), 3)
        config.override("max_line_quantity", 2)
        with self.assertRaises(ValidationError):
            validate_quantity(3)
        with self.assertRaises(ValidationError):
            validate_quantity(0)
