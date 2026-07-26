#!/usr/bin/env bash
# Reproduce the issue #64 head-to-head evaluation from a clean checkout.
#
# Third-party runnable (§AC5): no network, no API key, no external corpus. The
# committed corpus under corpus/ is the pin; its SHA-256 is printed and recorded
# in MANIFEST.md. Regenerates results.json + raw_results.json.
#
# Optional answer-quality judge leg (§7): export AP_EVAL_JUDGE_CMD to a shell
# command that reads a JSON judging request on stdin and writes
# {"score_a":N,"score_b":M} (0-4) on stdout. Unset ⇒ the leg is skipped and
# reported loudly; the deterministic legs still produce the published numbers.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

echo "== issue #64 head-to-head evaluation =="
echo "toolchain: $(rustc --version)"
if [[ -n "${AP_EVAL_JUDGE_CMD:-}" ]]; then
  echo "judge leg: ENABLED (AP_EVAL_JUDGE_CMD set)"
else
  echo "judge leg: DISABLED (AP_EVAL_JUDGE_CMD unset) — deterministic legs only"
fi

cd "$repo_root"
cargo run -p eval-headtohead-bench --release

echo
echo "wrote:"
echo "  $here/results.json"
echo "  $here/raw_results.json"
echo "see $here/MANIFEST.md for provenance and the honest H4 negative."
