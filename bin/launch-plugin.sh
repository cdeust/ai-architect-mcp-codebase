#!/usr/bin/env bash
# Claude Code launcher. CLAUDE_PLUGIN_ROOT is supplied by the plugin runtime
# and points at the immutable installed package, so no registry-file lookup is
# needed and renamed marketplace coordinates cannot break startup.
set -euo pipefail

PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
"$PLUGIN_ROOT/bin/ensure-binary.sh" >&2
exec "$PLUGIN_ROOT/target/release/ai-architect-mcp-codebase" "$@"
