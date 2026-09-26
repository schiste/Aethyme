import unittest
from shoplib import registry
from shoplib.models import LineItem, Order


class T03(unittest.TestCase):
    def test_tsv(self):
        orders = [Order("O1", "C1", [LineItem("MUG-001", 2, 1299), LineItem("TEA-010", 1, 850)]),
                  Order("O2", "C2", [LineItem("BAG-100", 1, 2400)])]
        lines = registry.exporter("tsv")(orders).rstrip("\n").split("\n")
        self.assertEqual(lines[0], "order_id\tcustomer_id\tsku\tqty\tunit_price_cents")
        self.assertEqual(lines[1:], ["O1\tC1\tMUG-001\t2\t1299", "O1\tC1\tTEA-010\t1\t850",
                                     "O2\tC2\tBAG-100\t1\t2400"])

    def test_existing_exporters_kept(self):
        for name in ("csv", "json", "tsv"):
            self.assertIn(name, registry.EXPORTERS)
