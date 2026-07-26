"""Application entry point (D3 process root)."""

from gateway import handle_request
from api import apply_config


def main():
    apply_config({"retries": 5})
    return handle_request({"id": "A-1"})


if __name__ == "__main__":
    main()
