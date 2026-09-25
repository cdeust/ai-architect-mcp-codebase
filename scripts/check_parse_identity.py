#!/usr/bin/env python3
"""Proves that the Rust parse output of this tree equals the one of origin/main.

A file without `#[cfg]` twins must parse to exactly the same nodes and refs as
before the twin identity of issue #353. This script fingerprints every Rust file
of `src/`, `tests/` and `crates/` of THIS tree with the parser of a temporary
worktree of origin/main and with the parser of this tree, and diffs the two. A
control corpus (one file with twins, one without) must differ on the twin file
only, so an empty diff is not an empty measurement.

    python3 scripts/check_parse_identity.py

Exit 0 when the diff is empty and the control differs exactly as expected.
"""
import difflib
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
DUMPER = ROOT / "scripts" / "parse_digest.rs"
CONTROL = {
    "src/twins.rs": "#[cfg(unix)]\nfn f() {}\n#[cfg(not(unix))]\nfn f() {}\n",
    "src/plain.rs": "fn g() {}\nstruct S;\nimpl S {\n    fn m(&self) {}\n}\n",
}


def run(cmd, cwd, env=None):
    subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})}, check=True)


def fingerprint(tree: pathlib.Path, corpus: pathlib.Path, out: pathlib.Path, target: pathlib.Path):
    """Runs the dumper built inside `tree` over `corpus`."""
    examples = tree / "examples"
    examples.mkdir(exist_ok=True)
    shutil.copy(DUMPER, examples / "parse_digest.rs")
    try:
        run(
            ["cargo", "run", "--quiet", "--example", "parse_digest"],
            tree,
            {"CORPUS": str(corpus), "OUT": str(out), "CARGO_TARGET_DIR": str(target),
             "CARGO_BUILD_JOBS": "4"},
        )
    finally:
        (examples / "parse_digest.rs").unlink(missing_ok=True)
        if not any(examples.iterdir()):
            examples.rmdir()
    return out.read_text().splitlines()


def main() -> int:
    target = ROOT / "target"
    main_tree = ROOT / ".claude" / "worktrees" / f"parse-identity-{os.getpid()}"
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="parse-identity-"))
    subprocess.run(["git", "fetch", "-q", "origin", "main"], cwd=ROOT, check=True)
    run(["git", "worktree", "add", "--detach", str(main_tree), "origin/main"], ROOT)
    try:
        for name, text in CONTROL.items():
            path = scratch / "control" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        results = {}
        for label, tree in (("main", main_tree), ("head", ROOT)):
            results[label] = fingerprint(tree, ROOT, scratch / f"{label}.txt", target)
            results[label + "-control"] = fingerprint(
                tree, scratch / "control", scratch / f"{label}-control.txt", target)
    finally:
        run(["git", "worktree", "remove", "--force", str(main_tree)], ROOT)
        shutil.rmtree(scratch, ignore_errors=True)

    diff = list(difflib.unified_diff(results["main"], results["head"], "main", "head", lineterm=""))
    print(f"files fingerprinted: {len(results['main'])} (main) / {len(results['head'])} (head)")
    print("diff main vs head:", "EMPTY" if not diff else f"{len(diff)} lines")
    print("\n".join(diff[:40]))
    changed = [
        m.split(" ")[0]
        for m, h in zip(results["main-control"], results["head-control"])
        if m != h
    ]
    print("control (files whose fingerprint differs):", changed)
    ok = not diff and changed == ["src/twins.rs"]
    print("OK" if ok else "FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
