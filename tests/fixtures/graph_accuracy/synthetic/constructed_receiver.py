"""Synthetic fixture: a method call on a local built from a class of the file.

Validates issue #444 for Python: `w = Worker()` then `w.run(3)` must resolve
to `Worker.run`, not stay open because `Other` also owns a `run`. Two classes
own `run` on purpose, so an edge can only come from the receiver's type. The
last function rebinds the local: its call must stay without an edge.
"""

from __future__ import annotations


class Worker:
    def run(self, n: int) -> int:
        return n


class Other:
    def run(self, n: int) -> int:
        return -n


class Pool:
    def drain(self) -> int:
        w = Worker()
        return w.run(1)


def use_worker() -> int:
    w = Worker()
    return w.run(3)


def use_other() -> int:
    o = Other()
    return o.run(3)


def rebound(flag: bool) -> int:
    w = Worker()
    if flag:
        w = Other()
    return w.run(3)
