# shoplib

A small order-management library: catalog, inventory reservations, pricing,
discounts, shipping, reports, exporters and a tiny CLI. Standard library
only (Python 3.9+).

## Tests

    ./run_tests.sh            # or: python3 -m unittest discover -s tests -t . -q

## Conventions

- Amounts are integer cents everywhere.
- `shoplib/registry.py` maps names to implementations as `"module:function"`
  strings; register new exporters and CLI commands there.
- Put the tests for a change in the test file your task names. Do not edit
  unrelated tests, and do not add a changelog.
