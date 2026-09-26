"""CLI command implementations. Each takes (args, ctx) and returns output text."""

from shoplib.money import format_price


def cmd_price(args, ctx):
    product = ctx.catalog.get(args[0])
    return format_price(product.price_cents)


def cmd_stock(args, ctx):
    return str(ctx.inventory.available(args[0]))
