# shellcheck shell=bash disable=SC2034
# cases_guard.sh - docs-only boundary, oracle trigger, main-clone refusal.

# docs_case <file> <expect>: stages <file> alone on a fresh fixture with fmt failing.
# expect=docs -> exit 0, cmd docs-only, no cargo; expect=code -> cargo ran (fmt failed, exit 1).
docs_case() {
  mkfix
  : >"$CTL/fail_fmt"
  stage "$FIX_WT" "$1"
  local tree
  tree=$(wt_tree)
  run_gate commit
  if [ "$2" = docs ]; then
    assert_eq "docs-only: $1 exit 0" 0 "$RC"
    assert_eq "docs-only: $1 cmd" docs-only "$(proof_field "$PROOFS/commit-$tree.json" cmd)"
    assert_eq "docs-only: $1 no cargo" 0 "$(calls | grep -c '^cargo ')"
  else
    assert_eq "NOT docs-only: $1 gates ran and failed" 1 "$RC"
    assert_ne "NOT docs-only: $1 cargo was called" 0 "$(calls | grep -c '^cargo ')"
    assert_nofile "NOT docs-only: $1 no proof" "$PROOFS/commit-$tree.json"
  fi
}

t_docs_only_positive() {
  local f
  for f in README.md CHANGELOG.md docs/new/deep/guide.md docs/diagram.png LICENSE LICENSE-MIT \
    .github/ISSUE_TEMPLATE/bug.yml; do
    docs_case "$f" docs
  done
}

t_docs_only_negative() {
  local f
  for f in src/new.rs src/deep/mod.rs build.rs docs/snippet.rs Cargo.toml Cargo.lock .github/workflows/ci.yml \
    scripts/x.sh scripts/notes.md tests/notes.md examples/readme.md sub/LICENSE .github/CODEOWNERS; do
    docs_case "$f" code
  done
}

t_docs_only_mixed() {
  mkfix
  : >"$CTL/fail_fmt"
  stage "$FIX_WT" README.md
  stage "$FIX_WT" src/lib.rs
  run_gate commit
  assert_eq "docs + .rs is not docs-only" 1 "$RC"
}

t_docs_only_push() {
  mkfix
  : >"$CTL/fail_fmt"
  commit_files "$FIX_WT" README.md docs/guide.md
  run_gate push
  assert_eq "push of docs only: exit 0" 0 "$RC"
  assert_eq "push docs-only cmd" docs-only "$(proof_field "$PROOFS/push-$(head_tree).json" cmd)"
  assert_eq "push docs-only ran no cargo" 0 "$(calls | grep -c '^cargo ')"
  commit_files "$FIX_WT" tests/x.rs
  run_gate push
  assert_eq "push with a .rs in the range: not docs-only" 1 "$RC"
  rm -f "$CTL/fail_fmt"
  stage "$FIX_WT" README.md
  g -C "$FIX_WT" commit -q -m docs2
  run_gate push
  assert_eq "a later docs-only commit does not hide the earlier .rs" 0 "$RC"
  assert_ne "...it ran the gates (range is merge-base..HEAD)" docs-only "$(proof_field "$PROOFS/push-$(head_tree).json" cmd)"
}

t_docs_only_amend_empty() {
  mkfix
  : >"$CTL/fail_fmt"
  run_gate commit
  assert_eq "empty index (amend): not docs-only, gates run" 1 "$RC"
}

# oracle_case <expect-called 0|1> <file>...: commits the files on feat, runs push.
oracle_case() {
  local want=$1 got
  shift
  mkfix
  commit_files "$FIX_WT" "$@"
  run_gate push
  got=$(calls | grep -c '^oracle')
  assert_eq "push $* : green" 0 "$RC"
  assert_eq "push $* : oracle calls" "$want" "$got"
}

t_oracle_trigger() {
  oracle_case 1 src/lib.rs
  oracle_case 1 src/deep/nested/mod.rs
  oracle_case 0 tests/only.rs
  oracle_case 0 Cargo.toml tests/x.rs
  oracle_case 0 src/notes.txt examples/e.rs
  mkfix
  commit_files "$FIX_WT" src/lib.rs
  commit_files "$FIX_WT" tests/later.rs
  run_gate push
  assert_eq "src .rs in an earlier commit of the range: oracle runs" 1 "$(calls | grep -c '^oracle')"
}

t_main_clone_refused() {
  mkfix
  stage "$FIX_MAIN" src/lib.rs
  run_gate_in "$FIX_MAIN" commit
  assert_eq "main clone, commit: exit 2" 2 "$RC"
  assert_out "main clone: message" "live-mounted main clone"
  assert_eq "main clone: no cargo" 0 "$(calls | grep -c '^cargo ')"
  assert_eq "main clone: no proof" 0 "$(count_named "$PROOFS" 'commit-*')"
  commit_files "$FIX_MAIN" src/other.rs
  run_gate_in "$FIX_MAIN" push
  assert_eq "main clone, push: exit 2" 2 "$RC"
  assert_eq "main clone push: no cargo" 0 "$(calls | grep -c '^cargo ')"
}

t_main_clone_docs_exception() {
  mkfix
  stage "$FIX_MAIN" README.md
  run_gate_in "$FIX_MAIN" commit
  assert_eq "main clone, docs-only: exit 0" 0 "$RC"
  assert_eq "main clone, docs-only: no cargo" 0 "$(calls | grep -c '^cargo ')"
}

t_worktree_accepted() {
  mkfix
  stage "$FIX_WT" src/lib.rs
  run_gate commit
  assert_eq "worktree under .claude/worktrees: accepted" 0 "$RC"
  g -C "$FIX_MAIN" worktree add -q "$FIX/elsewhere" -b other
  stage "$FIX/elsewhere" src/lib.rs
  run_gate_in "$FIX/elsewhere" commit
  assert_eq "linked worktree elsewhere (git-dir != common-dir): accepted" 0 "$RC"
}

# Documents a known false block: an ordinary clone outside .claude/worktrees is refused.
t_plain_clone_refused() {
  mkfix
  g clone -q "$FIX_MAIN" "$FIX/clone"
  g -C "$FIX/clone" update-ref refs/remotes/origin/main HEAD
  stage "$FIX/clone" src/lib.rs
  run_gate_in "$FIX/clone" commit
  assert_eq "plain clone (git-dir == common-dir): refused by design" 2 "$RC"
}
