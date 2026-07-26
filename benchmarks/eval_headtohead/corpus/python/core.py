"""Order pipeline core — the symbols the eval questions target."""


class OrderConfig:
    """Configuration for the order pipeline (D4 target type)."""

    def __init__(self, retries=3, region="eu"):
        self.retries = retries
        self.region = region


def load_config():
    """Return the default OrderConfig (D1/D5 target)."""
    return OrderConfig()


def validate_order(order):
    """Validate an order dict; raise on missing id (D5 'validate' target)."""
    if "id" not in order:
        raise ValueError("order missing id")
    return True


def process_order(order):
    """Process a single validated order (D1/D2 target symbol)."""
    validate_order(order)
    return {"id": order["id"], "status": "processed"}
