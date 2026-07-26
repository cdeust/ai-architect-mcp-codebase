"""Background worker — a real caller of process_order (D2 ground truth)."""

from core import process_order


def drain(queue):
    results = []
    for order in queue:
        results.append(process_order(order))
    return results
