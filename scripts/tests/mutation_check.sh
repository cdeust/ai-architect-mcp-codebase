#!/usr/bin/env bash
# mutation_check.sh - proves test_gate.sh KILLS mutants of scripts/gate*: each mutation breaks one
# key behavior in a scratch copy; the matching tests must fail (mutant killed). A surviving mutant
# or a mutation that does not apply exactly once makes this script exit 1.
# Env: GATE_TEST_WORKDIR (scratch dir; default mktemp) + the variables of test_gate.sh.
# shellcheck disable=SC2016 # the mutated texts are literal shell source, never expanded
set -u
TESTS_DIR=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
SRC=$(cd -P "$TESTS_DIR/.." && pwd -P)
WORK=${GATE_TEST_WORKDIR:-}
OWN=0
if [ -z "$WORK" ]; then WORK=$(mktemp -d "${TMPDIR:-/tmp}/gate-mut.XXXXXX") || exit 2; OWN=1; fi
trap '[ "$OWN" -eq 0 ] || rm -rf "$WORK"' EXIT
KILLED=0
SURVIVED=0
N=0

# apply <file> <old> <new> <dir>: replaces the exact text <old> once; rc 1 unless it occurs once.
apply() {
  python3 - "$4/$1" "$2" "$3" <<'PY'
import sys
path, old, new = sys.argv[1:4]
text = open(path, encoding="utf-8").read()
if text.count(old) != 1:
    sys.exit(f"mutation does not apply exactly once in {path}: found {text.count(old)}")
open(path, "w", encoding="utf-8").write(text.replace(old, new))
PY
}

# mutant <name> <file> <old> <new> <test-regex>
mutant() {
  local name=$1 dir log
  N=$((N + 1))
  dir=$WORK/mut-$N
  log=$WORK/mut-$N.log
  cp -R "$SRC" "$dir" || exit 2
  if ! apply "$2" "$3" "$4" "$dir"; then
    echo "INVALID  $name"
    SURVIVED=$((SURVIVED + 1))
    return
  fi
  if GATE_SRC=$dir GATE_TEST_ONLY=$5 bash "$TESTS_DIR/test_gate.sh" >"$log" 2>&1; then
    echo "SURVIVED $name   (tests /$5/ still pass)"
    SURVIVED=$((SURVIVED + 1))
  else
    echo "KILLED   $name   ($(grep -c '^  FAIL' "$log") failing assertion(s); e.g. $(grep -m1 '^  FAIL' "$log" | cut -c1-110))"
    KILLED=$((KILLED + 1))
  fi
  rm -rf "$dir"
}

mutant "1 failure still writes a proof" gate.d/commands.sh \
  'if ! gate_run_all "$kind" "$tree" "$GATE_PLAN" || ! gate_check_stable "$kind" "$tree"; then' \
  'gate_run_all "$kind" "$tree" "$GATE_PLAN"; gate_check_stable "$kind" "$tree"; if false; then' \
  't_fail_'
mutant "2 proof tree is HEAD tree, not write-tree" gate.d/common.sh \
  't=$(git -C "$GATE_TOP" write-tree 2>/dev/null)' \
  "t=\$(git -C \"\$GATE_TOP\" rev-parse 'HEAD^{tree}' 2>/dev/null)" \
  't_commit_green|t_verify'
mutant "3 main-clone refusal removed" gate.d/guard.sh \
  '[ "$gitdir" = "$common" ]' 'false' 't_main_clone'
mutant "4 waive without --reason accepted" gate.d/commands.sh \
  'if [ "$have" -eq 0 ] || [ -z "$n" ] || [ -z "$gate" ]; then' 'if [ -z "$gate" ]; then' 't_waive'
mutant "5 docs-only short-circuits everything (.rs included)" gate.d/guard.sh \
  '    gate_is_doc_path "$f" || return 1' '    :' 't_docs_only'
mutant "6 docs-only ignores the .rs extension" gate.d/guard.sh \
  '*.rs | Cargo.toml | */Cargo.toml | Cargo.lock | */Cargo.lock) return 1 ;;' \
  'Cargo.toml | */Cargo.toml | Cargo.lock | */Cargo.lock) return 1 ;;' 't_docs_only_negative'
mutant "7 oracle runs on every push" gate.d/commands.sh \
  'if gate_changed push | gate_any_src_rs; then names="$names oracle"; fi' 'names="$names oracle"' 't_oracle_trigger'
mutant "8 a dead owner never makes the lock stale" gate.d/guard.sh \
  '[ "$(gate_lock_owner_stamp "$pid")" != "$owner" ]' 'false' 't_lock_stale|t_lock_recycled'
mutant "9 verify accepts a proof with rc != 0" gate.d/common.sh \
  'if isinstance(rc, bool) or rc != 0 or data.get("tree") != sys.argv[2]:' \
  'if data.get("tree") != sys.argv[2]:' 't_verify'
mutant "10 shortcut for clones under .claude/worktrees removed" gate.d/guard.sh \
  '  case "$1" in */.claude/worktrees/*) return 1 ;; esac
' '' 't_clone_under_worktrees'
mutant "11 push dirty check compares the index, not HEAD" gate.d/guard.sh \
  'git -C "$GATE_TOP" diff --quiet HEAD' 'git -C "$GATE_TOP" diff --quiet' 't_push_refuses_staged'
mutant "12 push reuses a docs-only commit proof" gate.d/commands.sh \
  'if cmd=$(gate_proof_valid commit "$2") && [ "$cmd" != docs-only ]; then' \
  'if cmd=$(gate_proof_valid commit "$2"); then' 't_push_does_not_reuse'
mutant "13 --no-renames dropped from the commit diff" gate.d/guard.sh \
  'git -C "$GATE_TOP" diff --cached --name-only -z --no-renames' \
  'git -C "$GATE_TOP" diff --cached --name-only -z' 't_rename_to_docs'
mutant "14 sk- tokens not redacted" gate.d/common.sh \
  "'s/(gh[pousr]_|github_pat_|sk-)[A-Za-z0-9_-]{8,}/[REDACTED]/g'" \
  "'s/(gh[pousr]_|github_pat_)[A-Za-z0-9_-]{8,}/[REDACTED]/g'" 't_failure_excerpt_redacts'

echo "mutants killed: $KILLED / $N   survived or invalid: $SURVIVED"
[ "$SURVIVED" -eq 0 ]
