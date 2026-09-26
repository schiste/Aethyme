"""A tiny synchronous event bus."""

_subscribers = {}


def subscribe(topic, handler):
    _subscribers.setdefault(topic, []).append(handler)


def publish(topic, payload):
    for handler in _subscribers.get(topic, []):
        handler(payload)


def clear():
    _subscribers.clear()
