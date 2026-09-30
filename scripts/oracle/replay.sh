#!/usr/bin/env bash
# replay.sh - replays the dy-wcet oracle (static pass) against THIS worktree's release binary and
# compares it with expected.json. Called by `gate.sh push` when the diff touches src/**/*.rs.
# Local only, never in CI. Env: GATE_ORACLE_CORPUS (required: checkout of DYResearch/dy-wcet at the
# commit pinned in expected.json), GATE_CARGO (default cargo), GATE_PYTHON (default python3).
# Exit: 0 identical; 1 deviation/measure failure; 2 refusal (no corpus, wrong corpus/oracle bytes,
# live-mounted main clone). Nothing is built before every refusal has been checked.
set -u
HERE=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
TOP=$(git rev-parse --show-toplevel) || { echo "replay.sh: not in a git work tree" >&2; exit 2; }
PY=${GATE_PYTHON:-python3}
BIN_NAME=ai-architect-mcp-codebase # Cargo.toml [[bin]] name

gitdir=$(cd -P "$(git rev-parse --absolute-git-dir)" && pwd -P)
common=$(cd -P "$(git rev-parse --git-common-dir)" && pwd -P)
case "$TOP" in */.claude/worktrees/*) ;; *)
  if [ "$gitdir" = "$common" ]; then
    echo "replay.sh: refusing to build in the live-mounted main clone ($TOP); use a worktree." >&2
    exit 2
  fi
  ;;
esac

corpus=${GATE_ORACLE_CORPUS:-}
if [ -z "$corpus" ]; then
  echo "replay.sh: GATE_ORACLE_CORPUS is not set. Point it at a checkout of DYResearch/dy-wcet v4.1.6" >&2
  echo "  (commit $("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["corpus"]["commit"])' "$HERE/expected.json"))." >&2
  exit 2
fi
"$PY" "$HERE/identity.py" "$HERE/oracle.json" "$HERE/expected.json" "$corpus" || exit 2

out=$(mktemp -d "${TMPDIR:-/tmp}/oracle-replay.XXXXXX") || exit 2
trap 'rm -rf "$out"' EXIT
(cd "$TOP" && "${GATE_CARGO:-cargo}" build --release --bin "$BIN_NAME") || exit 1
bin=$TOP/target/release/$BIN_NAME
"$PY" "$HERE/measure.py" "$bin" "$corpus" "$out/run" >/dev/null || exit 1
(cd "$HERE" && "$PY" sitecollect.py "$bin" "$out/run/graph" "$out/sites") >/dev/null || exit 1
"$PY" "$HERE/compare.py" "$HERE/oracle.json" "$HERE/expected.json" "$out/run" "$out/sites"
