# shellcheck shell=bash disable=SC2034
# cases_commit.sh - commit/push proofs, failures, reuse, waive, verify.

t_commit_green() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  local tree f
  tree=$(wt_tree)
  f=$PROOFS/commit-$tree.json
  run_gate commit
  assert_eq "commit green: exit 0" 0 "$RC"
  assert_file "commit green: proof named by the write-tree" "$f"
  assert_eq "proof.tree == git write-tree" "$tree" "$(proof_field "$f" tree)"
  assert_eq "proof.rc == 0" 0 "$(proof_field "$f" rc)"
  assert_eq "proof.head == HEAD" "$(g -C "$FIX_WT" rev-parse HEAD)" "$(proof_field "$f" head)"
  assert_eq "proof.cargo_lock_sha256" "$(shasum -a 256 <"$FIX_WT/Cargo.lock" | cut -d' ' -f1)" "$(proof_field "$f" cargo_lock_sha256)"
  assert_eq "proof.cmd names the gates" "gate.sh commit [fmt clippy unit]" "$(proof_field "$f" cmd)"
  assert_eq "commit gates: fmt clippy unit ran once each" "3" "$(calls | grep -c '^cargo ')"
  assert_grep "fmt flags" '^cargo fmt --all -- --check' "$CTL/calls"
  assert_grep "clippy flags" '^cargo clippy --workspace --all-targets -- -D warnings' "$CTL/calls"
  assert_grep "unit flags" '^cargo test --lib -- --test-threads=4' "$CTL/calls"
  assert_grep "CARGO_BUILD_JOBS and RUST_TEST_THREADS are 4" 'jobs=4 threads=4' "$CTL/calls"
  assert_eq "no plain cargo test on commit" 0 "$(calls | grep -c 'cargo test \[')"
  assert_eq "gates run niced (>= 10)" 1 "$(calls | sed -n 's/.*nice=\([0-9]*\)].*/\1/p' | sort -n | head -1 | awk '{print ($1>=10)}')"
  assert_nofile "no failure ledger on green" "$PROOFS/failures.jsonl"
  assert_eq "no temp file left" 0 "$(count_named "$PROOFS" '.tmp-*')"
  assert_eq "started <= finished" 1 "$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(int(float(d["started"])<=float(d["finished"])))' "$f")"
}

# last_failure <field>: field of the last row of failures.jsonl.
last_failure() {
  python3 -c 'import json,sys; rows=[json.loads(l) for l in open(sys.argv[1])]; print(rows[-1][sys.argv[2]])' \
    "$PROOFS/failures.jsonl" "$1"
}

# fail_case <ctl-name> <kind> <gate-reported>: one failing gate, exactly that one.
fail_case() {
  local ctl=$1 kind=$2 gate=$3 tree
  mkfix
  if [ "$kind" = push ]; then
    commit_files "$FIX_WT" src/lib.rs
    tree=$(head_tree)
  else
    stage "$FIX_WT" src/lib.rs
    tree=$(wt_tree)
  fi
  : >"$CTL/fail_$ctl"
  run_gate "$kind"
  assert_eq "fail $ctl: exit 1" 1 "$RC"
  assert_nofile "fail $ctl: NO $kind proof" "$PROOFS/$kind-$tree.json"
  assert_eq "fail $ctl: ledger gate" "$gate" "$(last_failure gate)"
  assert_eq "fail $ctl: ledger kind" "$kind" "$(last_failure kind)"
  assert_eq "fail $ctl: ledger tree" "$tree" "$(last_failure tree)"
  assert_eq "fail $ctl: ledger rc" 1 "$(last_failure rc)"
  assert_eq "fail $ctl: excerpt <= 400 chars" 1 "$(python3 -c 'import json,sys; r=[json.loads(l) for l in open(sys.argv[1])][-1]; print(int(len(r["excerpt"])<=400 and "marker-" in r["excerpt"]))' "$PROOFS/failures.jsonl")"
  assert_out "fail $ctl: last lines shown" "marker-"
}

t_fail_fmt() { fail_case fmt commit fmt; }
t_fail_clippy() { fail_case clippy commit clippy; }
t_fail_unit() { fail_case unit commit unit; }
t_fail_test() { fail_case test push test; }
t_fail_docs_claims() { fail_case docs push test; }
t_fail_bench() { fail_case bench push bench; }
t_fail_oracle() { fail_case oracle push oracle; }

t_fail_lists_all_and_skips_heavy() {
  mkfix
  commit_files "$FIX_WT" src/lib.rs
  : >"$CTL/fail_fmt"
  : >"$CTL/fail_unit"
  run_gate push
  assert_eq "two failing gates: two ledger rows" 2 "$(wc -l <"$PROOFS/failures.jsonl" | tr -d ' ')"
  assert_eq "bench skipped after a failure" 0 "$(calls | grep -c 'cargo run')"
  assert_eq "oracle skipped after a failure" 0 "$(calls | grep -c '^oracle')"
  assert_out "skip is announced" "skipped"
}

t_failure_removes_same_tree_proof() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  local tree
  tree=$(wt_tree)
  run_gate commit
  assert_file "green proof exists" "$PROOFS/commit-$tree.json"
  : >"$CTL/fail_fmt"
  run_gate commit
  assert_eq "failing rerun: exit 1" 1 "$RC"
  assert_nofile "failing rerun withdraws the stale proof of that tree" "$PROOFS/commit-$tree.json"
}

t_push_reuses_commit_proof() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  run_gate commit
  assert_eq "commit green" 0 "$RC"
  g -C "$FIX_WT" commit -q -m c1
  clear_calls
  run_gate push
  assert_eq "push green" 0 "$RC"
  assert_eq "push reuses: no fmt rerun" 0 "$(calls | grep -c 'cargo fmt')"
  assert_eq "push reuses: no clippy rerun" 0 "$(calls | grep -c 'cargo clippy')"
  assert_eq "push reuses: no unit rerun" 0 "$(calls | grep -c 'test --lib')"
  assert_eq "push runs plain cargo test once" 1 "$(calls | grep -c '^cargo test \[')"
  assert_eq "push runs the bench" 1 "$(calls | grep -c '^cargo run --release -p bench-end-result --bin bench_end_result -- --all')"
  assert_eq "push runs check_doc_claims on the log" 1 "$(calls | grep -c '^check_doc_claims --test-log ')"
  assert_out "reuse is announced" "commit proof reused"
  assert_grep "push proof cmd says commit-proof" 'commit-proof' "$PROOFS/push-$(head_tree).json"
  commit_files "$FIX_WT" src/lib.rs
  clear_calls
  run_gate push
  assert_eq "other tree: push green" 0 "$RC"
  assert_eq "other tree (no commit proof): fmt rerun" 1 "$(calls | grep -c 'cargo fmt')"
  assert_eq "other tree: unit rerun" 1 "$(calls | grep -c 'test --lib')"
}

# A docs-only commit proof was granted on one diff; it must not stand in for fmt/clippy/unit.
t_push_does_not_reuse_docs_only_proof() {
  mkfix
  commit_files "$FIX_WT" src/lib.rs
  stage "$FIX_WT" CHANGELOG.md
  run_gate commit
  assert_eq "docs-only commit proof" docs-only "$(proof_field "$PROOFS/commit-$(wt_tree).json" cmd)"
  g -C "$FIX_WT" commit -q -m docs
  run_gate push
  assert_eq "push green" 0 "$RC"
  assert_eq "push reran fmt (docs-only proof not reused)" 1 "$(calls | grep -c 'cargo fmt')"
  assert_eq "push reran unit" 1 "$(calls | grep -c 'test --lib')"
  assert_eq "push proof is not a commit-proof reuse" 0 "$(proof_field "$PROOFS/push-$(head_tree).json" cmd | grep -c 'commit-proof')"
}

# Every secret shape the ledger excerpt must mask, leaked through a failing gate's output.
t_failure_excerpt_redacts_secrets() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  printf '%s\n' 'k1 sk-abcdefgh12345678' 'k2 ghp_abcdefgh12345678' 'k3 github_pat_abcdefgh12345678' \
    'k4 AKIAABCDEFGHIJKLMNOP' 'k5 Bearer abcdefgh12345678' 'k6 MY_API_KEY=hunter2hunter2' >"$CTL/leak"
  : >"$CTL/fail_fmt"
  run_gate commit
  local s
  for s in sk-abcdefgh ghp_abcdefgh github_pat_abcdefgh AKIAABCDEFGH 'Bearer abcdefgh' hunter2; do
    assert_eq "ledger excerpt does not contain $s" 0 "$(grep -c -- "$s" "$PROOFS/failures.jsonl")"
  done
  assert_grep "excerpt keeps the masked marker" 'REDACTED' "$PROOFS/failures.jsonl"
}

t_waive() {
  mkfix
  run_gate waive clippy
  assert_eq "waive without --reason: exit 2" 2 "$RC"
  run_gate waive clippy --reason ""
  assert_eq "waive with empty reason: exit 2" 2 "$RC"
  run_gate waive clippy --reason "   "
  assert_eq "waive with blank reason: exit 2" 2 "$RC"
  run_gate waive --reason "why"
  assert_eq "waive without gate: exit 2" 2 "$RC"
  run_gate waive clipyp --reason "typo"
  assert_eq "waive of an unknown gate: exit 2" 2 "$RC"
  assert_nofile "no line written by the refused waivers" "$PROOFS/failures.jsonl"
  run_gate waive clippy --reason 'flaky "rustc" bump, see #1'
  assert_eq "waive with reason: exit 0" 0 "$RC"
  assert_eq "waiver line" 1 "$(python3 -c 'import json,sys; rows=[json.loads(l) for l in open(sys.argv[1])]; r=rows[0]; print(int(len(rows)==1 and r["waive"]=="clippy" and r["reason"]=="flaky \"rustc\" bump, see #1" and float(r["time"])>0 and set(r)=={"waive","reason","time"}))' "$PROOFS/failures.jsonl")"
  run_gate waive fmt --reason=because
  assert_eq "waive --reason=value: exit 0" 0 "$RC"
  assert_eq "two auditable lines" 2 "$(wc -l <"$PROOFS/failures.jsonl" | tr -d ' ')"
}

t_verify() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  run_gate verify
  assert_eq "verify without proof: exit 1" 1 "$RC"
  run_gate commit
  run_gate verify
  assert_eq "verify after green: exit 0" 0 "$RC"
  run_gate verify commit
  assert_eq "verify commit: exit 0" 0 "$RC"
  run_gate verify push
  assert_eq "verify push without push proof: exit 1" 1 "$RC"
  stage "$FIX_WT" src/lib.rs
  run_gate verify
  assert_eq "verify after the tree changed: exit 1" 1 "$RC"
  run_gate verify nonsense
  assert_eq "verify with a bad kind: exit 2" 2 "$RC"
  printf '{"tree":"%s","rc":1}\n' "$(wt_tree)" >"$PROOFS/commit-$(wt_tree).json"
  run_gate verify
  assert_eq "verify with an rc!=0 proof: exit 1" 1 "$RC"
  assert_eq "verify ran no cargo" 3 "$(calls | grep -c '^cargo ')"
}
