# shellcheck shell=bash disable=SC2034
# lib.sh - assertions and fixture builders for test_gate.sh (bash 3.2 compatible).
# A fixture is a fake "main clone" (a real git repo) with a real worktree under
# .claude/worktrees/, a fake origin/main ref, copies of gate.sh, gate.d and the stubs.

PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1${2:+ -- $2}"; }

assert_eq() { # desc expected actual
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "expected [$2] got [$3]"; fi
}
assert_ne() { # desc unexpected actual
  if [ "$2" != "$3" ]; then ok "$1"; else bad "$1" "got unexpected [$3]"; fi
}
assert_file() { if [ -f "$2" ]; then ok "$1"; else bad "$1" "missing $2"; fi; }
assert_nofile() { if [ ! -e "$2" ]; then ok "$1"; else bad "$1" "unexpected $2"; fi; }
assert_grep() { # desc pattern file-or-text-var (file)
  if grep -q -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1" "no /$2/ in $3"; fi
}
assert_out() { # desc pattern  (searches $OUT)
  case "$OUT" in *"$2"*) ok "$1" ;; *) bad "$1" "no [$2] in output: $(echo "$OUT" | tail -3)" ;; esac
}

# git for fixtures only: no user hooks, no signing, fixed identity.
g() { git -c core.hooksPath=/dev/null -c commit.gpgsign=false -c user.name=t -c user.email=t@t.invalid "$@"; }

# mkfix: builds $FIX/main (+ worktree $FIX/main/.claude/worktrees/wt on branch feat).
# Sets FIX FIX_MAIN FIX_WT CTL PROOFS. origin/main = the initial commit.
mkfix() {
  FIX=$(mktemp -d "$WORK/fix.XXXXXX")
  FIXES="${FIXES:-} $FIX"
  FIX_MAIN=$FIX/main
  CTL=$FIX/ctl
  mkdir -p "$CTL" "$FIX/tmp" "$FIX/stubs" "$FIX_MAIN/scripts" "$FIX_MAIN/src" "$FIX_MAIN/docs"
  cp -p "$TESTS_DIR"/stubs/* "$FIX/stubs/"
  cp -p "$GATE_SRC/gate.sh" "$FIX_MAIN/scripts/gate.sh"
  cp -Rp "$GATE_SRC/gate.d" "$FIX_MAIN/scripts/gate.d"
  cp -p "$TESTS_DIR/stubs/check_doc_claims.py" "$FIX_MAIN/scripts/check_doc_claims.py"
  echo "pub fn a() {}" >"$FIX_MAIN/src/lib.rs"
  echo "# readme" >"$FIX_MAIN/README.md"
  echo "guide" >"$FIX_MAIN/docs/guide.md"
  printf '[package]\nname = "x"\n' >"$FIX_MAIN/Cargo.toml"
  echo "# lock" >"$FIX_MAIN/Cargo.lock"
  g init -q "$FIX_MAIN"
  g -C "$FIX_MAIN" add -A
  g -C "$FIX_MAIN" commit -q -m init
  g -C "$FIX_MAIN" update-ref refs/remotes/origin/main HEAD
  g -C "$FIX_MAIN" worktree add -q .claude/worktrees/wt -b feat
  FIX_WT=$FIX_MAIN/.claude/worktrees/wt
  PROOFS=$FIX_MAIN/.git/zetetic-gates
}

# stage <dir> <file> [content]: writes and stages one file (creating parents).
stage() {
  mkdir -p "$(dirname "$1/$2")"
  printf '%s\n' "${3:-change $RANDOM}" >"$1/$2"
  g -C "$1" add "$2"
}

# commit_files <dir> <file>...: stages each file and commits them on the current branch.
commit_files() {
  local d=$1 f
  shift
  for f in "$@"; do stage "$d" "$f"; done
  g -C "$d" commit -q -m "change $*"
}

# run_gate_in <dir> <args...>: runs the fixture's gate.sh with the stubs; sets RC and OUT.
# Extra env can be passed through the variable GATE_ENV_EXTRA (space separated NAME=value).
run_gate_in() {
  local dir=$1
  shift
  # shellcheck disable=SC2086 # GATE_ENV_EXTRA is deliberately word-split into NAME=value words
  OUT=$(cd "$dir" && env GATE_CARGO="$FIX/stubs/cargo" GATE_ORACLE_CMD="$FIX/stubs/oracle" \
    GATE_STUB_DIR="$CTL" TMPDIR="$FIX/tmp" ${GATE_ENV_EXTRA:-} bash scripts/gate.sh "$@" 2>&1)
  RC=$?
}
run_gate() { run_gate_in "$FIX_WT" "$@"; }

calls() { cat "$CTL/calls" 2>/dev/null || true; }
clear_calls() { rm -f "$CTL/calls"; }
count_calls() { calls | grep -c -- "$1" || true; }

proof_field() { # file field
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}
wt_tree() { g -C "$FIX_WT" write-tree; }
head_tree() { g -C "$FIX_WT" rev-parse 'HEAD^{tree}'; }

# count_named <dir> <glob>: number of entries of <dir> matching <glob> (dotfiles included).
count_named() { find "$1" -maxdepth 1 -name "$2" 2>/dev/null | wc -l | tr -d " "; }
