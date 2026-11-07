#!/bin/sh
# Code-discovery nudge for the ai-architect plugin (issue #198).
#
#   discovery-nudge.sh session   -> the nudge as plain stdout (SessionStart)
#   discovery-nudge.sh subagent  -> {"hookSpecificOutput": {"hookEventName":
#                                   "SubagentStart", "additionalContext": ...}}
#
# Fail-open by contract: a hook that breaks session startup is worse than a
# missing nudge, so every failure path (unknown or missing mode, no awk, a
# closed stdout) exits 0 with no output. The output is built in full before
# anything is written, so a failure never leaves half a JSON document.
#
# The text names only tools in CORE_TOOL_NAMES (src/tool_profile.rs), which
# both profiles register; scripts/tests/test_discovery_nudge_hooks.py enforces
# that, the payload shapes, the fail-open paths and the byte budget.

nudge='ai-architect (plugin ai-architect-mcp-codebase) is installed. For structural code questions (where is X defined, who calls X, what breaks if X changes, what a diff touches) prefer its code-graph tools, prefix mcp__plugin_ai-architect-mcp-codebase_ai-architect__: search_codebase, get_symbol, get_context, get_impact, detect_changes, query_graph. If the repository is not indexed yet, call analyze_codebase on it first; health_check reports liveness. The graph is a parse of the supported languages, not ground truth: absence from the graph is not proof of absence. Before concluding that something does not exist, check query_graph(graph="missed"), and use Grep/Read for comments, strings and unsupported files.'

case "${1-}" in
session)
    out=$nudge
    ;;
subagent)
    # The text is one line with no control characters, so escaping the
    # backslash and the double quote is all JSON needs; the tests parse the
    # result and compare it with the session text.
    escaped=$(printf '%s' "$nudge" | awk '
        { gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); printf "%s", $0 }
    ' 2>/dev/null) || exit 0
    [ -n "$escaped" ] || exit 0
    out='{"hookSpecificOutput":{"hookEventName":"SubagentStart","additionalContext":"'$escaped'"}}'
    ;;
*)
    exit 0
    ;;
esac

printf '%s\n' "$out" 2>/dev/null
exit 0
