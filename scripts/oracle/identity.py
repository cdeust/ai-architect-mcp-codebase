#!/usr/bin/env python3
"""Identity gate of the oracle replay: refuses (exit 1, reason on stderr) unless
  - oracle.json has the sha256 pinned in expected.json,
  - the corpus checkout is at the pinned commit and every oracle file has the pinned bytes.
usage: identity.py ORACLE_JSON EXPECTED_JSON CORPUS_DIR"""
import hashlib
import json
import pathlib
import subprocess
import sys


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def problems(oracle_path: pathlib.Path, expected: dict, corpus: pathlib.Path) -> list:
    found = []
    if sha256(oracle_path) != expected["corpus"]["oracle_sha256"]:
        found.append(f"{oracle_path} does not have the pinned sha256")
    if not corpus.is_dir():
        return found + [f"corpus directory not found: {corpus}"]
    try:
        head = subprocess.check_output(["git", "-C", str(corpus), "rev-parse", "HEAD"],
                                       text=True, stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.CalledProcessError):
        return found + [f"{corpus} is not a git checkout"]
    if head != expected["corpus"]["commit"]:
        found.append(f"corpus is at {head}, expected {expected['corpus']['commit']}")
    for item in json.loads(oracle_path.read_text())["corpus"]["files"]:
        target = corpus / item["file"]
        if not target.is_file() or sha256(target) != item["sha256"]:
            found.append(f"corpus bytes differ from the oracle: {item['file']}")
    return found


def main() -> int:
    oracle_path, expected_path, corpus = (pathlib.Path(a) for a in sys.argv[1:4])
    expected = json.loads(expected_path.read_text())
    bad = problems(oracle_path, expected, corpus)
    for line in bad:
        print("oracle identity: " + line, file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
