#!/usr/bin/env bash
# gate.sh - local proof gates. Owner rule 2026-09-30: only valid, working code is committed.
#   gate.sh commit | push | verify [commit|push] | waive <gate> --reason "..."
# Proofs: $(git rev-parse --git-common-dir)/zetetic-gates/<kind>-<tree>.json (read by the
# gate-proof hook); failures and waivers: same directory, failures.jsonl (read by the Stop hook).
# Local only: bench and oracle are never added to CI (owner decision: heavy measures run here).
# Env (tests inject stubs): GATE_CARGO GATE_PYTHON GATE_RUSTC GATE_ORACLE_CMD GATE_BASE GATE_LOCK_WAIT.
#
# Commands follow .github/workflows/ci.yml, with two stated differences:
#  - clippy is --workspace (CI's job is workspace-wide; the narrower local run would pass
#    what CI rejects, the gap behind issue #42). Owner's brief said the root crate only.
#  - no --locked (CI has none); instead a run that rewrites tracked files (Cargo.lock) fails
#    the `tree-stable` gate, so the proof always describes the tree that will be committed.
# `unit` (cargo test --lib) is a fast subset of CI's `cargo test`, run before the full `test`.
# shellcheck source-path=SCRIPTDIR disable=SC2034
set -u

GATE_SELF=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=gate.d/common.sh
. "$GATE_SELF/gate.d/common.sh"
# shellcheck source=gate.d/guard.sh
. "$GATE_SELF/gate.d/guard.sh"
# shellcheck source=gate.d/gates.sh
. "$GATE_SELF/gate.d/gates.sh"
# shellcheck source=gate.d/commands.sh
. "$GATE_SELF/gate.d/commands.sh"

GATE_CHILD=
GATE_TMP=
GATE_LOCK_HELD=
trap gate_cleanup EXIT
trap gate_on_signal INT TERM HUP

case "${1:-}" in
  commit | push) gate_cmd_run "$1" ;;
  verify) shift; gate_cmd_verify "$@" ;;
  waive) shift; gate_cmd_waive "$@" ;;
  *)
    echo "usage: gate.sh commit | push | verify [commit|push] | waive <gate> --reason \"...\"" >&2
    exit 2
    ;;
esac
