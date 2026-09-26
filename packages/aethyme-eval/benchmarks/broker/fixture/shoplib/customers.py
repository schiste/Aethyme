"""Customers."""

from dataclasses import dataclass

from shoplib.validation import validate_email


@dataclass
class Customer:
    id: str
    name: str
    email: str


class CustomerBook:
    def __init__(self):
        self._by_id = {}

    def add(self, customer):
        customer.email = validate_email(customer.email)
        self._by_id[customer.id] = customer
        return customer

    def get(self, customer_id):
        return self._by_id[customer_id]

    def find_by_email(self, email):
        email = email.strip().lower()
        for customer in self._by_id.values():
            if customer.email == email:
                return customer
        return None
