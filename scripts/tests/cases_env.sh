# shellcheck shell=bash disable=SC2034
# cases_env.sh - heavy-job lock, interruption, missing binaries, dirty tree.

# bg_gate <dir> <args...>: starts gate.sh in the background (exec: $! is the gate's own pid).
bg_gate() {
  local dir=$1
  shift
  (cd "$dir" && exec env GATE_CARGO="$FIX/stubs/cargo" GATE_ORACLE_CMD="$FIX/stubs/oracle" \
    GATE_STUB_DIR="$CTL" TMPDIR="$FIX/tmp" bash scripts/gate.sh "$@" >"$FIX/bg.out" 2>&1) &
  BG_PID=$!
}

# wait_for <seconds> <test-command...>: polls until the command succeeds.
wait_for() {
  local n=$(($1 * 10))
  shift
  while [ "$n" -gt 0 ]; do
    "$@" && return 0
    sleep 0.1
    n=$((n - 1))
  done
  return 1
}
has_calls() { [ -s "$CTL/calls" ]; }

t_lock_concurrent() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  echo 3 >"$CTL/slow_fmt"
  bg_gate "$FIX_WT" commit
  wait_for 10 has_calls || bad "first gate started"
  run_gate commit
  assert_eq "second launch while one runs: exit 75" 75 "$RC"
  assert_out "second launch: says who holds the lock" "another heavy gate job"
  wait "$BG_PID"
  assert_eq "first gate still completes: exit 0" 0 "$?"
  assert_nofile "lock released at the end" "$PROOFS/heavy.lock"
  assert_eq "only one gate set ran (3 cargo calls)" 3 "$(calls | grep -c '^cargo ')"
  run_gate commit
  assert_eq "after release a new launch is accepted" 0 "$RC"
}

t_lock_stale_dead_pid() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  mkdir -p "$PROOFS/heavy.lock"
  echo "999999 Thu Jan 1 00:00:00 1970" >"$PROOFS/heavy.lock/owner"
  run_gate commit
  assert_eq "lock of a dead pid is stolen: exit 0" 0 "$RC"
  assert_nofile "stolen lock released" "$PROOFS/heavy.lock"
}

t_lock_recycled_pid() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  mkdir -p "$PROOFS/heavy.lock"
  echo "$$ Mon Jan 1 00:00:00 1990" >"$PROOFS/heavy.lock/owner"
  run_gate commit
  assert_eq "live pid with another start time (recycled) is stale: exit 0" 0 "$RC"
}

t_lock_live_owner() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  sleep 30 &
  local holder=$!
  mkdir -p "$PROOFS/heavy.lock"
  printf '%s %s' "$holder" "$(ps -o lstart= -p "$holder" | tr -s ' ')" >"$PROOFS/heavy.lock/owner"
  run_gate commit
  assert_eq "live owner with matching start time: exit 75" 75 "$RC"
  assert_eq "live owner: its lock is left alone" 1 "$([ -d "$PROOFS/heavy.lock" ] && echo 1 || echo 0)"
  assert_eq "live owner: no cargo" 0 "$(calls | grep -c '^cargo ')"
  kill "$holder" 2>/dev/null
  wait "$holder" 2>/dev/null
}

t_lock_half_made() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  mkdir -p "$PROOFS/heavy.lock"
  run_gate commit
  assert_eq "fresh lock without owner file: treated as busy, exit 75" 75 "$RC"
  touch -t 200001010000 "$PROOFS/heavy.lock"
  run_gate commit
  assert_eq "old lock without owner file: stale, exit 0" 0 "$RC"
}

t_interrupt_leaves_nothing() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  echo 20 >"$CTL/slow_fmt"
  bg_gate "$FIX_WT" commit
  wait_for 10 has_calls || bad "gate started"
  kill -TERM "$BG_PID"
  wait "$BG_PID"
  assert_eq "SIGTERM: exit 143" 143 "$?"
  assert_eq "no proof after interruption" 0 "$(count_named "$PROOFS" 'commit-*')"
  assert_eq "no partial temp file" 0 "$(count_named "$PROOFS" '.tmp-*')"
  assert_nofile "lock released" "$PROOFS/heavy.lock"
  assert_nofile "an interruption is not a gate failure" "$PROOFS/failures.jsonl"
  assert_eq "temp dir removed" 0 "$(count_named "$FIX/tmp" 'gate.*')"
  sleep 0.3
  assert_eq "gate child is gone" 1 "$(kill -0 "$(head -1 "$CTL/pids")" 2>/dev/null && echo 0 || echo 1)"
}

t_missing_python() {
  mkfix
  commit_files "$FIX_WT" src/lib.rs
  GATE_ENV_EXTRA="GATE_PYTHON=/nonexistent/python3" run_gate push
  assert_eq "python3 missing: push fails" 1 "$RC"
  assert_eq "python3 missing: ledger gate is test" test "$(last_failure gate)"
  assert_eq "python3 missing: no proof" 0 "$(count_named "$PROOFS" 'push-*')"
}

t_missing_cargo() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  GATE_ENV_EXTRA="GATE_CARGO=/nonexistent/cargo" run_gate commit
  assert_eq "cargo missing: exit 1" 1 "$RC"
  assert_eq "cargo missing: three ledger rows" 3 "$(wc -l <"$PROOFS/failures.jsonl" | tr -d ' ')"
  assert_eq "cargo missing: no proof" 0 "$(count_named "$PROOFS" 'commit-*')"
}

t_dirty_tree_refused() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  echo "unstaged" >>"$FIX_WT/src/lib.rs"
  run_gate commit
  assert_eq "unstaged change: exit 2" 2 "$RC"
  assert_eq "unstaged change: no cargo" 0 "$(calls | grep -c '^cargo ')"
}

t_tree_stable() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  : >"$CTL/mutate"
  run_gate commit
  assert_eq "a gate that rewrites a tracked file: exit 1" 1 "$RC"
  assert_eq "ledger gate is tree-stable" tree-stable "$(last_failure gate)"
  assert_eq "no proof" 0 "$(count_named "$PROOFS" 'commit-*')"
}

t_not_a_repo() {
  mkfix
  OUT=$(cd "$FIX" && bash "$FIX_WT/scripts/gate.sh" commit 2>&1)
  RC=$?
  assert_eq "outside a git work tree: exit 2" 2 "$RC"
}
