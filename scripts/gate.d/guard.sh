# shellcheck shell=bash disable=SC2034,SC2153
# gate.d/guard.sh - sourced by scripts/gate.sh. Refusals, docs-only classifier, heavy-job lock.

# True (rc 0) when the work tree is the live-mounted main clone: not under .claude/worktrees/
# AND git-dir == git-common-dir. Nothing may be compiled there (the MCP server runs from it).
gate_is_main_clone() { # toplevel
  local gitdir common
  case "$1" in */.claude/worktrees/*) return 1 ;; esac
  gitdir=$(cd -P "$(git -C "$1" rev-parse --absolute-git-dir)" && pwd -P) || return 0
  common=$(git -C "$1" rev-parse --git-common-dir) || return 0
  case "$common" in /*) ;; *) common="$1/$common" ;; esac
  common=$(cd -P "$common" && pwd -P) || return 0
  [ "$gitdir" = "$common" ]
}

# rc 0 when path $1 cannot change what cargo builds or tests. Code-bearing directories and
# scripts/ are refused first, so a .md under tests/ or scripts/ is NOT docs-only.
gate_is_doc_path() {
  case "$1" in
    *.rs | Cargo.toml | */Cargo.toml | Cargo.lock | */Cargo.lock) return 1 ;;
    .github/workflows/* | scripts/* | src/* | tests/* | benches/* | crates/*) return 1 ;;
    examples/* | kani/* | fuzz/*) return 1 ;;
    *.md | docs/* | .github/ISSUE_TEMPLATE/*) return 0 ;;
    LICENSE*) case "$1" in */*) return 1 ;; *) return 0 ;; esac ;;
  esac
  return 1
}

# Reads NUL-separated paths on stdin. rc 0 iff the list is non-empty and every path is docs.
# An empty list is NOT docs-only: nothing proves the tree (e.g. `commit --amend` of HEAD).
gate_all_docs() {
  local n=0 f
  while IFS= read -r -d '' f; do
    n=$((n + 1))
    gate_is_doc_path "$f" || return 1
  done
  [ "$n" -gt 0 ]
}

# Reads NUL-separated paths on stdin. rc 0 iff one of them is src/**/*.rs.
gate_any_src_rs() {
  local f
  while IFS= read -r -d '' f; do
    case "$f" in src/*.rs) return 0 ;; esac
  done
  return 1
}

# NUL-separated paths changed by kind: commit -> index vs HEAD; push -> merge-base..HEAD.
# --no-renames lists both sides of a rename, so a moved .rs cannot hide behind a docs path.
gate_changed() { # kind
  if [ "$1" = commit ]; then
    git -C "$GATE_TOP" diff --cached --name-only -z --no-renames
  else
    git -C "$GATE_TOP" diff --name-only -z --no-renames "$GATE_MERGE_BASE" HEAD
  fi
}

# Sets GATE_MERGE_BASE (merge-base of ${GATE_BASE:-origin/main} and HEAD); rc 2 when absent.
gate_resolve_base() {
  local base=${GATE_BASE:-origin/main}
  GATE_MERGE_BASE=$(git -C "$GATE_TOP" merge-base "$base" HEAD 2>/dev/null) || {
    echo "gate.sh: cannot compute merge-base of $base and HEAD (git fetch origin?)" >&2
    return 2
  }
}

# rc 0 when tracked files in the work tree equal what is being proven (index / HEAD).
gate_tree_is_clean() { # kind
  if [ "$1" = commit ]; then
    git -C "$GATE_TOP" diff --quiet
  else
    git -C "$GATE_TOP" diff --quiet HEAD
  fi
}

# --- heavy-job lock: one at a time per repository (all its worktrees share the common dir).
# mkdir is atomic; the owner file holds "<pid> <process start time>" so a dead owner and a
# recycled PID are both detected. No flock on macOS.

gate_lock_owner_stamp() { # pid
  printf '%s %s' "$1" "$(ps -o lstart= -p "$1" 2>/dev/null | tr -s ' ')"
}

gate_lock_is_stale() {
  local lock=$1 owner pid
  owner=$(cat "$lock/owner" 2>/dev/null || true)
  if [ -z "$owner" ]; then
    # Crashed between mkdir and the owner write: stale once it is more than a minute old.
    [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]
    return
  fi
  pid=${owner%% *}
  [ "$(gate_lock_owner_stamp "$pid")" != "$owner" ]
}

# Removes a stale lock under a second mutex so two stealers cannot both delete a fresh lock.
gate_lock_steal() {
  local lock=$1
  [ -n "$(find "$lock.steal" -maxdepth 0 -mmin +1 2>/dev/null)" ] && rmdir "$lock.steal" 2>/dev/null
  mkdir "$lock.steal" 2>/dev/null || return 1
  gate_lock_is_stale "$lock" && rm -rf "$lock"
  rmdir "$lock.steal" 2>/dev/null
  return 0
}

# Takes the lock or returns 75 (EX_TEMPFAIL). Waits up to ${GATE_LOCK_WAIT:-0} seconds.
gate_lock_acquire() {
  local lock="$GATE_PROOFS/heavy.lock" waited=0 owner
  mkdir -p "$GATE_PROOFS"
  while :; do
    if mkdir "$lock" 2>/dev/null; then
      gate_lock_owner_stamp "$$" >"$lock/owner"
      GATE_LOCK_HELD=$lock
      return 0
    fi
    if gate_lock_is_stale "$lock" && gate_lock_steal "$lock"; then
      continue
    fi
    [ "$waited" -ge "${GATE_LOCK_WAIT:-0}" ] && break
    sleep 1
    waited=$((waited + 1))
  done
  owner=$(cat "$lock/owner" 2>/dev/null || echo unknown)
  echo "gate.sh: another heavy gate job holds $lock (owner: $owner); retry when it ends." >&2
  return 75
}

gate_lock_release() {
  [ -n "${GATE_LOCK_HELD:-}" ] || return 0
  if [ "$(cat "$GATE_LOCK_HELD/owner" 2>/dev/null)" = "$(gate_lock_owner_stamp "$$")" ]; then
    rm -rf "$GATE_LOCK_HELD"
  fi
  GATE_LOCK_HELD=
}
