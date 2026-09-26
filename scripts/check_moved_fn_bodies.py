#!/usr/bin/env python3
"""Proof that a code move changed no function body.

Usage: python3 scripts/check_moved_fn_bodies.py <before-ref> <after-ref> <file>...

Run from the repository root. Every function of every listed file is read at
<before-ref> and at <after-ref> and the two sides are compared as multisets of
(name, text). A function that moved from one listed file to another is
unchanged; a function that is missing on one side, or present on both with a
different text, is reported and the exit code is 1. Exit 2 means there was
nothing to compare: a usage error, or a side with no function at all, which is
what a wrong ref or a mistyped path produces.

The text of a function starts at the first outer attribute or doc comment
that precedes it (`#[test]`, `#[should_panic(..)]`, `#[cfg(..)]`, `///`) and
includes its visibility and qualifiers (`pub(crate)`, `const`, `async`,
`unsafe`, `extern "C"`), so removing or changing any of them is detected.
Whitespace is collapsed outside literals only: string, byte string, raw string
and character literals are compared byte for byte.

A ref is a git ref (`git show <ref>:<file>`) or `dir:<path>` to read the files
from a directory, which is how a mutated copy is checked. A file absent at a
ref counts as empty, so a file created by the move needs no special case.

Functions are found by brace matching that skips literals (including raw
strings `r#"..."#` with any number of `#`, `b".."`, `br".."`, `c".."` and
char literals such as `'\''`) and comments (nested block comments too);
a lifetime such as `'a` is not a literal. Two same-named functions in one side
are kept apart (the sides are multisets), so a duplicated or dropped helper is
not hidden.

source: issue #352 review, the split of src/graph_cache.rs into
graph_cache.rs and graph_cache_tests.rs; hardened by issue #362.
"""

import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_QUALIFIERS = {"pub", "const", "async", "unsafe", "extern", "default"}


def _string_end(text, i):
    """Index after the escaped string whose opening quote is at i."""
    j = i + 1
    while j < len(text) and text[j] != '"':
        j += 2 if text[j] == "\\" else 1
    return min(j + 1, len(text))


def _raw_end(text, i):
    """Index after the raw string whose `#`s or quote start at i."""
    hashes = 0
    while i + hashes < len(text) and text[i + hashes] == "#":
        hashes += 1
    if text[i + hashes : i + hashes + 1] != '"':
        return None
    close = '"' + "#" * hashes
    end = text.find(close, i + hashes + 1)
    return len(text) if end < 0 else end + len(close)


def _char_end(text, i):
    """Index after the char literal at i, or None when it is a lifetime."""
    if text[i + 1 : i + 2] == "\\":
        end = text.find("'", i + 3)
        return len(text) if end < 0 else end + 1
    if text[i + 2 : i + 3] == "'":
        return i + 3
    return None


def _comment_end(text, i):
    """Index after the comment starting at i, or None."""
    if text.startswith("//", i):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    if not text.startswith("/*", i):
        return None
    depth, j = 1, i + 2
    while j < len(text) and depth:
        if text.startswith("/*", j):
            depth, j = depth + 1, j + 2
        elif text.startswith("*/", j):
            depth, j = depth - 1, j + 2
        else:
            j += 1
    return j


def _literal_end(text, i):
    """Index after the literal starting at i (prefix included), or None."""
    char = text[i]
    if char == '"':
        return _string_end(text, i)
    if char == "'":
        return _char_end(text, i)
    word = _WORD.match(text, i)
    if not word or word.group() not in ("r", "b", "br", "c", "cr"):
        return None
    prefix, nxt = word.group(), word.end()
    if prefix.endswith("r"):
        return _raw_end(text, nxt)
    if text[nxt : nxt + 1] == '"':
        return _string_end(text, nxt)
    if prefix == "b" and text[nxt : nxt + 1] == "'":
        return _char_end(text, nxt)
    return None


def _skip(text, i):
    """Index after the literal or comment starting at i, else i."""
    end = _comment_end(text, i)
    if end is None:
        end = _literal_end(text, i)
    return i if end is None else end


def _matching(text, i, open_char, close_char):
    """Index after the bracket that closes the one at i."""
    depth = 0
    while i < len(text):
        skipped = _skip(text, i)
        if skipped != i:
            i = skipped
            continue
        if text[i] == open_char:
            depth += 1
        elif text[i] == close_char:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return i


def _body_end(text, start):
    """Index after the body of the function whose name ends at `start`.

    A declaration without a body (`fn f();`) ends before its semicolon. Before
    the body opens, a `;` inside parentheses or brackets belongs to the
    signature (`[u8; 4]`), not to the end of a declaration.
    """
    depth, nest, i, opened = 0, 0, start, False
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
        elif not opened and char in "([":
            nest += 1
        elif not opened and char in ")]":
            nest -= 1
        elif char == ";" and not opened and nest == 0:
            return i
        i += 1
        if opened and depth == 0:
            return i
    return i


def normalise(text):
    """Collapse whitespace outside literals; literals are kept byte for byte."""
    out, pending, i = [], False, 0
    while i < len(text):
        end = _literal_end(text, i)
        if end is None and text[i].isspace():
            pending, i = True, i + 1
            continue
        if pending and out:
            out.append(" ")
        pending = False
        if end is not None:
            out.append(text[i:end])
            i = end
            continue
        end = _comment_end(text, i)
        if end is not None:
            out.append(" ".join(text[i:end].split()))
            i = end
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def fn_bodies(text):
    """Counter of (name, normalised text) for every fn, prefix included.

    The prefix is the run of outer attributes, doc comments, visibility and
    qualifiers directly before `fn`; any other token ends it.
    """
    found = Counter()
    prefix, i = None, 0
    while i < len(text):
        if text[i].isspace():
            i += 1
            continue
        if text.startswith("#[", i) or text.startswith("///", i) or text.startswith("/**", i):
            prefix = i if prefix is None else prefix
            i = _matching(text, i + 1, "[", "]") if text[i] == "#" else _comment_end(text, i)
            continue
        if text.startswith("#![", i):
            prefix, i = None, _matching(text, i + 2, "[", "]")
            continue
        end = _skip(text, i)
        if end != i:
            if text[i] != "/" and not (prefix is not None and text[i] == '"'):
                prefix = None  # a literal, except the ABI string of `extern "C"`
            i = end
            continue
        word = _WORD.match(text, i)
        if word and word.group() == "fn":
            after_fn = word.end()
            while after_fn < len(text) and text[after_fn].isspace():
                after_fn += 1
            name = _WORD.match(text, after_fn)
            if name:
                stop = _body_end(text, name.end())
                start = i if prefix is None else prefix
                found[(name.group(), normalise(text[start:stop]))] += 1
                prefix, i = None, stop
                continue
        if word and word.group() in _QUALIFIERS:
            prefix = i if prefix is None else prefix
            i = word.end()
            rest = text[i:]
            if word.group() == "pub" and rest.lstrip().startswith("("):
                i = _matching(text, i + len(rest) - len(rest.lstrip()), "(", ")")
            continue
        prefix = None
        i = word.end() if word else i + 1
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
    counts = f"fn bodies before={sum(before.values())} after={sum(after.values())}"
    print(counts)
    if not before or not after:
        empty = " and ".join(n for n, s in (("before", before), ("after", after)) if not s)
        print(f"nothing to compare: no function found {empty}; check the refs and paths", file=sys.stderr)
        return 2
    lost, added, changed = compare(before, after)
    print(f"only before: {lost}")
    print(f"only after: {added}")
    print(f"changed: {changed}")
    return 1 if (lost or added or changed) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
