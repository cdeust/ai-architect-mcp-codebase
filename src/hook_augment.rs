// hook_augment — fail-open PreToolUse context augmenter (issue #59).
//
// Layer: shared/utility. A Claude Code PreToolUse hook: when the agent is about
// to run Grep or Glob in a project that has an ai-architect graph, emit a
// suggestion (as `hookSpecificOutput.additionalContext`) to consider the graph
// tools (search_codebase / query_graph) first. In EVERY other case — no graph,
// unparseable payload, a different tool, any missing field — emit NOTHING.
//
// CARDINAL RULE (CBM src/cli/hook_augment.c): the hook must NEVER block the tool
// call. `build_suggestion` therefore returns `Option<String>` and cannot error:
// every failure path returns `None`, and the binary wrapper prints nothing and
// exits 0. The error paths are tested explicitly (a malformed payload must be a
// silent no-op), because "fail open" is only true if the failure path is proven.

use serde_json::{json, Value};
use std::path::Path;

/// Marker that a project has an ai-architect graph. Convention: the committed
/// artifact directory (issue #55) — its presence means the repo carries a graph
/// a teammate can bootstrap, so suggesting the graph tools is worthwhile.
/// Re-exported from `artifact` rather than duplicated as a literal, so the two
/// modules cannot drift on the directory name (issue #195 rename fallout).
pub const GRAPH_MARKER_DIR: &str = crate::artifact::ARTIFACT_DIR;

/// Builds the PreToolUse `additionalContext` suggestion for a Grep/Glob call, or
/// `None` in every case where nothing should be emitted (fail-open). NEVER errors.
///
/// `graph_present(cwd)` decides whether the project has an indexed graph —
/// injected so the fail-open behavior is testable without a filesystem.
pub fn build_suggestion<F>(payload: &str, graph_present: F) -> Option<String>
where
    F: Fn(&Path) -> bool,
{
    // Any parse failure → None (never block).
    let v: Value = serde_json::from_str(payload).ok()?;
    let tool = v.get("tool_name")?.as_str()?;
    if tool != "Grep" && tool != "Glob" {
        return None;
    }
    let cwd = v.get("cwd")?.as_str()?;
    if !graph_present(Path::new(cwd)) {
        return None;
    }
    let pattern = v
        .get("tool_input")
        .and_then(|ti| ti.get("pattern"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let for_q = if pattern.is_empty() {
        String::new()
    } else {
        format!(" for '{pattern}'")
    };
    let ctx = format!(
        "An ai-architect code graph is indexed for this project. Before relying on this {tool} \
         result, consider search_codebase (ranked symbol search) or query_graph (structural \
         Cypher){for_q} — the graph gives structural context grep cannot (callers, blast radius, \
         communities). Note: absence from the graph is NOT a completeness guarantee; run \
         index_status / query_graph(graph=\"missed\") to see uncovered files."
    );
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": ctx,
        }
    });
    Some(out.to_string())
}

/// Filesystem check the binary uses: a project has a graph iff the marker dir
/// exists under `cwd`. Runs the issue #195 one-shot legacy-dir migration
/// first, so a repo indexed before the rename still resolves — this hook
/// fires on every Grep/Glob, which makes it the highest-frequency touchpoint
/// and the fastest place for an unmigrated repo to self-heal.
pub fn graph_present_fs(cwd: &Path) -> bool {
    crate::artifact::migrate_legacy_dir(cwd);
    cwd.join(GRAPH_MARKER_DIR).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grep_payload(tool: &str, cwd: &str, pattern: &str) -> String {
        json!({
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "cwd": cwd,
            "tool_input": { "pattern": pattern }
        })
        .to_string()
    }

    #[test]
    fn grep_with_graph_emits_suggestion() {
        let out = build_suggestion(&grep_payload("Grep", "/repo", "handle_tool"), |_| true)
            .expect("suggestion");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            json!("PreToolUse")
        );
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.contains("search_codebase"),
            "must steer to search_codebase"
        );
        assert!(ctx.contains("handle_tool"), "must echo the grep pattern");
    }

    #[test]
    fn glob_is_also_augmented() {
        assert!(build_suggestion(&grep_payload("Glob", "/repo", "**/*.rs"), |_| true).is_some());
    }

    #[test]
    fn no_graph_is_silent() {
        assert!(build_suggestion(&grep_payload("Grep", "/repo", "x"), |_| false).is_none());
    }

    #[test]
    fn non_grep_tool_is_silent() {
        assert!(build_suggestion(&grep_payload("Read", "/repo", "x"), |_| true).is_none());
    }

    #[test]
    fn graph_present_fs_migrates_a_legacy_marker_dir() {
        // Issue #195: `graph_present_fs` is the highest-frequency touchpoint
        // (fires on every Grep/Glob), so it must self-heal a pre-rename repo
        // rather than silently going blind on it.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".automatised-pipeline")).expect("mk legacy dir");
        assert!(graph_present_fs(tmp.path()));
        assert!(tmp.path().join(GRAPH_MARKER_DIR).is_dir());
        assert!(!tmp.path().join(".automatised-pipeline").exists());
    }

    #[test]
    fn malformed_payload_is_silent_fail_open() {
        // The cardinal-rule test: garbage in → None (the binary prints nothing,
        // exits 0), never a block.
        assert!(build_suggestion("{ not json at all", |_| true).is_none());
        assert!(build_suggestion("", |_| true).is_none());
        assert!(build_suggestion("null", |_| true).is_none());
        // Missing tool_name / cwd.
        assert!(build_suggestion(&json!({"cwd":"/r"}).to_string(), |_| true).is_none());
        assert!(build_suggestion(&json!({"tool_name":"Grep"}).to_string(), |_| true).is_none());
    }
}
