"""Plain data types."""

from dataclasses import dataclass, field


@dataclass
class Product:
    sku: str
    name: str
    price_cents: int
    weight_grams: int = 0
    tags: tuple = ()


@dataclass
class LineItem:
    sku: str
    qty: int
    unit_price_cents: int

    @property
    def total_cents(self):
        return self.qty * self.unit_price_cents


@dataclass
class Order:
    id: str
    customer_id: str
    items: list = field(default_factory=list)
    region: str = "US"
    discount_code: str = ""
    status: str = "draft"
