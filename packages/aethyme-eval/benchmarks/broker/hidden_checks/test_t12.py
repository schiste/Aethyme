import unittest
from shoplib.text import slugify


class T12(unittest.TestCase):
    def test_fold(self):
        self.assertEqual(slugify("Crème Brûlée"), "creme-brulee")
        self.assertEqual(slugify("Ça va? 100%"), "ca-va-100")
        self.assertEqual(slugify("日本 tea"), "tea")

    def test_ascii_unchanged(self):
        self.assertEqual(slugify("Blue Mug (Large)"), "blue-mug-large")
