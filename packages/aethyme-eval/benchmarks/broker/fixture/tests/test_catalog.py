import unittest

from shoplib.errors import UnknownSku
from tests.helpers import make_shop


class CatalogTest(unittest.TestCase):
    def test_get_and_search(self):
        catalog, _, _ = make_shop()
        self.assertEqual(catalog.get("TEA-010").name, "Green Tea")
        self.assertEqual([p.sku for p in catalog.search("Mug")], ["MUG-001", "MUG-002"])
        with self.assertRaises(UnknownSku):
            catalog.get("NOP-000")
