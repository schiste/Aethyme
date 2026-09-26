import json


def export_orders_json(orders):
    payload = [
        {
            "order_id": o.id,
            "customer_id": o.customer_id,
            "status": o.status,
            "items": [{"sku": i.sku, "qty": i.qty, "unit_price_cents": i.unit_price_cents} for i in o.items],
        }
        for o in orders
    ]
    return json.dumps(payload, sort_keys=True)
