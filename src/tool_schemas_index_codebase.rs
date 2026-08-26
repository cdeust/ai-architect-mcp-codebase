//! `index_codebase`'s tool schema and its parameter definitions.
//!
//! Split out of `tool_schemas.rs` (review round 2, finding 5). That file was
//! 884 lines on `main` — already far past §4.1's 500-line cap — and extracting
//! these parameters into functions WITHIN it, to fix a §4.2 function-length
//! violation, pushed it to 922. A split that relocates a violation and grows
//! the file is not a split; this moves the whole tool out, which is the shape
//! the rest of the file should follow as it is broken up per tool.
//!
//! The descriptions are long because each documents a behavioural contract a
//! caller depends on; shortening the prose would cost the caller more than the
//! split does.

use super::shared_params::{dependency_scope_param, include_dependencies_param};
use serde_json::{json, Value};

pub(super) fn index_codebase_schema() -> Value {
    json!({
        "name": "index_codebase",
        "description": "Stage 3a — Index a codebase. Walks the directory, parses source files with tree-sitter (Rust, Python, TypeScript), and persists a code-intelligence graph (nodes: functions, structs/classes, enums, traits/interfaces, etc.; edges: contains, defines, has_method, etc.) into a LadybugDB database at <output_dir>/graph/. Returns node/edge counts, elapsed time, and a COVERAGE report (issue #57) listing files that were parse-incomplete, skipped, or quarantined — absence of a flag is NOT a completeness guarantee; query the full report any time via index_status or query_graph(graph=\"missed\").",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["path", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the codebase root to index."
                },
                "language": {
                    "type": "string",
                    "enum": ["auto", "rust", "python", "typescript", "java", "kotlin", "swift", "objc", "c", "cpp", "go"],
                    "default": "auto",
                    "description": "Language to parse. 'auto' detects per-file by extension (.rs, .py, .ts/.tsx, .java, .kt/.kts, .swift, .m/.mm, .c/.h, .cc/.cpp/.hpp, .go). Specific values restrict to that language only."
                },
                "output_dir": {
                    "type": "string",
                    "description": "Absolute directory where the graph will be stored (at <output_dir>/graph/)."
                },
                "dependency_scope": dependency_scope_param(),
                "include_dependencies": include_dependencies_param(),
                "export_artifact": export_artifact_param(),
                "bootstrap": bootstrap_param(),
                "accept_stale": accept_stale_param(),
                "full": full_rebuild_param(),
                "exclude_dirs": exclude_dirs_param(),
                "cochange": cochange_param()
            }
        }
    })
}

// `index_codebase`'s parameters, one function each, following this module's
// existing `detail_param`/`format_param` convention. Extracted because the
// combined literal put `index_codebase_schema` at 70 lines, over §4.2's cap —
// these descriptions are long because each documents a behavioural contract,
// and shortening the prose would cost a caller more than the split does.

fn export_artifact_param() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Issue #55 — after a successful index, write a team-shared compressed graph snapshot to <path>/.ai-architect-mcp-codebase/graph.zst (+ graph.meta.json sidecar with git sha, tool version, node/edge counts) and a .gitattributes 'merge=ours' entry so the committed blob never produces merge conflicts. Uses the best-ratio (zstd-9) tier. Export failure is logged but does not fail the index."
    })
}

fn bootstrap_param() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Issue #55/#62 — when there is no local graph at <output_dir>/graph but a committed artifact exists at <path>/.ai-architect-mcp-codebase/graph.zst, import (decompress) that snapshot instead of cold-indexing, so a fresh clone skips the full index. Staleness is checked first: a FRESH artifact (sha == HEAD) is imported as-is (response source='artifact_bootstrap', graph_state='fresh'). A STALE artifact is, by DEFAULT, imported AND then incrementally filled up to the working tree — only the artifact→HEAD diff is re-parsed (response source='artifact_bootstrap_fill', graph_state='filled_to_working_tree', with fill_method and {changed,added,deleted,renamed,unchanged} counts). The fill derives its change set from 'git diff <artifact_sha> <working tree>' (renames included), falling back to the bundled manifest's content hashes when the repo is not a git tree. If the import or fill fails, it falls back to a full index explicitly (logged, with a 'bootstrap_skipped' note)."
    })
}

fn accept_stale_param() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Issue #55/#62 — only meaningful with bootstrap=true. Repurposed by the incremental-fill contract: when the committed artifact is stale, accept_stale=true imports the snapshot AS-IS and SKIPS the incremental fill (a deliberate fast path when HEAD-accuracy is not needed). The response carries graph_state='accepted_stale' and a 'stale_artifact' object {artifact_sha, head_sha, commits_behind} so the caller can never mistake the stale graph for a current one; the skipped delta is filled on the next local index_codebase run (which classifies the working tree against the bundled manifest). Leave false (the default) to bootstrap-then-fill up to the working tree."
    })
}

fn full_rebuild_param() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Issue #62 — force a from-scratch full rebuild. By DEFAULT index_codebase is incremental: when a prior graph at <output_dir>/graph and its file_manifest.json exist, only the files that changed since the last index are re-parsed (the response carries mode='incremental' and {changed, added, deleted, renamed, unchanged} counts). Pass full=true to bypass that and rebuild everything — required when you change 'language', 'dependency_scope', or 'exclude_dirs', none of which the manifest captures."
    })
}

fn exclude_dirs_param() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "default": [],
        "description": "Issue #249 — directory paths (relative to 'path', no leading '/' or '..') or bare directory names to prune from the walk, in addition to the built-in build/dependency skip list. An entry WITHOUT a path separator (e.g. \"secrets\") is a bare name matched anywhere in the tree, like the built-in list; an entry WITH one (e.g. \"config/secrets\") matches exactly that one subtree relative to 'path'. No glob support. Exclusion WINS over every 'dependency_scope' tier, including 'full' — this is for directories that must never be read, not a performance prune. Pruned directories are NEVER silently dropped: each is reported in the coverage sidecar as skipped (reason 'user_excluded'), and the response's coverage.skipped.user_excluded_count carries the total. Changing this value on an existing graph requires full=true (the manifest does not capture it, same caveat as 'dependency_scope'). A directory the OS refuses to read (permission denied) is handled independently and automatically — see the coverage 'unreadable' reason — even without listing it here."
    })
}

fn cochange_param() -> Value {
    json!({
        "type": "boolean",
        "default": true,
        "description": "Issue #58 — after indexing, mine git temporal coupling into FILE_CHANGES_WITH File→File edges (Tornhill-style: files that change together, thresholded at >=3 co-changes and >=0.30 coupling degree over a 1-year window). Default true; self-skips on a non-git tree. A full index re-mines the window; an incremental index EXTENDS the mined aggregates with only the new commits (append-only). The response carries a 'cochange' block {mode, commits_scanned, edges_written}. Query the edges via query_graph: `MATCH (a:File)-[r:FILE_CHANGES_WITH]-(b:File) WHERE r.coupling > 0.5 RETURN a.id, b.id, r.cochange_count`."
    })
}
