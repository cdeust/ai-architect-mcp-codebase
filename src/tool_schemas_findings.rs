//! The findings-refinement + clarification-session pipeline's tool schemas
//! (stages 1a-2d): `extract_finding`, `refine_finding`, `start_verification`,
//! `append_clarification`, `finalize_verification`, `abort_verification`, and
//! their shared sub-schemas.
//!
//! Split out of `tool_schemas.rs` (issue #289) to bring that file under
//! §4.1's 500-line cap. Pure code move, following the pattern established by
//! `tool_schemas_index_codebase.rs` / `tool_schemas_analyze_codebase.rs`.

use serde_json::{json, Value};

pub(super) fn extract_finding_schema() -> Value {
    json!({
        "name": "extract_finding",
        "description": "Stage 1a — Deterministic extraction. Normalizes one incoming finding (inline object or absolute path to a .json file) to the canonical schema, writes stage-1.source.json + stage-1.extracted.json atomically under <output_dir>/runs/<run_id>/findings/<finding_id>/, and creates or updates an index.json entry. Does NOT call an LLM. The caller runs the orchestrator refinement and then calls refine_finding with the payload.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["finding", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "finding": {
                    "description": "Either an inline finding object (must match spec §3.2) or an absolute path to a .json file containing a finding or a {findings: [...]} wrapper with exactly one entry. .md paths are rejected in v1 (spec §9.3 Q1).",
                    "oneOf": [
                        { "type": "object" },
                        { "type": "string", "pattern": "^/.+\\.json$" }
                    ]
                },
                "output_dir": {
                    "type": "string",
                    "pattern": "^/.+",
                    "description": "Absolute directory where the artifact will be staged."
                },
                "run_id": {
                    "type": "string",
                    "description": "Optional run identifier. Auto-generated as YYYYMMDD-HHMMSS-<6 lowercase alphanumeric> (UTC) when absent."
                }
            }
        }
    })
}

pub(super) fn refine_finding_schema() -> Value {
    json!({
        "name": "refine_finding",
        "description": "Stage 1b — Orchestrator-aware persistence. Reads an existing stage-1.extracted.json, composes stage-1.refined.json with the agent-produced refined_prompt + refinement payload, and updates index.json atomically. Pure persistence — no LLM call, no network. Requires extract_finding to have been called first for the same (run_id, finding_id).",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir", "refined_prompt", "refinement"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "refined_prompt": refined_prompt_schema(),
                "refinement": refinement_schema(),
            }
        }
    })
}

fn refined_prompt_schema() -> Value {
    json!({
        "type": "object",
        "required": ["text", "role_hint"],
        "additionalProperties": false,
        "properties": {
            "text":           { "type": "string", "minLength": 1 },
            "role_hint":      { "type": "string" },
            "token_estimate": { "type": ["integer", "null"] }
        }
    })
}

fn refinement_schema() -> Value {
    json!({
        "type": "object",
        "required": ["added_context", "orchestrator_version"],
        "additionalProperties": false,
        "properties": {
            "added_context": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["kind", "content"],
                    "additionalProperties": false,
                    "properties": {
                        "kind":       { "type": "string" },
                        "content":    { "type": "string" },
                        "provenance": { "type": "string" }
                    }
                }
            },
            "orchestrator_version": { "type": "string" }
        }
    })
}

pub(super) fn start_verification_schema() -> Value {
    json!({
        "name": "start_verification",
        "description": "Stage 2a — Create a clarification session for a refined finding. Verifies stage-1.refined.json exists and parses (schema_ok), then atomically writes stage-2.session.json with state 'open'. Rejects if an existing session is finalized; overwrites an aborted session. No LLM call.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

pub(super) fn append_clarification_schema() -> Value {
    json!({
        "name": "append_clarification",
        "description": "Stage 2b — Append one turn (agent_question or user_answer) to stage-2.session.json. Enforces the alternation invariant (two consecutive same-kind turns rejected) and the §3 state machine. Whole-file atomic rewrite per spec §12.3. No LLM call.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir", "kind", "content"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "kind":       { "enum": ["agent_question", "user_answer"] },
                "content":    { "type": "string", "minLength": 1 },
                "meta":       { "type": "object" }
            }
        }
    })
}

pub(super) fn finalize_verification_schema() -> Value {
    json!({
        "name": "finalize_verification",
        "description": "Stage 2c — Consume the user-ready signal. Rejects from state 'open' (no_clarification_round) or 'waiting_for_user' (unanswered_question) per spec §12.2. Computes sha256 over the canonical transcript bytes, writes stage-2.verified.json atomically, flips the session to 'finalized', and updates index.json with verified+stage2_path. No LLM call.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

pub(super) fn abort_verification_schema() -> Value {
    json!({
        "name": "abort_verification",
        "description": "Stage 2d — Kill a non-terminal session. Atomically rewrites stage-2.session.json with state 'aborted', aborted_at, and optional abort_reason. Does NOT touch index.json (aborted sessions are invisible to stage 3). A fresh start_verification after abort overwrites the session.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "reason":     { "type": "string" }
            }
        }
    })
}
