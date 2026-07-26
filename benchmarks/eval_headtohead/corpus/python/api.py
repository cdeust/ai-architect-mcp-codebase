"""HTTP API layer — uses OrderConfig (D4) and matches 'config' (D5)."""

from core import load_config, OrderConfig


def apply_config(overrides):
    """Uses OrderConfig directly (D4 ground truth)."""
    cfg = load_config()
    if isinstance(cfg, OrderConfig):
        cfg.retries = overrides.get("retries", cfg.retries)
    return cfg
