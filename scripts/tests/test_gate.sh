#!/usr/bin/env bash
# test_gate.sh - proves scripts/gate.sh by running the real script in throwaway git repos.
# Needs bash, git, python3; NOT cargo (cargo and the oracle are stubs, see stubs/).
# Env: GATE_TEST_WORKDIR (scratch dir; default mktemp), GATE_TEST_ONLY (regex on test names),
#      GATE_SRC (scripts dir under test; mutation_check.sh points it at a mutated copy),
#      GATE_TEST_HOOKS (default ~/.claude/hooks), GATE_TEST_SKIP_HOOKS=1 (explicit skip).
# shellcheck disable=SC2034
set -u

TESTS_DIR=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
GATE_SRC=${GATE_SRC:-$(cd -P "$TESTS_DIR/.." && pwd -P)}
HOOKS=${GATE_TEST_HOOKS:-$HOME/.claude/hooks}
WORK=${GATE_TEST_WORKDIR:-}
OWN_WORK=0
if [ -z "$WORK" ]; then
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/gate-test.XXXXXX") || exit 2
  OWN_WORK=1
fi
FIXES=

finish() {
  local f
  for f in $FIXES; do rm -rf "$f"; done
  [ "$OWN_WORK" -eq 0 ] || rm -rf "$WORK"
}
trap finish EXIT

for f in lib cases_commit cases_guard cases_env cases_oracle cases_hooks; do
  # shellcheck source=/dev/null
  . "$TESTS_DIR/$f.sh"
done

for t in $(compgen -A function | grep '^t_' | grep -E "${GATE_TEST_ONLY:-.}"); do
  echo "$t"
  "$t"
done
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ] && [ "$PASS" -gt 0 ]
