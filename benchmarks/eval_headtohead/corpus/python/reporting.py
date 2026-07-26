"""Reporting — a LEXICAL DISTRACTOR module (§3 pre-registration).

It mentions `process_order` in a comment and a string, and defines
substring-colliding names (`preprocess_order`, `process_orders`) that a
word-boundary grep for `process_order` will NOT match but a careless one might.
None of these are real call sites of core.process_order.
"""

# NOTE: process_order is the hot path; report on its throughput here.
LABEL = "process_order latency"


def preprocess_order(order):
    """Not process_order — a different function (substring collision)."""
    return dict(order)


def process_orders(orders):
    """Batch shim; iterates but does NOT call core.process_order."""
    return [preprocess_order(o) for o in orders]
