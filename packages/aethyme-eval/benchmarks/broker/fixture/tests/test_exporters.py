import json
import unittest

from shoplib import registry
from tests.helpers import make_shop, reset_globals


class ExportersTest(unittest.TestCase):
    def tearDown(self):
        reset_globals()

    def test_registered_exporters(self):
        _, _, service = make_shop()
        orders = [service.create_order("C1", [("MUG-001", 2)])]
        csv_text = registry.exporter("csv")(orders)
        self.assertEqual(csv_text.splitlines()[1].split(",")[2:], ["MUG-001", "2", "1299"])
        data = json.loads(registry.exporter("json")(orders))
        self.assertEqual(data[0]["items"][0]["qty"], 2)
        with self.assertRaises(KeyError):
            registry.exporter("yaml")
