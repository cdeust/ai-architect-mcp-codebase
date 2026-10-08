"""The bootstrap's manifest pins: what is hashed, and how, in one place.

`bin/ensure-binary.sh` pins two digests; `check_distribution_identity.py` enforces
them and `repin_bootstrap_digests.py` rewrites them. All three must hash the same
bytes, so the definition lives here and the shell side mirrors `package_table`.

Pre: `data` is the bytes of a Cargo manifest.
Post: `package_table(data)` is the `[package]` table, from its header up to (not
including) the next line that opens a table, each line terminated by "\\n" -
what `cargo_package_table` in `bin/ensure-binary.sh` feeds to sha256 (awk prints
each line and a trailing "\\n"). Both refuse a manifest holding a NUL byte: BSD
awk drops the rest of such a line, so no awk-based extraction can hash it as
Python does. `digest` also refuses an empty table, so no pin of sha256("") can
be written or pass the gate while binding nothing.
"""
import hashlib
from pathlib import Path

# (manifest path, the shell variable pinning its digest, digest of what)
PINS = (
    (".claude-plugin/plugin.json", "EXPECTED_PLUGIN_MANIFEST_SHA256", "whole"),
    ("Cargo.toml", "EXPECTED_CARGO_PACKAGE_SHA256", "package"),
)

PACKAGE_HEADER = b"[package]"


def package_table(data: bytes) -> bytes:
    if b"\x00" in data:
        raise ValueError("Cargo manifest holds a NUL byte")
    lines = data.split(b"\n")
    if lines and lines[-1] == b"":
        lines.pop()
    out = []
    inside = False
    for line in lines:
        if line.rstrip(b" \t") == PACKAGE_HEADER:
            inside = True
        elif inside and line.startswith(b"["):
            break
        if inside:
            out.append(line + b"\n")
    return b"".join(out)


def digest(root: Path, path: str, scope: str) -> str:
    data = (root / path).read_bytes()
    if scope == "package":
        data = package_table(data)
        if not data:
            raise ValueError(f"{path} has no [package] table to pin")
    return hashlib.sha256(data).hexdigest()
