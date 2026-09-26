import unittest
from tests.helpers import make_shop


class T07(unittest.TestCase):
    def test_case_insensitive(self):
        catalog, _, _ = make_shop()
        self.assertEqual([p.sku for p in catalog.search("mug")], ["MUG-001", "MUG-002"])
        self.assertEqual([p.sku for p in catalog.search("GREEN")], ["TEA-010"])

    def test_tag_filter(self):
        catalog, _, _ = make_shop()
        self.assertEqual([p.sku for p in catalog.search("", tag="blue")], ["BAG-100", "MUG-001"])
        self.assertEqual([p.sku for p in catalog.search("mug", tag="blue")], ["MUG-001"])
        self.assertEqual(catalog.search("", tag="nope"), [])
        self.assertEqual(len(catalog.search("")), 4)
