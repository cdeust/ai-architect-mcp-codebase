#!/usr/bin/env python3
"""Proof that a code move changed no function body.

Usage: python3 scripts/check_moved_fn_bodies.py <before-ref> <after-ref> <file>...

Run from the repository root. Every function of every listed file is read at
<before-ref> and at <after-ref>, its text is whitespace-normalised, and the two
sides are compared as sets of (name, body). A function that moved from one
listed file to another is unchanged; a function that is missing on one side, or
present on both with a different body, is reported and the exit code is 1.

A ref is a git ref (`git show <ref>:<file>`) or `dir:<path>` to read the files
from a directory, which is how a mutated copy is checked. A file absent at a
ref counts as empty, so a file created by the move needs no special case.

Functions are found by brace matching that skips strings, character literals
and comments. Two same-named functions in one side are kept apart (the sides
are multisets), so a duplicated or dropped helper is not hidden.

source: issue #352 review, the split of src/graph_cache.rs into
graph_cache.rs and graph_cache_tests.rs.
"""

import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

_FN = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")


def _skip(text, i):
    """Index after the string, char literal or comment starting at i, else i."""
    if text.startswith("//", i):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    if text.startswith("/*", i):
        end = text.find("*/", i + 2)
        return len(text) if end < 0 else end + 2
    if text[i] == '"':
        j = i + 1
        while j < len(text) and text[j] != '"':
            j += 2 if text[j] == "\\" else 1
        return j + 1
    if text[i] == "'" and text[i + 2 : i + 3] == "'":
        return i + 3
    if text[i] == "'" and text[i + 1 : i + 2] == "\\":
        end = text.find("'", i + 2)
        return len(text) if end < 0 else end + 1
    return i


def _body_end(text, start):
    """Index after the body of the function whose name ends at `start`.

    A declaration without a body (`fn f();`) ends before its semicolon.
    """
    depth, i, opened = 0, start, False
    while i < len(text):
        skipped = _skip(text, i)
        if skipped != i:
            i = skipped
            continue
        char = text[i]
        if char == "{":
            depth, opened = depth + 1, True
        elif char == "}":
            depth -= 1
        elif char == ";" and not opened:
            return i
        i += 1
        if opened and depth == 0:
            return i
    return i


def fn_bodies(text):
    """Counter of (name, whitespace-normalised source) for every fn in text."""
    found = Counter()
    pos = 0
    for match in _FN.finditer(text):
        if match.start() < pos:
            continue  # nested in a function already taken whole
        end = _body_end(text, match.end())
        found[(match.group(1), " ".join(text[match.start() : end].split()))] += 1
        pos = end
    return found


def read_at(ref, path):
    if ref.startswith("dir:"):
        target = Path(ref[4:]) / path
        return target.read_text() if target.exists() else ""
    shown = subprocess.run(
        ["git", "show", f"{ref}:{path}"], capture_output=True, text=True
    )
    return shown.stdout if shown.returncode == 0 else ""


def side(ref, files):
    total = Counter()
    for path in files:
        total += fn_bodies(read_at(ref, path))
    return total


def compare(before, after):
    """(lost, added, changed) as sorted name lists."""
    lost, added = before - after, after - before
    lost_names = {name for name, _ in lost}
    added_names = {name for name, _ in added}
    changed = sorted(lost_names & added_names)
    return (
        sorted(lost_names - set(changed)),
        sorted(added_names - set(changed)),
        changed,
    )


def main(argv):
    if len(argv) < 4:
        print(__doc__.split("\n\n")[1].strip(), file=sys.stderr)
        return 2
    before_ref, after_ref, files = argv[1], argv[2], argv[3:]
    before, after = side(before_ref, files), side(after_ref, files)
    lost, added, changed = compare(before, after)
    print(f"fn bodies before={sum(before.values())} after={sum(after.values())}")
    print(f"only before: {lost}")
    print(f"only after: {added}")
    print(f"changed: {changed}")
    return 1 if (lost or added or changed) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
