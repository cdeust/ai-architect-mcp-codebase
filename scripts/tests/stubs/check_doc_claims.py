#!/usr/bin/env python3
"""Fake scripts/check_doc_claims.py for test_gate.sh: fails when $GATE_STUB_DIR/fail_docs exists."""
import os
import sys

d = os.environ["GATE_STUB_DIR"]
with open(os.path.join(d, "calls"), "a", encoding="utf-8") as fh:
    fh.write("check_doc_claims " + " ".join(sys.argv[1:]) + "\n")
if os.path.exists(os.path.join(d, "fail_docs")):
    print("doc claims deviation: marker-docs")
    sys.exit(1)
