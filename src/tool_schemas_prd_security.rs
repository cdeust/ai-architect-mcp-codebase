//! PRD-grounding and post-implementation gate tool schemas (stages 4, 6, 8,
//! 9): `prepare_prd_input`, `validate_prd_against_graph`,
//! `check_security_gates`, `verify_semantic_diff`.
//!
//! Split out of `tool_schemas.rs` (issue #289) to bring that file under
//! §4.1's 500-line cap. Pure code move, following the pattern established by
//! `tool_schemas_index_codebase.rs` / `tool_schemas_analyze_codebase.rs`.

use serde_json::{json, Value};

pub(super) fn prepare_prd_input_schema() -> Value {
    json!({
        "name": "prepare_prd_input",
        "description": "Stage 4 — Bundle graph intel (matched symbols, impacted communities, impacted processes, graph stats) into stage-4.prd_input.json for the PRD generator. Read-only against the graph. TWO modes: (1) FINDING mode — pass finding_id to bundle a VERIFIED stage-2 finding (writes under runs/<run_id>/findings/<finding_id>/ and updates index.json); (2) FEATURE mode — pass feature_description (no finding_id) to ground a free-text feature directly on the code graph, skipping the stage-2 gate (writes under runs/<run_id>/features/<slug>/). Provide finding_id OR feature_description. Grounding trust (v1.1.0, issue #14): each `matched_symbols` entry carries `match_mode` ('verbatim' — identifier cited in backticks and resolved exactly; 'exact_name' — a description word equals the symbol's name exactly) and `confidence`; only these are counted as verified grounding. Substring/fuzzy hits with no exact-identity evidence are returned separately in `candidate_symbols` (same shape, `match_mode: 'lexical'`) and are NEVER folded into matched_symbols or into impacted_communities/impacted_processes — an empty matched_symbols is expected and correct when the description contains no cited or exactly-named identifiers. Cite identifiers in backticks in finding/feature descriptions to get verbatim-priority grounding.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["output_dir", "graph_path"],
            "additionalProperties": false,
            "properties": {
                "run_id":              { "type": "string", "description": "Pipeline run id (path segment). Defaults to 'adhoc' in feature mode." },
                "finding_id":          { "type": "string", "description": "Finding mode: the verified stage-2 finding to bundle. Omit for feature mode." },
                "feature_description": { "type": "string", "description": "Feature mode: free-text feature/intent to ground on the graph. Used when finding_id is absent." },
                "output_dir":          { "type": "string", "pattern": "^/.+" },
                "graph_path":          { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

pub(super) fn validate_prd_against_graph_schema() -> Value {
    json!({
        "name": "validate_prd_against_graph",
        "description": "Stage 6 — Validate a PRD against the resolved+clustered graph. Three axes: (1) symbol hallucination — claimed symbols that don't exist (critical); (2) community-consistency — affected symbols spanning multiple Leiden communities (warning/critical); (3) process-impact contradiction — PRD claims 'does not affect X' while a changed symbol participates in X (critical). Contract-first on stage-5.affected_symbols.json with regex fallback from the PRD markdown. LLM-free. Read-only. When run_id+finding_id+output_dir are provided, writes stage-6.validation.json under findings/<finding_id>/.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["prd_path", "graph_path"],
            "additionalProperties": false,
            "properties": {
                "prd_path":    { "type": "string", "pattern": "^/.+" },
                "graph_path":  { "type": "string", "pattern": "^/.+" },
                "affected_symbols_path": { "type": "string", "pattern": "^/.+", "description": "Optional absolute path to stage-5.affected_symbols.json. When absent, regex fallback extracts claims from the PRD text." },
                "output_dir":  { "type": "string", "pattern": "^/.+", "description": "Optional; required together with run_id + finding_id to write stage-6.validation.json." },
                "run_id":      { "type": "string" },
                "finding_id":  { "type": "string" }
            }
        }
    })
}

pub(super) fn check_security_gates_schema() -> Value {
    json!({
        "name": "check_security_gates",
        "description": "Stage 8 — Graph-aware security gates. Runs five checks on the changed_symbols list: S1 auth-critical community touch (critical), S2 unsafe-symbol touch (info-skip until parser records is_unsafe), S3 public-API surface change (warning), S4 unresolved-import presence in the changed file (warning/critical), S5 test-coverage structural gap (warning). Returns gates_passed=true iff zero critical flags; report.assessment_complete=false for empty input, skipped checks, or unresolved changed symbols. Neither field certifies security. Import-check query errors fail explicitly. LLM-free. Read-only. When run_id+finding_id+output_dir are provided, writes stage-8.security.json.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "changed_symbols"],
            "additionalProperties": false,
            "properties": {
                "graph_path":      { "type": "string", "pattern": "^/.+" },
                "changed_symbols": { "type": "array", "items": { "type": "string" }, "minItems": 0 },
                "output_dir":      { "type": "string", "pattern": "^/.+", "description": "Optional; required together with run_id + finding_id to write stage-8.security.json." },
                "run_id":          { "type": "string" },
                "finding_id":      { "type": "string" }
            }
        }
    })
}

pub(super) fn verify_semantic_diff_schema() -> Value {
    json!({
        "name": "verify_semantic_diff",
        "description": "Stage 9 — Compare a post-implementation graph against a pre-implementation graph to flag regressions: nodes added/removed, edges added/removed, dangling references (edges whose target disappeared), new unresolved imports, and new strongly-connected cycles. Returns a heuristic regression_score (cap 10.0, thresholds: <1 clean, <5 concerning, >=5 regression); any positive unresolved-import delta is at least concerning. Details are sorted before truncation. Clean is a structural policy verdict, not behavioral equivalence or compilation success. Read-only against both graphs.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["before_graph_path", "after_graph_path"],
            "additionalProperties": false,
            "properties": {
                "before_graph_path": { "type": "string", "pattern": "^/.+" },
                "after_graph_path":  { "type": "string", "pattern": "^/.+" },
                "report_path":       { "type": "string", "pattern": "^/.+", "description": "Optional absolute path where the full report JSON is written. If absent, the report is returned inline only." }
            }
        }
    })
}
