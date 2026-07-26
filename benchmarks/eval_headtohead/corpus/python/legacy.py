"""Legacy shim — a LEXICAL DISTRACTOR: mentions process_order only in prose."""

# TODO: migrate callers off process_order once the v2 pipeline lands.


def deprecated_entry(payload):
    return {"warning": "use gateway.handle_request instead", "payload": payload}
