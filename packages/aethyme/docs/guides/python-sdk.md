# Python SDK

Last Updated: 2026-03-06

The Python SDK is a thin client for the active core API.

## Install

```bash
cd packages/aethyme/sdk/python
python3 -m pip install -e .
```

## Authenticate

```python
from aethyme_sdk import AethymeClient

client = AethymeClient(token="your-bearer-token-or-api-key")
```

## Use

```python
from aethyme_sdk import AethymeClient

client = AethymeClient(token="your-bearer-token-or-api-key")
search = client.search.query("GraphStore", limit=5)
print(search)
```

## Scope Rule

The SDK should stay minimal and only cover endpoints that are active in core.
