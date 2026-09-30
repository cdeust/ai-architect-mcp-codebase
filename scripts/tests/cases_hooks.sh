# shellcheck shell=bash disable=SC2034
# cases_hooks.sh - end to end with the REAL hooks (gate-proof.py, stop-gate-ledger.py).
# Missing hooks are a failure, not a skip, unless GATE_TEST_SKIP_HOOKS=1 says so explicitly.

hooks_present() {
  if [ -f "$HOOKS/gate-proof.py" ] && [ -f "$HOOKS/stop-gate-ledger.py" ]; then return 0; fi
  if [ "${GATE_TEST_SKIP_HOOKS:-}" = 1 ]; then
    echo "  skip hooks not found in $HOOKS (GATE_TEST_SKIP_HOOKS=1)"
  else
    bad "hooks present in $HOOKS" "set GATE_TEST_HOOKS or GATE_TEST_SKIP_HOOKS=1"
  fi
  return 1
}

# hook <script> <json>: runs the real hook on a payload; sets RC and OUT (stdout+stderr).
hook() {
  OUT=$(printf '%s' "$2" | python3 -B "$HOOKS/$1" 2>&1)
  RC=$?
}
bash_payload() { # command
  python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","cwd":sys.argv[1],"tool_input":{"command":sys.argv[2]}}))' "$FIX_WT" "$1"
}
stop_payload() {
  python3 -c 'import json,sys; print(json.dumps({"cwd":sys.argv[1],"stop_hook_active":False}))' "$FIX_WT"
}

t_hook_gate_proof_e2e() {
  hooks_present || return 0
  mkfix
  stage "$FIX_WT" src/lib.rs
  hook gate-proof.py "$(bash_payload 'git commit -m x')"
  assert_eq "real gate-proof hook, no proof: exit 2" 2 "$RC"
  assert_out "hook names the missing gate" "GATE-PROOF"
  run_gate commit
  assert_eq "gate.sh commit with green stubs" 0 "$RC"
  hook gate-proof.py "$(bash_payload 'git commit -m x')"
  assert_eq "real gate-proof hook, after gate.sh commit: exit 0" 0 "$RC"
  stage "$FIX_WT" src/lib.rs
  hook gate-proof.py "$(bash_payload 'git commit -m x')"
  assert_eq "hook blocks again once the index tree changed" 2 "$RC"
}

t_hook_gate_proof_push_e2e() {
  hooks_present || return 0
  mkfix
  commit_files "$FIX_WT" src/lib.rs
  hook gate-proof.py "$(bash_payload 'git push origin feat')"
  assert_eq "real hook, push without proof: exit 2" 2 "$RC"
  run_gate push
  assert_eq "gate.sh push with green stubs" 0 "$RC"
  hook gate-proof.py "$(bash_payload 'git push origin feat')"
  assert_eq "real hook, push after gate.sh push: exit 0" 0 "$RC"
}

t_hook_docs_only_e2e() {
  hooks_present || return 0
  mkfix
  stage "$FIX_WT" README.md
  run_gate commit
  hook gate-proof.py "$(bash_payload 'git commit -m docs')"
  assert_eq "real hook accepts a docs-only proof: exit 0" 0 "$RC"
}

t_hook_stop_ledger_e2e() {
  hooks_present || return 0
  mkfix
  stage "$FIX_WT" src/lib.rs
  : >"$CTL/fail_fmt"
  run_gate commit
  assert_eq "failing gate" 1 "$RC"
  hook stop-gate-ledger.py "$(stop_payload)"
  assert_eq "stop hook exits 0 (it blocks via stdout)" 0 "$RC"
  assert_out "real stop hook blocks after a failure" '"decision": "block"'
  assert_out "the block names the failing gate" "fmt"
  rm -f "$CTL/fail_fmt"
  run_gate commit
  assert_eq "green rerun" 0 "$RC"
  hook stop-gate-ledger.py "$(stop_payload)"
  assert_eq "stop hook silent after a later green proof" "" "$OUT"
  stage "$FIX_WT" src/lib.rs
  : >"$CTL/fail_clippy"
  run_gate commit
  hook stop-gate-ledger.py "$(stop_payload)"
  assert_out "a new failure blocks again" '"decision": "block"'
  run_gate waive clippy --reason "test waiver"
  hook stop-gate-ledger.py "$(stop_payload)"
  assert_eq "an auditable waiver lifts the block" "" "$OUT"
}
