# shellcheck shell=bash disable=SC2034,SC2153
# gate.d/common.sh - sourced by scripts/gate.sh. Paths, JSON, ledger, proofs.
# Bash 3.2 compatible (macOS /bin/bash): no arrays under set -u, no mapfile.

# Seconds since epoch, sub-second when the platform allows (the Stop hook lifts a failure
# only by a proof whose `finished` is strictly later than the failure's `time`).
gate_now() {
  if [ -n "${EPOCHREALTIME:-}" ]; then
    printf '%s' "${EPOCHREALTIME/,/.}"
  elif command -v perl >/dev/null 2>&1; then
    perl -MTime::HiRes=time -e 'printf "%.6f", time'
  else
    date +%s
  fi
}

# JSON string literal of $1, printable ASCII only (control bytes and non-ASCII become '?').
gate_json_str() {
  local s
  s=$(printf '%s' "$1" | LC_ALL=C tr -c '[:print:]\n\t' '?')
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\t'/\\t}
  printf '"%s"' "$s"
}

# Masks secrets in text read from stdin (excerpts land in a file that may be shared).
gate_redact() {
  sed -E \
    -e "s/$(printf '\033')\\[[0-9;]*[A-Za-z]//g" \
    -e 's/(gh[pousr]_|github_pat_|sk-)[A-Za-z0-9_-]{8,}/[REDACTED]/g' \
    -e 's/AKIA[A-Z0-9]{12,}/[REDACTED]/g' \
    -e 's/([Bb]earer )[A-Za-z0-9._~+\/=-]{8,}/\1[REDACTED]/g' \
    -e 's/([A-Za-z_]*([Tt][Oo][Kk][Ee][Nn]|[Ss][Ee][Cc][Rr][Ee][Tt]|[Pp][Aa][Ss][Ss]([Ww][Oo][Rr])?[Dd]?|[Aa][Pp][Ii]_?[Kk][Ee][Yy])[A-Za-z_]*)=[^[:space:]]+/\1=[REDACTED]/g'
}

# Resolves the repo root and the shared proof directory.
gate_locate() {
  GATE_TOP=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "gate.sh: not inside a git work tree" >&2
    return 2
  }
  local common
  common=$(git -C "$GATE_TOP" rev-parse --git-common-dir 2>/dev/null) || return 2
  case "$common" in /*) ;; *) common="$GATE_TOP/$common" ;; esac
  GATE_PROOFS="$common/zetetic-gates"
}

# Expected tree of a kind: commit -> index now; push -> HEAD. Prints 40 hex.
gate_tree() {
  local t
  if [ "$1" = commit ]; then
    t=$(git -C "$GATE_TOP" write-tree 2>/dev/null)
  else
    t=$(git -C "$GATE_TOP" rev-parse 'HEAD^{tree}' 2>/dev/null)
  fi
  case "$t" in
    *[!0-9a-f]* | "") return 1 ;;
  esac
  [ "${#t}" -eq 40 ] || return 1
  printf '%s' "$t"
}

# Appends one failure row {"gate","kind","tree","rc","excerpt","time"}; excerpt <= 400 chars.
gate_record_failure() { # gate kind tree rc logfile
  local excerpt
  excerpt=$(tail -n 12 "$5" 2>/dev/null | gate_redact | LC_ALL=C tr -c '[:print:]\n\t' '?' | tail -c 400)
  mkdir -p "$GATE_PROOFS"
  printf '{"gate":%s,"kind":%s,"tree":%s,"rc":%s,"excerpt":%s,"time":%s}\n' \
    "$(gate_json_str "$1")" "$(gate_json_str "$2")" "$(gate_json_str "$3")" "$4" \
    "$(gate_json_str "$excerpt")" "$(gate_now)" >>"$GATE_PROOFS/failures.jsonl"
}

# SHA-256 of Cargo.lock as committed in tree $1; empty when the tree has none.
gate_lock_sha() {
  git -C "$GATE_TOP" cat-file -e "$1:Cargo.lock" 2>/dev/null || return 0
  git -C "$GATE_TOP" cat-file blob "$1:Cargo.lock" | shasum -a 256 | cut -d' ' -f1
}

# Writes <kind>-<tree>.json atomically (temp file, then mv). Only ever called with rc 0.
gate_write_proof() { # kind tree cmd started
  local kind=$1 tree=$2 cmd=$3 started=$4 tmp head rustc
  head=$(git -C "$GATE_TOP" rev-parse HEAD 2>/dev/null || true)
  rustc=$("${GATE_RUSTC:-rustc}" --version 2>/dev/null || echo unknown)
  mkdir -p "$GATE_PROOFS"
  tmp="$GATE_PROOFS/.tmp-$kind-$tree.$$"
  printf '{"tree":%s,"head":%s,"cargo_lock_sha256":%s,"rustc":%s,"cmd":%s,"rc":0,"started":%s,"finished":%s,"host":%s}\n' \
    "$(gate_json_str "$tree")" "$(gate_json_str "$head")" "$(gate_json_str "$(gate_lock_sha "$tree")")" \
    "$(gate_json_str "$rustc")" "$(gate_json_str "$cmd")" "$started" "$(gate_now)" \
    "$(gate_json_str "$(hostname 2>/dev/null || echo unknown)")" >"$tmp"
  if ! mv -f "$tmp" "$GATE_PROOFS/$kind-$tree.json"; then
    rm -f "$tmp"
    return 1
  fi
}

# rc 0 and prints the proof's cmd when <kind>-<tree>.json parses, has rc == 0 and tree == $2
# (the exact rule of ~/.claude/hooks/gate-proof.py). rc 1 otherwise or when python is absent.
gate_proof_valid() { # kind tree
  "${GATE_PYTHON:-python3}" - "$GATE_PROOFS/$1-$2.json" "$2" 2>/dev/null <<'PY'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        data = json.load(fh)
except (OSError, ValueError):
    sys.exit(1)
rc = data.get("rc") if isinstance(data, dict) else None
if isinstance(rc, bool) or rc != 0 or data.get("tree") != sys.argv[2]:
    sys.exit(1)
print(data.get("cmd", ""))
PY
}
