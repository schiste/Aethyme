import unittest

from shoplib.customers import Customer, CustomerBook


class CustomersTest(unittest.TestCase):
    def test_add_and_find(self):
        book = CustomerBook()
        book.add(Customer("C1", "Ann", "Ann@Example.com"))
        self.assertEqual(book.get("C1").email, "ann@example.com")
        self.assertIs(book.find_by_email(" ANN@example.com"), book.get("C1"))
        self.assertIsNone(book.find_by_email("bob@example.com"))
