import csv
import io


def export_orders_csv(orders):
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="\n")
    writer.writerow(["order_id", "customer_id", "sku", "qty", "unit_price_cents"])
    for order in orders:
        for item in order.items:
            writer.writerow([order.id, order.customer_id, item.sku, item.qty, item.unit_price_cents])
    return buf.getvalue()
