# shellcheck shell=bash disable=SC2034
# cases_oracle.sh - oracle replay refusals and comparison plumbing. Never builds, never runs the
# real oracle: cargo is the stub and the corpus is fake.

# replay_in <dir> <oracle-dir> [ENV=val...]: runs replay.sh of <oracle-dir> with cwd <dir>.
replay_in() {
  local dir=$1 od=$2
  shift 2
  OUT=$(cd "$dir" && env -u GATE_ORACLE_CORPUS GATE_CARGO="$FIX/stubs/cargo" GATE_STUB_DIR="$CTL" \
    TMPDIR="$FIX/tmp" "$@" bash "$od/replay.sh" 2>&1)
  RC=$?
}

t_oracle_replay_refusals() {
  mkfix
  replay_in "$FIX_MAIN" "$GATE_SRC/oracle" GATE_ORACLE_CORPUS="$FIX"
  assert_eq "replay in the main clone: exit 2" 2 "$RC"
  assert_out "replay in the main clone: message" "live-mounted main clone"
  replay_in "$FIX_WT" "$GATE_SRC/oracle"
  assert_eq "no GATE_ORACLE_CORPUS: exit 2" 2 "$RC"
  assert_out "no GATE_ORACLE_CORPUS: says which variable" "GATE_ORACLE_CORPUS is not set"
  replay_in "$FIX_WT" "$GATE_SRC/oracle" GATE_ORACLE_CORPUS="$FIX/absent"
  assert_eq "corpus dir absent: exit 2" 2 "$RC"
  assert_out "corpus dir absent: message" "corpus directory not found"
  g init -q "$FIX/corpus"
  g -C "$FIX/corpus" commit -q --allow-empty -m x
  replay_in "$FIX_WT" "$GATE_SRC/oracle" GATE_ORACLE_CORPUS="$FIX/corpus"
  assert_eq "corpus at another commit: exit 2" 2 "$RC"
  assert_out "corpus at another commit: message" "corpus is at"
  assert_out "corpus with other bytes: message" "corpus bytes differ from the oracle"
  assert_eq "no cargo was called by any refusal" 0 "$(calls | grep -c '^cargo ')"
}

t_oracle_identity_pins_oracle_bytes() {
  mkfix
  cp -R "$GATE_SRC/oracle" "$FIX/oracle"
  echo " " >>"$FIX/oracle/oracle.json"
  g init -q "$FIX/corpus"
  replay_in "$FIX_WT" "$FIX/oracle" GATE_ORACLE_CORPUS="$FIX/corpus"
  assert_eq "altered oracle.json: exit 2" 2 "$RC"
  assert_out "altered oracle.json: message" "pinned sha256"
  assert_eq "altered oracle.json: no cargo" 0 "$(calls | grep -c '^cargo ')"
}

# compare_run <dir>: runs compare.py on the fixture; sets RC and OUT.
compare_run() {
  OUT=$(python3 "$GATE_SRC/oracle/compare.py" "$1/oracle.json" "$1/expected.json" "$1/run" "$1/sites" 2>&1)
  RC=$?
}

t_oracle_compare() {
  mkfix
  python3 "$TESTS_DIR/oracle_fixture.py" "$GATE_SRC/oracle" "$FIX/of" || bad "fixture built"
  compare_run "$FIX/of"
  assert_eq "compare: identical values: exit 0" 0 "$RC"
  assert_out "compare: zero deviations" "0 deviation(s)"
  sed -i.bak 's/"proof": 1/"proof": 0/' "$FIX/of/expected.json"
  compare_run "$FIX/of"
  assert_eq "compare: a deviating context count: exit 1" 1 "$RC"
  assert_out "compare: names the deviating value" "DEVIATION impact_response_of"
  python3 "$TESTS_DIR/oracle_fixture.py" "$GATE_SRC/oracle" "$FIX/of2" || bad "fixture 2 built"
  sed -i.bak 's/"two_tasks_terminate_context": "proof"/"two_tasks_terminate_context": "production"/' "$FIX/of2/expected.json"
  compare_run "$FIX/of2"
  assert_eq "compare: two_tasks_terminate context deviation: exit 1" 1 "$RC"
  python3 "$TESTS_DIR/oracle_fixture.py" "$GATE_SRC/oracle" "$FIX/of3" || bad "fixture 3 built"
  sed -i.bak 's/"node_count": 7/"node_count": 8/' "$FIX/of3/expected.json"
  compare_run "$FIX/of3"
  assert_eq "compare: node_count deviation: exit 1" 1 "$RC"
  rm -f "$FIX/of3/run/status.json"
  compare_run "$FIX/of3"
  assert_eq "compare: a missing measurement file is a failure" 1 "$RC"
}

t_oracle_default_command_is_replay() {
  mkfix
  assert_eq "scripts/oracle/replay.sh is executable in the repo under test" 1 "$([ -x "$GATE_SRC/oracle/replay.sh" ] && echo 1 || echo 0)"
  mkdir -p "$FIX_WT/scripts/oracle"
  # shellcheck disable=SC2016 # $GATE_STUB_DIR must expand in the written stub, not here
  printf '#!/bin/sh\necho "oracle default" >>"$GATE_STUB_DIR/calls"\n' >"$FIX_WT/scripts/oracle/replay.sh"
  chmod +x "$FIX_WT/scripts/oracle/replay.sh"
  g -C "$FIX_WT" add scripts/oracle/replay.sh
  commit_files "$FIX_WT" src/lib.rs
  GATE_ENV_EXTRA="GATE_ORACLE_CMD=" run_gate push
  assert_eq "push without GATE_ORACLE_CMD: exit 0" 0 "$RC"
  assert_eq "push without GATE_ORACLE_CMD runs scripts/oracle/replay.sh" 1 "$(calls | grep -c '^oracle default')"
}
