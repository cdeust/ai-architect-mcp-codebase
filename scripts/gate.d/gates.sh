# shellcheck shell=bash disable=SC2034,SC2153
# gate.d/gates.sh - sourced by scripts/gate.sh. The gate commands and their runner.

# Heavy commands run niced and with bounded parallelism.
# source: owner instruction 2026-09-30 (one heavy job at a time; nice 10; 4 jobs/threads).
export CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4

gate_heavy() { nice -n 10 "$@"; }

gate_cmd_fmt() { gate_heavy "${GATE_CARGO:-cargo}" fmt --all -- --check; }
gate_cmd_clippy() { gate_heavy "${GATE_CARGO:-cargo}" clippy --workspace --all-targets -- -D warnings; }
gate_cmd_unit() { gate_heavy "${GATE_CARGO:-cargo}" test --lib -- --test-threads=4; }
gate_cmd_bench() {
  gate_heavy "${GATE_CARGO:-cargo}" run --release -p bench-end-result --bin bench_end_result -- --all
}
gate_cmd_oracle() { gate_heavy "${GATE_ORACLE_CMD:-scripts/oracle/replay.sh}"; }

# Plain `cargo test` (CI's command, not --workspace), log kept, then the doc-truth check on it.
gate_cmd_test() {
  local log="$GATE_TMP/cargo-test.log" rc=0
  gate_heavy "${GATE_CARGO:-cargo}" test >"$log" 2>&1 || rc=$?
  cat "$log"
  [ "$rc" -eq 0 ] || return "$rc"
  gate_heavy "${GATE_PYTHON:-python3}" scripts/check_doc_claims.py --test-log "$log"
}

# Every name `waive` accepts (a typo must not become a silent no-op waiver).
GATE_NAMES="fmt clippy unit test bench oracle tree-stable"

# Runs one gate in its own process group, output to a log. Failure: tail shown, ledger row,
# GATE_FAILED set. A gate that cannot start (missing binary) is a failure, rc 127.
gate_run() { # kind tree name
  local kind=$1 tree=$2 name=$3 log rc=0
  log="$GATE_TMP/$name.log"
  echo "gate $name: running" >&2
  set -m
  (cd "$GATE_TOP" && "gate_cmd_$name") >"$log" 2>&1 &
  GATE_CHILD=$!
  set +m
  wait "$GATE_CHILD" 2>/dev/null || rc=$?
  GATE_CHILD=
  if [ "$rc" -eq 0 ]; then
    echo "gate $name: ok" >&2
    return 0
  fi
  echo "gate $name: FAILED (rc=$rc); last lines:" >&2
  tail -n 20 "$log" | gate_redact >&2
  gate_record_failure "$name" "$kind" "$tree" "$rc" "$log"
  GATE_FAILED="$GATE_FAILED $name"
  return "$rc"
}

# Runs the gates named in $3 in order. fmt/clippy/unit/test all run (the failure list is the
# todo); bench and oracle are skipped once anything failed, they are meaningless on a broken build.
gate_run_all() { # kind tree "names"
  local name
  for name in $3; do
    case "$name" in bench | oracle)
      if [ -n "$GATE_FAILED" ]; then
        echo "gate $name: skipped (earlier failure:$GATE_FAILED)" >&2
        continue
      fi
      ;;
    esac
    gate_run "$1" "$2" "$name"
  done
  [ -z "$GATE_FAILED" ]
}

gate_kill_child() {
  [ -n "${GATE_CHILD:-}" ] || return 0
  kill -TERM -- "-$GATE_CHILD" 2>/dev/null || kill -TERM "$GATE_CHILD" 2>/dev/null
  wait "$GATE_CHILD" 2>/dev/null
  GATE_CHILD=
}

gate_cleanup() {
  gate_kill_child
  gate_lock_release
  [ -n "${GATE_TMP:-}" ] && rm -rf "$GATE_TMP"
  GATE_TMP=
}

gate_on_signal() {
  gate_cleanup
  exit 143
}
