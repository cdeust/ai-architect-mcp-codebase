"""Inbound gateway — a real caller of process_order (D2 ground truth)."""

from core import process_order, validate_order


def handle_request(order):
    validate_order(order)
    return process_order(order)
