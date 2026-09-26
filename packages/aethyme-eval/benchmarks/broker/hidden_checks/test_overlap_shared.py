"""t03 x t09: both registry entries survive, and the originals remain."""
import unittest
from shoplib import registry


class OverlapShared(unittest.TestCase):
    def test_both_entries(self):
        self.assertEqual(set(registry.EXPORTERS), {"csv", "json", "tsv"})
        self.assertEqual(set(registry.COMMANDS), {"price", "stock", "low-stock"})
        registry.exporter("tsv")
        registry.command("low-stock")
