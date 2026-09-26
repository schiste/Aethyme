"""Name -> implementation registries.

Entries are "module:function" strings resolved lazily, so this file never
imports the implementations and stays cheap to load.
"""

import importlib

# --- Exporters -------------------------------------------------------------
# Format name -> "module:function" taking a list of orders, returning text.
EXPORTERS = {
    "csv": "shoplib.exporters.csv_export:export_orders_csv",
    "json": "shoplib.exporters.json_export:export_orders_json",
}


def _resolve(target):
    module_name, _, attr = target.partition(":")
    return getattr(importlib.import_module(module_name), attr)


def exporter(name):
    """Return the export function registered as `name`."""
    try:
        return _resolve(EXPORTERS[name])
    except KeyError:
        raise KeyError(f"no exporter named {name!r}") from None


def command(name):
    """Return the CLI command registered as `name`."""
    try:
        return _resolve(COMMANDS[name])
    except KeyError:
        raise KeyError(f"no command named {name!r}") from None


def names(table):
    return sorted(table)


# --- CLI commands ----------------------------------------------------------
# Command name -> "module:function" taking (args, ctx), returning text.
COMMANDS = {
    "price": "shoplib.commands:cmd_price",
    "stock": "shoplib.commands:cmd_stock",
}
