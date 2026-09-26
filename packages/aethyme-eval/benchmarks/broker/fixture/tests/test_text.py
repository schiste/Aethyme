import unittest

from shoplib.text import slugify, truncate


class TextTest(unittest.TestCase):
    def test_slugify(self):
        self.assertEqual(slugify("Blue Mug (Large)"), "blue-mug-large")
        self.assertEqual(slugify("  --a  b--  "), "a-b")

    def test_truncate(self):
        self.assertEqual(truncate("hello", 10), "hello")
        self.assertEqual(truncate("hello world", 8), "hello...")
