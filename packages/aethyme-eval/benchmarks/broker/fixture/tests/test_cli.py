import unittest

from shoplib import cli


class CliTest(unittest.TestCase):
    def test_listing_and_commands(self):
        self.assertTrue(cli.main([]).startswith("commands: "))
        self.assertIn("price", cli.main([]))
        self.assertEqual(cli.main(["price", "MUG-001"]), "12.99 USD")
        self.assertEqual(cli.main(["stock", "MUG-002"]), "3")
