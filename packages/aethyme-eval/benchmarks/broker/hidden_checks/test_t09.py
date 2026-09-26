import unittest
from shoplib import cli, registry


class T09(unittest.TestCase):
    def test_command(self):
        self.assertEqual(cli.main(["low-stock"]), "MUG-002")
        self.assertEqual(cli.main(["low-stock", "20"]), "MUG-002\nTEA-010")
        self.assertEqual(cli.main(["low-stock", "0"]), "none")

    def test_registered(self):
        for name in ("price", "stock", "low-stock"):
            self.assertIn(name, registry.COMMANDS)
