# shellcheck shell=bash disable=SC2034,SC2153
# gate.d/commands.sh - sourced by scripts/gate.sh. The four subcommands.
# Exit codes: 0 proof written / valid; 1 a gate failed (or verify found no proof);
# 2 refused (usage, main clone, dirty tree, no base); 75 heavy lock busy; 143 interrupted.

# Sets GATE_PLAN (gate names to run for $1 commit|push, $2 tree) and GATE_REUSED when the commit
# proof of the same tree covers the commit gates. A docs-only commit proof never does:
# it was granted on one commit's diff and says nothing about fmt/clippy/unit.
gate_plan() { # kind tree
  local names="fmt clippy unit" cmd
  GATE_REUSED=
  if [ "$1" = push ]; then
    names="test bench"
    if cmd=$(gate_proof_valid commit "$2") && [ "$cmd" != docs-only ]; then
      GATE_REUSED=1
    else
      names="fmt clippy unit test bench"
    fi
    if gate_changed push | gate_any_src_rs; then names="$names oracle"; fi
  fi
  GATE_PLAN=$names
}

# Re-reads the tree after the gates: a gate that rewrote tracked files (e.g. Cargo.lock)
# proved something other than what would be committed.
gate_check_stable() { # kind tree
  local now
  now=$(gate_tree "$1") || now=unknown
  if [ "$now" = "$2" ] && gate_tree_is_clean "$1"; then return 0; fi
  echo "tracked files changed while the gates ran (tree $2 -> $now)" >"$GATE_TMP/tree-stable.log"
  echo "gate tree-stable: FAILED; $(cat "$GATE_TMP/tree-stable.log")" >&2
  gate_record_failure tree-stable "$1" "$2" 1 "$GATE_TMP/tree-stable.log"
  GATE_FAILED="$GATE_FAILED tree-stable"
  return 1
}

# Everything after the docs-only short-circuit: refusals, lock, gates, proof.
gate_prove() { # kind tree started
  local kind=$1 tree=$2 started=$3
  if gate_is_main_clone "$GATE_TOP"; then
    echo "gate.sh: refusing to compile in the live-mounted main clone ($GATE_TOP)." >&2
    echo "Work in a worktree: git worktree add .claude/worktrees/<topic> -b <branch> origin/main" >&2
    return 2
  fi
  if ! gate_tree_is_clean "$kind"; then
    echo "gate.sh: tracked files differ from the tree being proven; stage or revert them first." >&2
    return 2
  fi
  GATE_TMP=$(mktemp -d "${TMPDIR:-/tmp}/gate.XXXXXX") || return 2
  gate_lock_acquire || return $?
  GATE_FAILED=
  gate_plan "$kind" "$tree"
  [ -z "$GATE_REUSED" ] || echo "gate: commit proof reused for tree $tree" >&2
  if ! gate_run_all "$kind" "$tree" "$GATE_PLAN" || ! gate_check_stable "$kind" "$tree"; then
    rm -f "$GATE_PROOFS/$kind-$tree.json"
    echo "gate.sh $kind: FAILED:$GATE_FAILED (no proof written; see $GATE_PROOFS/failures.jsonl)" >&2
    return 1
  fi
  gate_write_proof "$kind" "$tree" "gate.sh $kind [${GATE_REUSED:+commit-proof }$GATE_PLAN]" "$started" || return 2
  echo "gate.sh $kind: proof written for tree $tree" >&2
}

gate_cmd_run() { # kind
  local kind=$1 tree started
  gate_locate || return 2
  tree=$(gate_tree "$kind") || {
    echo "gate.sh: cannot compute the $kind tree" >&2
    return 2
  }
  started=$(gate_now)
  [ "$kind" = commit ] || gate_resolve_base || return 2
  if gate_changed "$kind" | gate_all_docs; then
    gate_write_proof "$kind" "$tree" docs-only "$started" || return 2
    echo "gate.sh $kind: docs-only change set, proof written for tree $tree (no cargo)" >&2
    return 0
  fi
  gate_prove "$kind" "$tree" "$started"
}

gate_cmd_verify() { # [kind]
  local kind=${1:-commit} tree
  case "$kind" in commit | push) ;; *)
    echo "usage: gate.sh verify [commit|push]" >&2
    return 2
    ;;
  esac
  gate_locate || return 2
  tree=$(gate_tree "$kind") || {
    echo "gate.sh: cannot compute the $kind tree" >&2
    return 2
  }
  if gate_proof_valid "$kind" "$tree" >/dev/null; then
    echo "valid $kind proof for tree $tree"
    return 0
  fi
  echo "no valid $kind proof for tree $tree" >&2
  return 1
}

# waive <gate> --reason "..." : one auditable line in failures.jsonl. Changes no gate result.
gate_cmd_waive() {
  local gate="" reason="" have=0 n
  while [ $# -gt 0 ]; do
    case "$1" in
      --reason) have=1; reason=${2-}; shift; [ $# -eq 0 ] || shift ;;
      --reason=*) have=1; reason=${1#--reason=}; shift ;;
      -*) echo "gate.sh waive: unknown option $1" >&2; return 2 ;;
      *) [ -z "$gate" ] || { echo "gate.sh waive: one gate only" >&2; return 2; }; gate=$1; shift ;;
    esac
  done
  n=$(printf '%s' "$reason" | tr -d '[:space:]')
  if [ "$have" -eq 0 ] || [ -z "$n" ] || [ -z "$gate" ]; then
    echo "usage: gate.sh waive <gate> --reason \"<non-empty reason>\"   (gates: $GATE_NAMES)" >&2
    return 2
  fi
  case " $GATE_NAMES " in *" $gate "*) ;; *)
    echo "gate.sh waive: unknown gate '$gate' (gates: $GATE_NAMES)" >&2
    return 2
    ;;
  esac
  gate_locate || return 2
  mkdir -p "$GATE_PROOFS"
  printf '{"waive":%s,"reason":%s,"time":%s}\n' "$(gate_json_str "$gate")" \
    "$(gate_json_str "$reason")" "$(gate_now)" >>"$GATE_PROOFS/failures.jsonl"
  echo "gate.sh: waiver recorded for '$gate' in $GATE_PROOFS/failures.jsonl (reason: $reason)" >&2
}
