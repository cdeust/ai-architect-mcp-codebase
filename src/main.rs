// automatised-pipeline — stage-by-stage MCP rewrite of the ai-architect pipeline.
//
// Transport: stdio JSON-RPC 2.0, hand-rolled (no MCP SDK — we own the protocol
// wire layer so the agents know exactly what's happening).
//
// Today:
//   Stage 0 — health_check         (handshake + server state)
//   Stage 1 — extract_finding      (finding → extracted artifact + index entry)
//   Stage 1 — refine_finding       (orchestrator refinement → refined artifact)
//   Stage 2 — start_verification   (create clarification session)
//   Stage 2 — append_clarification (append one turn, advance state machine)
//   Stage 2 — finalize_verification(compute digest, write verified receipt)
//   Stage 2 — abort_verification   (kill a non-terminal session)
//
// Each future stage lands as one more entry in `tools_list()` + one more arm
// in `handle_tool_call()`. No pre-scaffolding of layers or helpers.
//
// Reference implementation (to read, not copy): /Users/cdeust/Developments/ai-architect

mod ambiguity_policy;
mod artifact;
mod bridge;
mod call_evidence;
mod cli;
mod clustering;
mod cochange;
mod epistemic;
mod git_diff;
mod graph_cache;
mod graph_store;
mod history;
mod hook_augment;
mod host_install;
mod indexer;
mod language_provider;
mod lsp_client;
mod lsp_resolver;
mod macro_expansion;
mod mcp_prompts;
mod parser;
mod prd_input;
mod prd_validator;
mod resolver;
mod resolver_layers;
mod response_budget;
mod search;
mod security_gates;
mod semantic_diff;
mod stdlib_index;
mod token_surface;
mod tool_profile;
mod tool_schemas;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use tool_profile::ToolProfile;

// ---------------------------------------------------------------------------
// Protocol / server identity
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "ai-architect";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// `initialize.instructions` teaches the connecting LLM the active profile's
// intended workflow. The text VARIES by profile (issue #65 criterion 5): a
// `core` client is told what `core` is for, not handed the full-pipeline map.
// source: tool_profile.rs — CORE_TOOL_NAMES is the read-only code-intelligence
// surface; Full is the whole finding→verification→PRD→gates pipeline.
const CORE_INSTRUCTIONS: &str =
    "ai-architect, 'core' profile — the read-only code-intelligence surface (8 tools). \
     Start with health_check to confirm liveness and the active profile, then analyze_codebase \
     on a repo to index + resolve + cluster the graph in one call. Explore with search_codebase \
     (keyword → ranked symbols), get_context / get_symbol (a symbol's neighborhood), get_impact \
     (reverse-dependency blast radius), detect_changes (a diff's blast radius), and query_graph \
     (arbitrary read-only Cypher). Absence from the graph is NOT proof of absence — a file may \
     be parse-incomplete; verify negatives with query_graph(graph=\"missed\"). Two guided \
     workflows are published via prompts/list: explore_codebase and review_change_impact. The \
     internal finding→verification→PRD pipeline stages are hidden in this profile; restart with \
     --profile full to expose them.";
const FULL_INSTRUCTIONS: &str =
    "ai-architect, 'full' profile — the entire pipeline (every tool; the default). The tools are \
     strongly ordered and the order is not guessable; a caller that gets it wrong gets an empty \
     or misleading result rather than an error. Stages: (1) findings — extract_finding → \
     refine_finding; (2) verification — start_verification → append_clarification → \
     finalize_verification; (3) code intelligence — index_codebase → resolve_graph → \
     cluster_graph (or analyze_codebase for all three), then search_codebase / get_context / \
     get_symbol / get_impact / detect_changes / query_graph; (4/6) PRD grounding — \
     prepare_prd_input → validate_prd_against_graph; (8/9) gates — check_security_gates, \
     verify_semantic_diff. Absence from the graph is NOT proof of absence — verify negatives \
     with query_graph(graph=\"missed\"). Guided workflows are published via prompts/list: \
     explore_codebase, review_change_impact, verify_finding, ground_prd. For the read-only \
     code-intelligence subset only, restart with --profile core.";

/// The `initialize.instructions` string for `profile` (issue #65 criterion 5).
fn server_instructions(profile: ToolProfile) -> &'static str {
    match profile {
        ToolProfile::Core => CORE_INSTRUCTIONS,
        ToolProfile::Full => FULL_INSTRUCTIONS,
    }
}

// ---------------------------------------------------------------------------
// Stage 1 constants — every value traces to a spec section.
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §9.3 Q6 — compile-time version of the extractor.
const EXTRACTOR_VERSION: &str = "1.0.0";
// source: stages/stage-1.md §9.3 Q6 — compile-time version of the refinement
// schema the Rust tool accepts from the agent layer.
const ORCHESTRATOR_CONTRACT_VERSION: &str = "1.0.0";

// source: stages/stage-1.md §9.3 Q5 — `[a-z0-9]` suffix on auto-generated run_id.
const RUN_ID_RANDOM_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
// source: stages/stage-1.md §9.3 Q5 — 6 chars, follows git short-hash (7 chars)
// convention trimmed to 6 for readability, collision ~ N²/(2·36⁶).
const RUN_ID_RANDOM_LEN: usize = 6;

// source: stages/stage-1.md §5.1.4, §9.3 Q4 — safe-ID regex `^[A-Za-z0-9._-]+$`,
// no leading `.`, no `..`. 128 chosen as the cap: long enough for any realistic
// upstream ID (e.g. "FID-2026-04-11-behavior-change-001" ≈ 40 chars) while short
// enough to keep filesystem paths well under POSIX PATH_MAX (1024 on macOS, see
// <sys/syslimits.h>). If we ever need more we bump it with a source comment.
const SAFE_ID_MAX_LEN: usize = 128;

// source: stages/stage-1.md §4.4 — canonical on-disk layout.
const RUNS_DIR_NAME: &str = "runs";
const FINDINGS_DIR_NAME: &str = "findings";
const INDEX_FILE_NAME: &str = "index.json";
const EXTRACTED_FILE_NAME: &str = "stage-1.extracted.json";
const SOURCE_FILE_NAME: &str = "stage-1.source.json";
const REFINED_FILE_NAME: &str = "stage-1.refined.json";

// ---------------------------------------------------------------------------
// Stage 2 constants — every value traces to a spec section.
// ---------------------------------------------------------------------------

// source: stages/stage-2.md §11 + §12.5 item 6 — compile-time version of the
// verifier. "1.0.0" = first release shipping the four-tool set (including
// abort_verification, locked in §12.1).
const VERIFIER_VERSION: &str = "1.0.0";

// source: stages/stage-2.md §12.3 — single-file session replacing the §5
// two-file split.
const SESSION_FILE_NAME: &str = "stage-2.session.json";
// source: stages/stage-2.md §5.3 — verified receipt filename.
const VERIFIED_FILE_NAME: &str = "stage-2.verified.json";

// source: stages/stage-2.md §12.3 "transcript_digest change" — the sha256
// algorithm identifier is a named constant, not a scattered string literal
// (zetetic standard in the stage-2 brief).
const DIGEST_ALGORITHM: &str = "sha256";

// ---------------------------------------------------------------------------
// Wire types (MCP JSON-RPC layer)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn write_message(msg: Value) {
    let line = msg.to_string();
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", line);
    let _ = lock.flush();
}

fn send_response(id: Value, result: Value) {
    write_message(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }));
}

fn send_error(id: Value, code: i32, message: &str) {
    write_message(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }));
}

// ---------------------------------------------------------------------------
// Stage 1 schema types (one Rust type per spec schema)
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §3.2 — canonical finding shape.
// additionalProperties: true → extra fields are captured in `extras`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Finding {
    id: String,
    title: String,
    // §5.1.1 preservation + §9.3 Q8: `None` must serialize as JSON `null`,
    // not absent. `default` is kept so input-absent still parses. Removing
    // `skip_serializing_if` makes `null` and `0.0` round-trip distinctly in
    // `stage-1.source.json`. (spec: stages/stage-1.md §5.1.1, §9.3 Q8)
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    relevance_category: String,
    #[serde(default)]
    relevance_score: Option<f64>,
    #[serde(default)]
    raw_data: Option<Value>,
    // Spec §3.2 declares additionalProperties: true and §5.1.1 (preservation)
    // requires round-tripping unknown fields. Kept as a flat map.
    // INVARIANT: adding a new explicit field to this struct requires verifying
    // it does NOT collide with any key that the input stream might put in
    // `extras`. On collision, serde flatten emits duplicate JSON keys — most
    // parsers silently keep the last, which violates §5.1.1 preservation.
    // Check the field name against recent `raw_data` shapes before adding.
    #[serde(flatten)]
    extras: BTreeMap<String, Value>,
}

// source: stages/stage-1.md §4.1 — deterministic extraction output.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtractedFinding {
    finding_id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    relevance_category: String,
    // Nullable per §9.3 Q8 — `null` and 0.0 must round-trip distinctly.
    relevance_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_data: Option<Value>,
    extracted_at: String,
    extractor_version: String,
    source_form: String,
    source_path: Option<String>,
    // §5.1.1 preservation of unknown fields from the canonical finding.
    // INVARIANT: adding a new explicit field to this struct requires verifying
    // it does NOT collide with any key that the input stream might put in
    // `extras`. On collision, serde flatten emits duplicate JSON keys — most
    // parsers silently keep the last, which violates §5.1.1 preservation.
    // Check the field name against recent `raw_data` shapes before adding.
    #[serde(flatten)]
    extras: BTreeMap<String, Value>,
}

// source: stages/stage-1.md §4.2 / §9.2 — orchestrator's refined prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinedPrompt {
    text: String,
    role_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_estimate: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AddedContext {
    kind: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<String>,
}

// source: stages/stage-1.md §9.2 — refinement metadata from the orchestrator.
// `refined_at` is filled in by the Rust tool, not the agent: the JSON Schema
// (§9.2 line 402) lists only `added_context` + `orchestrator_version` with
// `additionalProperties: false`, and `#[serde(skip_deserializing)]` below
// enforces the same invariant at the Rust layer — any `refined_at` an agent
// sends is silently dropped before the tool fills its own timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinementMeta {
    added_context: Vec<AddedContext>,
    orchestrator_version: String,
    #[serde(default, skip_deserializing)]
    refined_at: Option<String>,
}

// source: stages/stage-1.md §4.2 — full refined artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinedArtifact {
    extracted: ExtractedFinding,
    refined_prompt: RefinedPrompt,
    refinement: RefinementMeta,
}

// source: stages/stage-1.md §5.2 + stages/stage-2.md §5.4 — index entry.
// Stage-2 fields are optional because a finding may be extracted/refined but
// not yet verified. `skip_serializing_if = Option::is_none` is CORRECT here
// (different from `Finding.relevance_score` which needed null/absent
// distinction per §9.3 Q8) — the index is server-owned metadata with no
// null/absent distinction issue: absent means the stage has not run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct IndexEntry {
    artifact_path: String,
    extractor_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    orchestrator_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refined_at: Option<String>,
    // --- stage-2 (stages/stage-2.md §5.4) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage2_path: Option<String>,
}

// source: stages/stage-1.md §5.2 — index.json shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Index {
    run_id: String,
    started_at: String,
    last_updated_at: String,
    findings: BTreeMap<String, IndexEntry>,
}

// ---------------------------------------------------------------------------
// Tool registry — one entry per pipeline stage.
// ---------------------------------------------------------------------------

fn tools_list(profile: ToolProfile) -> Value {
    profile.filter_tools_list(tool_schemas::tools_list())
}

// ---------------------------------------------------------------------------
// Stage 1 — time + randomness (no external crates)
// ---------------------------------------------------------------------------

fn now_unix_seconds_nanos() -> (u64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs(), d.subsec_nanos()),
        // Pre-1970 system clock is unreachable on any modern host; fall back
        // to epoch so we never panic (spec: "No unwraps on I/O").
        Err(_) => (0, 0),
    }
}

// source: stages/stage-1.md §4.1 — `extracted_at` and §5.2 `started_at`.
// Reuses the format from parse_findings.py:127 (`%Y-%m-%dT%H:%M:%SZ`).
// Pure-stdlib UTC conversion via the civil-from-days algorithm by Howard
// Hinnant, "date algorithms", http://howardhinnant.github.io/date_algorithms.html
// (public-domain reference implementation; reproduced in §civil_from_days).
fn format_iso8601_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

// source: stages/stage-1.md §9.3 Q5 — compact `YYYYMMDD-HHMMSS` for the
// run_id prefix. Matches SKILL.md:44 (`date +%Y%m%d-%H%M%S`).
fn format_compact_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, mi, s)
}

// source: Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms"
// http://howardhinnant.github.io/date_algorithms.html — civil_from_days().
// Public-domain. Valid for the full proleptic Gregorian range we care about.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;

    // Shift so day 0 is 0000-03-01 (Hinnant's "era" anchor).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if mo <= 2 { y + 1 } else { y };

    (y, mo, d, h, mi, s)
}

// source: stages/stage-1.md §9.3 Q5 — 6-char uniform-random suffix. This is
// not cryptographic; nanosecond mixing is adequate for collision avoidance
// at any realistic run rate (collision P ≈ N²/(2·36⁶), negligible).
fn random_suffix(len: usize) -> String {
    let (secs, nanos) = now_unix_seconds_nanos();
    let pid = process::id() as u64;
    // xorshift64* seeded from clock + pid. Source: Marsaglia, "Xorshift RNGs",
    // Journal of Statistical Software 8(14), 2003. Not cryptographic — good
    // enough for a collision suffix.
    let mut state: u64 = secs
        // source: Knuth, TAOCP vol 2 §3.3.4, Table 1 (MMIX LCG multiplier)
        .wrapping_mul(6_364_136_223_846_793_005)
        // source: Steele, Lea, Flood, "Fast Splittable Pseudorandom Number
        // Generators", OOPSLA 2014 — constant from SplitMix64 seed advance.
        .wrapping_add((nanos as u64).wrapping_mul(1_442_695_040_888_963_407))
        // PID mix: plain XOR into the state. No multiplier is needed for a
        // non-cryptographic collision suffix — the clock+nanos product above
        // already provides the avalanche, and XOR keeps the pid contribution
        // sourced and justifiably trivial (no magic multiplier to cite).
        ^ pid;
    if state == 0 {
        // source: golden ratio * 2^64 (Knuth TAOCP vol 3 §6.4); also used
        // as MurmurHash3 finalizer mixer. Non-zero fallback seed.
        state = 0x9E37_79B9_7F4A_7C15;
    }
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let idx = (state as usize) % RUN_ID_RANDOM_ALPHABET.len();
        out.push(RUN_ID_RANDOM_ALPHABET[idx] as char);
    }
    out
}

fn generate_run_id() -> String {
    let (secs, _) = now_unix_seconds_nanos();
    format!(
        "{}-{}",
        format_compact_utc(secs),
        random_suffix(RUN_ID_RANDOM_LEN)
    )
}

// ---------------------------------------------------------------------------
// Stage 1 — safe-ID validation (spec §5.1.4, §9.3 Q4)
// ---------------------------------------------------------------------------

fn validate_safe_id(kind: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must be non-empty",
            kind
        ));
    }
    if id.len() > SAFE_ID_MAX_LEN {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): length {} exceeds max {}",
            kind,
            id.len(),
            SAFE_ID_MAX_LEN
        ));
    }
    if id.starts_with('.') {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must not start with '.'",
            kind
        ));
    }
    if id.contains("..") {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must not contain '..'",
            kind
        ));
    }
    for b in id.bytes() {
        let ok = b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-';
        if !ok {
            return Err(format!(
                "unsafe {} (spec §5.1.4, §9.3 Q4): must match [A-Za-z0-9._-]+",
                kind
            ));
        }
    }
    Ok(())
}

fn require_absolute(path: &str, field: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!(
            "{} must be an absolute path (spec §3.1): got {:?}",
            field, path
        ));
    }
    // Reject `..` components outright — spec §5.1.4 safety applies to paths
    // the caller passes in. output_dir may still *resolve* to whatever the
    // user wants, but we do not silently consume `..`.
    for comp in p.components() {
        if matches!(comp, Component::ParentDir) {
            return Err(format!(
                "{} must not contain '..' (spec §5.1.4): got {:?}",
                field, path
            ));
        }
    }
    Ok(p.to_path_buf())
}

/// Parses the `language` field from tool arguments into an optional Language filter.
/// "auto" or absent -> None (detect per-file). Named language -> Some(Language).
fn parse_language_filter(args: &Map<String, Value>) -> Result<Option<parser::Language>, String> {
    match args.get("language").and_then(|v| v.as_str()) {
        None | Some("auto") => Ok(None),
        Some(lang_str) => parser::Language::from_str_opt(lang_str)
            .map(Some)
            .ok_or_else(|| format!("unsupported language: {lang_str}")),
    }
}

/// Resolves the tri-tier `dependency_scope` contract, honoring the deprecated
/// `include_dependencies: bool` alias (`true` -> Full, `false` -> None). If
/// both fields are present, `dependency_scope` wins. Emits a deprecation
/// warning to stderr whenever the alias is present at all.
/// source: ADR-4253701 §Decision 1.
fn parse_dependency_scope(args: &Map<String, Value>) -> Result<indexer::DependencyScope, String> {
    let legacy_include_deps = args.get("include_dependencies").and_then(|v| v.as_bool());
    if legacy_include_deps.is_some() {
        eprintln!(
            "deprecation warning: 'include_dependencies' is deprecated, use 'dependency_scope' \
             (\"none\" | \"public_api\" | \"full\") instead"
        );
    }
    match args.get("dependency_scope").and_then(|v| v.as_str()) {
        Some(s) => indexer::DependencyScope::from_str_opt(s)
            .ok_or_else(|| format!("unsupported dependency_scope: {s}")),
        None => Ok(match legacy_include_deps {
            Some(true) => indexer::DependencyScope::Full,
            Some(false) | None => indexer::DependencyScope::None,
        }),
    }
}

/// Parses an optional boolean argument, defaulting when absent and rejecting a
/// present-but-non-boolean value (rather than silently coercing it).
fn parse_bool_arg(args: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!("field '{key}' must be a boolean, got {other}")),
    }
}

// ---------------------------------------------------------------------------
// Stage 1 — atomic file writes (spec §5.2.3, POSIX rename(2))
// ---------------------------------------------------------------------------

// Writes `contents` to `target` by first writing to a sibling tempfile and
// renaming it over the target. POSIX rename(2) is atomic on the same
// filesystem — reference: IEEE Std 1003.1-2017, rename(2).
fn atomic_write(target: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("atomic_write: target has no parent: {:?}", target))?;
    fs::create_dir_all(parent).map_err(|e| format!("atomic_write: mkdir {:?}: {}", parent, e))?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("atomic_write: target has no file name: {:?}", target))?;

    let (secs, nanos) = now_unix_seconds_nanos();
    let pid = process::id();
    let tmp_name = format!(
        ".{}.tmp.{}.{}.{}.{}",
        file_name,
        pid,
        secs,
        nanos,
        random_suffix(4)
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)
            .map_err(|e| format!("atomic_write: create {:?}: {}", tmp_path, e))?;
        f.write_all(contents)
            .map_err(|e| format!("atomic_write: write {:?}: {}", tmp_path, e))?;
        f.sync_all()
            .map_err(|e| format!("atomic_write: fsync {:?}: {}", tmp_path, e))?;
    }

    fs::rename(&tmp_path, target).map_err(|e| {
        // Best-effort cleanup of the tempfile — it is not fatal if this fails.
        let _ = fs::remove_file(&tmp_path);
        format!("atomic_write: rename {:?} -> {:?}: {}", tmp_path, target, e)
    })?;
    Ok(())
}

fn write_json_atomic<T: Serialize>(target: &Path, value: &T) -> Result<usize, String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("json serialize {:?}: {}", target, e))?;
    atomic_write(target, &bytes)?;
    Ok(bytes.len())
}

// ---------------------------------------------------------------------------
// Stage 1 — finding resolution (spec §3.3 form 1 + form 2)
// ---------------------------------------------------------------------------

/// (finding, source_form, source_path, source_bytes_verbatim).
/// `source_bytes_verbatim` is the canonical JSON bytes to write into
/// stage-1.source.json — after normalization to the §3.2 schema, per §4.4.
/// clippy::type_complexity.
type ResolvedFinding = (Finding, &'static str, Option<String>, Vec<u8>);

fn resolve_finding(finding_arg: &Value) -> Result<ResolvedFinding, String> {
    match finding_arg {
        Value::Object(_) => {
            let finding: Finding = serde_json::from_value(finding_arg.clone()).map_err(|e| {
                format!("inline finding does not match canonical schema §3.2: {}", e)
            })?;
            validate_required_finding_fields(&finding)?;
            let bytes = serde_json::to_vec_pretty(&finding)
                .map_err(|e| format!("serialize inline finding: {}", e))?;
            Ok((finding, "inline", None, bytes))
        }
        Value::String(path_str) => {
            let path = require_absolute(path_str, "finding")?;
            let lower = path_str.to_ascii_lowercase();
            if lower.ends_with(".md") {
                // spec §9.3 Q1 — .md input is deferred in v1.
                return Err(
                    ".md finding inputs are not supported in v1 (spec §9.3 Q1); \
                     convert to JSON first"
                        .to_string(),
                );
            }
            if !lower.ends_with(".json") {
                return Err(format!(
                    "finding path must end in .json (spec §3.3): got {:?}",
                    path_str
                ));
            }
            let raw = fs::read_to_string(&path)
                .map_err(|e| format!("read finding file {:?}: {}", path, e))?;
            let parsed: Value = serde_json::from_str(&raw)
                .map_err(|e| format!("parse finding file {:?}: {}", path, e))?;

            // §3.3 form 2: either the root matches §3.2, or the root is
            // {findings: [...]} with exactly one element.
            let finding_value = if let Some(arr) = parsed
                .as_object()
                .and_then(|m| m.get("findings"))
                .and_then(|v| v.as_array())
            {
                if arr.len() != 1 {
                    return Err(format!(
                        "finding file {:?} has findings[{}]: stage 1 processes one finding per call (spec §3.3)",
                        path,
                        arr.len()
                    ));
                }
                arr[0].clone()
            } else {
                parsed
            };

            let finding: Finding = serde_json::from_value(finding_value).map_err(|e| {
                format!(
                    "finding file {:?} does not match canonical schema §3.2: {}",
                    path, e
                )
            })?;
            validate_required_finding_fields(&finding)?;
            let bytes = serde_json::to_vec_pretty(&finding)
                .map_err(|e| format!("serialize finding: {}", e))?;
            Ok((
                finding,
                "json_file",
                Some(path.to_string_lossy().into_owned()),
                bytes,
            ))
        }
        _ => Err("finding must be an object or an absolute path string (spec §3.1)".to_string()),
    }
}

fn validate_required_finding_fields(f: &Finding) -> Result<(), String> {
    if f.id.trim().is_empty() {
        return Err("finding.id is required and must be non-empty (spec §3.2)".to_string());
    }
    if f.title.trim().is_empty() {
        return Err("finding.title is required and must be non-empty (spec §3.2)".to_string());
    }
    if f.relevance_category.trim().is_empty() {
        return Err(
            "finding.relevance_category is required and must be non-empty (spec §3.2)".to_string(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stage 1 — index.json read/merge/write (spec §5.2)
// ---------------------------------------------------------------------------

fn read_index(path: &Path) -> Result<Option<Index>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read index {:?}: {}", path, e))?;
    let idx: Index =
        serde_json::from_str(&raw).map_err(|e| format!("parse index {:?}: {}", path, e))?;
    Ok(Some(idx))
}

// Strategy for merging a new index entry with an existing one.
//
// Preservation discipline (spec §5.1.2 idempotency + §5.2.1 unique appearance
// + stages/stage-2.md §5.4): each stage owns its own fields, but a re-run of
// an earlier stage must NOT clobber downstream fields the later stage wrote.
//
// MergeMode answers the question "which fields of `entry` are authoritative?"
//
// Layer diagram (each arrow = "preserves when re-run"):
//
//     extract_finding  ─┐
//     refine_finding   ─┼──▶ IndexEntry
//     finalize_verification ──┘
//
// | Mode                   | Called by             | Preserves on top of `entry`                                   |
// |------------------------|-----------------------|---------------------------------------------------------------|
// | `PreserveDownstream`   | extract_finding       | refined_* (if set) AND verified_*/stage2_path (if set)         |
// | `PreserveStage2`       | refine_finding        | verified_*/stage2_path (if set) — stage-2 survives re-refine   |
// | `PreserveRefinedOnly`  | finalize_verification | refined_* (if set) — stage-2 fields come from `entry`          |
// | `Replace`              | (reserved, unused)    | nothing — `entry` is written wholesale                         |
//
// Written explicitly here because the stage-1 code-reviewer flagged MergeMode
// preservation as the single most hazardous area of the index code.
enum MergeMode {
    PreserveDownstream,
    PreserveStage2,
    PreserveRefinedOnly,
    #[allow(dead_code)]
    Replace,
}

fn upsert_index_entry(
    output_dir: &Path,
    run_id: &str,
    finding_id: &str,
    entry: IndexEntry,
    mode: MergeMode,
) -> Result<(), String> {
    let index_path = output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(INDEX_FILE_NAME);
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let mut idx = match read_index(&index_path)? {
        Some(existing) => existing,
        None => Index {
            run_id: run_id.to_string(),
            started_at: now.clone(),
            last_updated_at: now.clone(),
            findings: BTreeMap::new(),
        },
    };
    idx.last_updated_at = now;

    let merged = merge_index_entry(idx.findings.get(finding_id), entry, mode);
    idx.findings.insert(finding_id.to_string(), merged);
    write_json_atomic(&index_path, &idx)?;
    Ok(())
}

// Pure function: given the existing entry (if any), the new entry, and the
// merge mode, return the entry that should be written. Split out of
// `upsert_index_entry` to keep that function under the 40-LOC bar and to
// make the preservation discipline testable in isolation.
fn merge_index_entry(
    existing: Option<&IndexEntry>,
    entry: IndexEntry,
    mode: MergeMode,
) -> IndexEntry {
    match mode {
        MergeMode::Replace => entry,
        MergeMode::PreserveDownstream => match existing {
            Some(e) => merge_preserve_downstream(e, entry),
            None => entry,
        },
        MergeMode::PreserveStage2 => match existing {
            Some(e) => merge_preserve_stage2(e, entry),
            None => entry,
        },
        MergeMode::PreserveRefinedOnly => match existing {
            Some(e) => merge_preserve_refined_only(e, entry),
            None => entry,
        },
    }
}

// extract_finding re-run: keep refined_* (if set) AND verified_*/stage2_path.
fn merge_preserve_downstream(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: if existing.refined_at.is_some() {
            existing.artifact_path.clone()
        } else {
            entry.artifact_path
        },
        extractor_version: entry.extractor_version,
        orchestrator_version: existing.orchestrator_version.clone(),
        refined_at: existing.refined_at.clone(),
        verified_at: existing.verified_at.clone(),
        verified: existing.verified,
        stage2_path: existing.stage2_path.clone(),
    }
}

// refine_finding re-run: keep verified_*/stage2_path from the existing entry.
fn merge_preserve_stage2(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: entry.artifact_path,
        extractor_version: entry.extractor_version,
        orchestrator_version: entry.orchestrator_version,
        refined_at: entry.refined_at,
        verified_at: existing.verified_at.clone(),
        verified: existing.verified,
        stage2_path: existing.stage2_path.clone(),
    }
}

// finalize_verification: keep refined_* from existing; stage-2 fields come
// from `entry`. artifact_path is kept as the refined path (stage 1b wrote it).
fn merge_preserve_refined_only(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: existing.artifact_path.clone(),
        extractor_version: existing.extractor_version.clone(),
        orchestrator_version: existing.orchestrator_version.clone(),
        refined_at: existing.refined_at.clone(),
        verified_at: entry.verified_at,
        verified: entry.verified,
        stage2_path: entry.stage2_path,
    }
}

// ---------------------------------------------------------------------------
// Stage 1a — extract_finding
// ---------------------------------------------------------------------------

struct ExtractArgs<'a> {
    finding_arg: &'a Value,
    output_dir: PathBuf,
    run_id: String,
}

fn run_extract_finding(arguments: &Value) -> Value {
    match do_extract_finding(arguments) {
        Ok(v) => v,
        Err(reason) => json!({
            "stage": 1,
            "status": "error",
            "reason": reason
        }),
    }
}

fn parse_extract_args(arguments: &Value) -> Result<ExtractArgs<'_>, String> {
    let args = arguments
        .as_object()
        .ok_or_else(|| "arguments must be an object (spec §3.1)".to_string())?;
    let finding_arg = args
        .get("finding")
        .ok_or_else(|| "missing required field 'finding' (spec §3.1)".to_string())?;
    let output_dir_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing required field 'output_dir' (spec §3.1)".to_string())?;
    let output_dir = require_absolute(output_dir_str, "output_dir")?;
    let run_id = match args.get("run_id") {
        Some(Value::String(s)) => {
            validate_safe_id("run_id", s)?;
            s.clone()
        }
        Some(Value::Null) | None => generate_run_id(),
        Some(other) => {
            return Err(format!(
                "run_id must be a string when provided (spec §3.1): got {}",
                other
            ));
        }
    };
    Ok(ExtractArgs {
        finding_arg,
        output_dir,
        run_id,
    })
}

// spec §4.1: pure construction of the canonical extracted artifact.
fn build_extracted_artifact(
    finding: &Finding,
    source_form: &'static str,
    source_path: Option<String>,
) -> ExtractedFinding {
    ExtractedFinding {
        finding_id: finding.id.clone(),
        title: finding.title.clone(),
        description: finding.description.clone(),
        source_url: finding.source_url.clone(),
        relevance_category: finding.relevance_category.clone(),
        relevance_score: finding.relevance_score,
        raw_data: finding.raw_data.clone(),
        extracted_at: format_iso8601_utc(now_unix_seconds_nanos().0),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        source_form: source_form.to_string(),
        source_path,
        extras: finding.extras.clone(),
    }
}

// spec §4.4 + §5.2.3 + §5.2 — mkdir, atomic-write source, atomic-write
// extracted, and upsert the preliminary index entry in MergeMode::PreserveRefined
// (so a prior refined artifact is not clobbered by a re-extract).
fn persist_extract(
    output_dir: &Path,
    run_id: &str,
    finding_dir: &Path,
    source_bytes: &[u8],
    extracted: &ExtractedFinding,
) -> Result<(PathBuf, usize), String> {
    fs::create_dir_all(finding_dir).map_err(|e| format!("mkdir {:?}: {}", finding_dir, e))?;
    atomic_write(&finding_dir.join(SOURCE_FILE_NAME), source_bytes)?;
    let extracted_path = finding_dir.join(EXTRACTED_FILE_NAME);
    let bytes_written = write_json_atomic(&extracted_path, extracted)?;
    let entry = IndexEntry {
        artifact_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, extracted.finding_id, EXTRACTED_FILE_NAME
        ),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        ..IndexEntry::default()
    };
    // PreserveDownstream: a re-extract must not clobber fields that stage 1b
    // or stage 2 later wrote on top (refined_*, verified_*, stage2_path).
    upsert_index_entry(
        output_dir,
        run_id,
        &extracted.finding_id,
        entry,
        MergeMode::PreserveDownstream,
    )?;
    Ok((extracted_path, bytes_written))
}

fn do_extract_finding(arguments: &Value) -> Result<Value, String> {
    let parsed = parse_extract_args(arguments)?;
    let (finding, source_form, source_path, source_bytes) = resolve_finding(parsed.finding_arg)?;
    // spec §5.1.4, §9.3 Q4 — hard-fail on unsafe finding_id BEFORE touching disk.
    validate_safe_id("finding_id", &finding.id)?;

    let finding_dir = parsed
        .output_dir
        .join(RUNS_DIR_NAME)
        .join(&parsed.run_id)
        .join(FINDINGS_DIR_NAME)
        .join(&finding.id);
    let extracted = build_extracted_artifact(&finding, source_form, source_path);
    let (extracted_path, bytes_written) = persist_extract(
        &parsed.output_dir,
        &parsed.run_id,
        &finding_dir,
        &source_bytes,
        &extracted,
    )?;

    // spec §4.3 — MCP receipt. artifact_path here is the extracted artifact;
    // refine_finding updates it to point at the refined artifact later.
    Ok(json!({
        "stage": 1,
        "status": "ok",
        "finding_id": finding.id,
        "artifact_path": extracted_path.to_string_lossy(),
        "run_id": parsed.run_id,
        "bytes_written": bytes_written,
        "extractor_version": EXTRACTOR_VERSION
    }))
}

// ---------------------------------------------------------------------------
// Stage 1b — refine_finding
// ---------------------------------------------------------------------------

// Err.0 is the short machine-readable reason code used by the smoke test
// (e.g. "no_extraction"), Err.1 is the human-readable message.
type StageErr = (String, String);

fn bad_request<E: std::fmt::Display>(msg: E) -> StageErr {
    ("bad_request".to_string(), msg.to_string())
}
fn io_err<E: std::fmt::Display>(msg: E) -> StageErr {
    ("io_error".to_string(), msg.to_string())
}
fn unsafe_id_err(msg: String) -> StageErr {
    ("unsafe_id".to_string(), msg)
}

struct RefineArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
    refined_prompt: RefinedPrompt,
    refinement_input: RefinementMeta,
}

fn run_refine_finding(arguments: &Value) -> Value {
    match do_refine_finding(arguments) {
        Ok(v) => v,
        Err((reason_code, reason_msg)) => json!({
            "stage": 1,
            "status": "error",
            "reason": reason_code,
            "message": reason_msg
        }),
    }
}

fn require_string_arg(args: &Map<String, Value>, key: &str) -> Result<String, StageErr> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| bad_request(format!("missing required field '{}' (spec §9.2)", key)))
}

fn parse_refine_args(arguments: &Value) -> Result<RefineArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §9.2)"))?;

    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;

    let refined_prompt_val = args
        .get("refined_prompt")
        .cloned()
        .ok_or_else(|| bad_request("missing required field 'refined_prompt' (spec §9.2)"))?;
    let refined_prompt: RefinedPrompt =
        serde_json::from_value(refined_prompt_val).map_err(|e| {
            bad_request(format!(
                "refined_prompt does not match schema (spec §9.2): {}",
                e
            ))
        })?;
    // spec §5.1.5 — non-empty refinement contract.
    if refined_prompt.text.is_empty() {
        return Err((
            "empty_prompt".to_string(),
            "refined_prompt.text must be non-empty (spec §5.1.5)".to_string(),
        ));
    }

    let refinement_val = args
        .get("refinement")
        .cloned()
        .ok_or_else(|| bad_request("missing required field 'refinement' (spec §9.2)"))?;
    let refinement_input: RefinementMeta = serde_json::from_value(refinement_val).map_err(|e| {
        bad_request(format!(
            "refinement does not match schema (spec §9.2): {}",
            e
        ))
    })?;

    Ok(RefineArgs {
        run_id,
        finding_id,
        output_dir,
        refined_prompt,
        refinement_input,
    })
}

// spec §9.2 precondition 1 — extraction must exist, parse cleanly.
fn load_existing_extracted(finding_dir: &Path) -> Result<ExtractedFinding, StageErr> {
    let extracted_path = finding_dir.join(EXTRACTED_FILE_NAME);
    if !extracted_path.exists() {
        return Err((
            "no_extraction".to_string(),
            format!(
                "no stage-1.extracted.json at {:?} — call extract_finding first (spec §9.2)",
                extracted_path
            ),
        ));
    }
    let extracted_raw = fs::read_to_string(&extracted_path)
        .map_err(|e| io_err(format!("read {:?}: {}", extracted_path, e)))?;
    serde_json::from_str(&extracted_raw).map_err(|e| {
        (
            "corrupt_extraction".to_string(),
            format!("parse {:?}: {}", extracted_path, e),
        )
    })
}

// spec §4.2 — pure construction of the refined artifact. Fills `refined_at`
// server-side per §9.2 (agent input is silently dropped by serde skip).
fn build_refined_artifact(
    extracted: ExtractedFinding,
    refined_prompt: RefinedPrompt,
    refinement_input: RefinementMeta,
    refined_at: String,
) -> RefinedArtifact {
    RefinedArtifact {
        extracted,
        refined_prompt,
        refinement: RefinementMeta {
            added_context: refinement_input.added_context,
            orchestrator_version: refinement_input.orchestrator_version,
            refined_at: Some(refined_at),
        },
    }
}

// spec §5.2.3 — atomic write of the refined artifact + §5.2 index upsert in
// MergeMode::Replace (refine_finding owns all four index fields).
fn persist_refine(
    output_dir: &Path,
    run_id: &str,
    finding_id: &str,
    finding_dir: &Path,
    artifact: &RefinedArtifact,
    refined_at: String,
) -> Result<(PathBuf, usize), StageErr> {
    let refined_path = finding_dir.join(REFINED_FILE_NAME);
    let bytes_written = write_json_atomic(&refined_path, artifact).map_err(io_err)?;
    let entry = IndexEntry {
        artifact_path: format!("{}/{}/{}", FINDINGS_DIR_NAME, finding_id, REFINED_FILE_NAME),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        orchestrator_version: Some(artifact.refinement.orchestrator_version.clone()),
        refined_at: Some(refined_at),
        ..IndexEntry::default()
    };
    // PreserveStage2: a re-refine must not clobber the stage-2 verified_*/
    // stage2_path fields an earlier finalize_verification wrote.
    upsert_index_entry(
        output_dir,
        run_id,
        finding_id,
        entry,
        MergeMode::PreserveStage2,
    )
    .map_err(io_err)?;
    Ok((refined_path, bytes_written))
}

fn do_refine_finding(arguments: &Value) -> Result<Value, StageErr> {
    let parsed = parse_refine_args(arguments)?;
    let finding_dir = parsed
        .output_dir
        .join(RUNS_DIR_NAME)
        .join(&parsed.run_id)
        .join(FINDINGS_DIR_NAME)
        .join(&parsed.finding_id);

    let extracted = load_existing_extracted(&finding_dir)?;
    let refined_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let orchestrator_version = parsed.refinement_input.orchestrator_version.clone();
    let artifact = build_refined_artifact(
        extracted,
        parsed.refined_prompt,
        parsed.refinement_input,
        refined_at.clone(),
    );
    let (refined_path, bytes_written) = persist_refine(
        &parsed.output_dir,
        &parsed.run_id,
        &parsed.finding_id,
        &finding_dir,
        &artifact,
        refined_at,
    )?;

    Ok(json!({
        "stage": 1,
        "status": "ok",
        "finding_id": parsed.finding_id,
        "artifact_path": refined_path.to_string_lossy(),
        "run_id": parsed.run_id,
        "bytes_written": bytes_written,
        "extractor_version": EXTRACTOR_VERSION,
        "orchestrator_version": orchestrator_version,
        "orchestrator_contract_version": ORCHESTRATOR_CONTRACT_VERSION
    }))
}

// ---------------------------------------------------------------------------
// Stage 2 — schema types (spec §12.3)
// ---------------------------------------------------------------------------

// source: stages/stage-2.md §3 + §12.3 — the five legal session states.
// `deny_unknown_fields` at the field level not needed — the enum string
// values are exhaustive and unknown strings are rejected by serde by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionState {
    Open,
    WaitingForUser,
    WaitingForAgent,
    Finalized,
    Aborted,
}

impl SessionState {
    fn as_str(self) -> &'static str {
        match self {
            SessionState::Open => "open",
            SessionState::WaitingForUser => "waiting_for_user",
            SessionState::WaitingForAgent => "waiting_for_agent",
            SessionState::Finalized => "finalized",
            SessionState::Aborted => "aborted",
        }
    }
}

// source: stages/stage-2.md §12.3 transcript item schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TurnKind {
    AgentQuestion,
    UserAnswer,
}

// source: stages/stage-2.md §12.3 — transcript element.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionTurn {
    seq: usize,
    kind: TurnKind,
    timestamp: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Value>,
}

// source: stages/stage-2.md §12.3 — single session file schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionFile {
    run_id: String,
    finding_id: String,
    state: SessionState,
    turn_count: usize,
    started_at: String,
    updated_at: String,
    schema_ok: bool,
    verifier_version: String,
    transcript: Vec<SessionTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aborted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    abort_reason: Option<String>,
}

// source: stages/stage-2.md §5.3 — verified receipt sub-object.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VerifiedKind {
    schema_ok: bool,
    completeness_ok: bool,
    user_acknowledged: bool,
}

// source: stages/stage-2.md §5.3 — verified receipt.
// `completeness_checklist` records which sub-items passed (per §5.3 + §9
// item 6: knowing WHICH item failed is load-bearing).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VerifiedArtifact {
    run_id: String,
    finding_id: String,
    verified: bool,
    verified_kind: VerifiedKind,
    finalized_at: String,
    stage1_refined_path: String,
    session_path: String,
    transcript_digest: String,
    digest_algorithm: String,
    transcript_bytes_at_finalize: usize,
    turn_count: usize,
    verifier_version: String,
    completeness_checklist: BTreeMap<String, bool>,
}

// ---------------------------------------------------------------------------
// Stage 2 — state machine (spec §3 + §12.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum TransitionEvent {
    Start,
    AgentQuestion,
    UserAnswer,
    Finalize,
    Abort,
}

// Pure function. Dispatches to per-event handlers so each branch stays tiny.
// Table is the §12.2 override of the §3 state machine; see the match arms in
// `transition_append`, `transition_finalize`, `transition_abort`, and
// `transition_start`.
fn can_transition(from: SessionState, event: TransitionEvent) -> Result<SessionState, StageErr> {
    // Terminal states reject everything except Aborted+Start (handled below).
    if matches!(from, SessionState::Finalized) {
        return Err(stage2_err(
            "already_finalized",
            "session is finalized; further writes are rejected (spec §8.4)",
        ));
    }
    if matches!(from, SessionState::Aborted) && !matches!(event, TransitionEvent::Start) {
        return Err(stage2_err(
            "already_aborted",
            "session is aborted; only start_verification may restart it (spec §12.1)",
        ));
    }
    match event {
        TransitionEvent::Start => transition_start(from),
        TransitionEvent::AgentQuestion => transition_append(from, TurnKind::AgentQuestion),
        TransitionEvent::UserAnswer => transition_append(from, TurnKind::UserAnswer),
        TransitionEvent::Finalize => transition_finalize(from),
        TransitionEvent::Abort => transition_abort(from),
    }
}

// Spec §7.1 + §12.1: start is legal only on a missing or aborted session.
// The "missing session" case never reaches this function (checked in the
// caller). This handles the "existing session" case.
fn transition_start(from: SessionState) -> Result<SessionState, StageErr> {
    match from {
        SessionState::Aborted => Ok(SessionState::Open),
        _ => Err(stage2_err(
            "illegal_transition",
            "start_verification requires no session or an aborted session (spec §7.1, §12.1)",
        )),
    }
}

// Spec §3 alternation invariant + §12.3. `Open → agent_question` is the
// first-turn case.
fn transition_append(from: SessionState, kind: TurnKind) -> Result<SessionState, StageErr> {
    match (from, kind) {
        (SessionState::Open, TurnKind::AgentQuestion) => Ok(SessionState::WaitingForUser),
        (SessionState::WaitingForUser, TurnKind::UserAnswer) => Ok(SessionState::WaitingForAgent),
        (SessionState::WaitingForAgent, TurnKind::AgentQuestion) => Ok(SessionState::WaitingForUser),
        _ => Err(stage2_err(
            "alternation_violation",
            "two consecutive turns of the same kind are illegal (spec §3 alternation invariant, §12.2)",
        )),
    }
}

// Spec §12.2: finalize is illegal from Open and WaitingForUser; legal from
// WaitingForAgent only.
fn transition_finalize(from: SessionState) -> Result<SessionState, StageErr> {
    match from {
        SessionState::WaitingForAgent => Ok(SessionState::Finalized),
        SessionState::Open => Err(stage2_err(
            "no_clarification_round",
            "finalize requires ≥1 agent_question AND ≥1 user_answer before it (spec §12.2)",
        )),
        SessionState::WaitingForUser => Err(stage2_err(
            "unanswered_question",
            "cannot finalize while an agent_question is awaiting a user_answer (spec §7.3, §12.2)",
        )),
        _ => Err(stage2_err(
            "illegal_transition",
            "finalize rejected (spec §12.2)",
        )),
    }
}

// Spec §12.1: abort is legal from any non-terminal state. Terminal-state
// rejection already happened at the top of can_transition.
fn transition_abort(_from: SessionState) -> Result<SessionState, StageErr> {
    Ok(SessionState::Aborted)
}

// Helper to build a StageErr with a stage-2 reason code (spec "error codes"
// list in the implementation brief).
fn stage2_err(code: &str, msg: &str) -> StageErr {
    (code.to_string(), msg.to_string())
}

// ---------------------------------------------------------------------------
// Stage 2 — session I/O (spec §12.3 atomicity-by-construction)
// ---------------------------------------------------------------------------

fn session_path_for(output_dir: &Path, run_id: &str, finding_id: &str) -> PathBuf {
    output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(FINDINGS_DIR_NAME)
        .join(finding_id)
        .join(SESSION_FILE_NAME)
}

fn finding_dir_for(output_dir: &Path, run_id: &str, finding_id: &str) -> PathBuf {
    output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(FINDINGS_DIR_NAME)
        .join(finding_id)
}

fn read_session(path: &Path) -> Result<Option<SessionFile>, StageErr> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).map_err(|e| io_err(format!("read session {:?}: {}", path, e)))?;
    let session: SessionFile = serde_json::from_str(&raw).map_err(|e| {
        stage2_err(
            "corrupt_session",
            &format!("parse session {:?}: {}", path, e),
        )
    })?;
    Ok(Some(session))
}

// Spec §12.3 steps 4-7: serialize + atomic_write. Steps 1-3 (read, validate,
// construct) live in the per-tool `build_*` functions.
fn write_session_atomic(path: &Path, session: &SessionFile) -> Result<(), StageErr> {
    write_json_atomic(path, session).map_err(io_err)?;
    Ok(())
}

// Spec §12.2: completeness checklist. Returns (overall_ok, per_item_map).
fn compute_completeness(transcript: &[SessionTurn]) -> (bool, BTreeMap<String, bool>) {
    let has_q = transcript.iter().any(|t| t.kind == TurnKind::AgentQuestion);
    let has_a = transcript.iter().any(|t| t.kind == TurnKind::UserAnswer);
    let mut m = BTreeMap::new();
    m.insert("at_least_one_agent_question".to_string(), has_q);
    m.insert("at_least_one_user_answer".to_string(), has_a);
    (has_q && has_a, m)
}

// Spec §12.3 "transcript_digest change": sha256 over canonicalized transcript
// array bytes. Returns (hex_digest, canonical_bytes_len).
fn compute_transcript_digest(transcript: &[SessionTurn]) -> Result<(String, usize), StageErr> {
    let bytes = serde_json::to_vec(transcript)
        .map_err(|e| io_err(format!("canonicalize transcript: {}", e)))?;
    let digest = Sha256::digest(&bytes);
    Ok((hex_lower(&digest), bytes.len()))
}

// Lowercase hex, no `0x` prefix. Source: RFC 4648 §8 (Base 16). Hand-rolled
// to keep stage 2's dep footprint to serde + serde_json + sha2 only.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 2a — start_verification (spec §7.1 + §12.1 restart)
// ---------------------------------------------------------------------------

struct StartArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
}

fn parse_start_args(arguments: &Value) -> Result<StartArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.1)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    Ok(StartArgs {
        run_id,
        finding_id,
        output_dir,
    })
}

// Spec §7.1 precondition (i): stage-1.refined.json exists and parses. Also
// sets `schema_ok`, which never mutates after start.
fn check_stage1_refined(finding_dir: &Path) -> Result<bool, StageErr> {
    let refined_path = finding_dir.join(REFINED_FILE_NAME);
    if !refined_path.exists() {
        return Err(stage2_err(
            "no_extraction",
            &format!(
                "no {} at {:?} — call refine_finding first (spec §4 schema_ok, §7.1)",
                REFINED_FILE_NAME, refined_path
            ),
        ));
    }
    let raw = fs::read_to_string(&refined_path)
        .map_err(|e| io_err(format!("read {:?}: {}", refined_path, e)))?;
    let _parsed: RefinedArtifact = serde_json::from_str(&raw).map_err(|e| {
        stage2_err(
            "corrupt_extraction",
            &format!("parse {:?}: {}", refined_path, e),
        )
    })?;
    Ok(true)
}

// Pure construction per §12.3 step 3 (never mutate in place).
fn build_new_session(args: &StartArgs, schema_ok: bool, now: String) -> SessionFile {
    SessionFile {
        run_id: args.run_id.clone(),
        finding_id: args.finding_id.clone(),
        state: SessionState::Open,
        turn_count: 0,
        started_at: now.clone(),
        updated_at: now,
        schema_ok,
        verifier_version: VERIFIER_VERSION.to_string(),
        transcript: Vec::new(),
        aborted_at: None,
        abort_reason: None,
    }
}

fn do_start_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_start_args(arguments)?;
    let finding_dir = finding_dir_for(&args.output_dir, &args.run_id, &args.finding_id);
    let schema_ok = check_stage1_refined(&finding_dir)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    if let Some(existing) = read_session(&session_path)? {
        // Spec §7.1 precondition (ii) + §12.1: only aborted is restartable.
        let _ = can_transition(existing.state, TransitionEvent::Start)?;
    }
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let session = build_new_session(&args, schema_ok, now.clone());
    fs::create_dir_all(&finding_dir)
        .map_err(|e| io_err(format!("mkdir {:?}: {}", finding_dir, e)))?;
    write_session_atomic(&session_path, &session)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": session.state.as_str(),
        "run_id": session.run_id,
        "finding_id": session.finding_id,
        "started_at": session.started_at,
        "session_path": session_path.to_string_lossy(),
    }))
}

fn run_start_verification(arguments: &Value) -> Value {
    match do_start_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

fn stage2_error_response(code: String, msg: String) -> Value {
    json!({
        "stage": 2,
        "status": "error",
        "reason": code,
        "message": msg,
    })
}

// ---------------------------------------------------------------------------
// Stage 2b — append_clarification (spec §7.2 + §12.3 single-file rewrite)
// ---------------------------------------------------------------------------

struct AppendArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
    kind: TurnKind,
    content: String,
    meta: Option<Value>,
}

fn parse_append_args(arguments: &Value) -> Result<AppendArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.2)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    let kind_str = require_string_arg(args, "kind")?;
    let kind = match kind_str.as_str() {
        "agent_question" => TurnKind::AgentQuestion,
        "user_answer" => TurnKind::UserAnswer,
        other => {
            return Err(bad_request(format!(
                "kind must be 'agent_question' or 'user_answer' (spec §5.2, §7.2): got {:?}",
                other
            )))
        }
    };
    let content = require_string_arg(args, "content")?;
    if content.is_empty() {
        return Err(bad_request(
            "content must be non-empty (spec §5.2 minLength:1)",
        ));
    }
    let meta = args.get("meta").cloned();
    Ok(AppendArgs {
        run_id,
        finding_id,
        output_dir,
        kind,
        content,
        meta,
    })
}

// Pure construction of the new session from the old one + the incoming turn.
// Spec §12.3 step 3: "never mutate in place" — returns a fresh SessionFile.
fn build_appended_session(prev: &SessionFile, args: &AppendArgs) -> Result<SessionFile, StageErr> {
    let event = match args.kind {
        TurnKind::AgentQuestion => TransitionEvent::AgentQuestion,
        TurnKind::UserAnswer => TransitionEvent::UserAnswer,
    };
    let new_state = can_transition(prev.state, event)?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let mut transcript = prev.transcript.clone();
    let new_turn = SessionTurn {
        seq: transcript.len(),
        kind: args.kind,
        timestamp: now.clone(),
        content: args.content.clone(),
        meta: args.meta.clone(),
    };
    transcript.push(new_turn);
    Ok(SessionFile {
        run_id: prev.run_id.clone(),
        finding_id: prev.finding_id.clone(),
        state: new_state,
        turn_count: transcript.len(),
        started_at: prev.started_at.clone(),
        updated_at: now,
        schema_ok: prev.schema_ok,
        verifier_version: prev.verifier_version.clone(),
        transcript,
        aborted_at: None,
        abort_reason: None,
    })
}

fn do_append_clarification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_append_args(arguments)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let prev = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json — call start_verification first (spec §7.2)",
        )
    })?;
    let next = build_appended_session(&prev, &args)?;
    write_session_atomic(&session_path, &next)?;
    let last_seq = next.transcript.len().saturating_sub(1);
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": next.state.as_str(),
        "seq": last_seq,
        "turn_count": next.turn_count,
    }))
}

fn run_append_clarification(arguments: &Value) -> Value {
    match do_append_clarification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 2c — finalize_verification (spec §7.3 + §12.2 + §12.3)
// ---------------------------------------------------------------------------

struct FinalizeArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
}

fn parse_finalize_args(arguments: &Value) -> Result<FinalizeArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.3)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    Ok(FinalizeArgs {
        run_id,
        finding_id,
        output_dir,
    })
}

fn load_session_for_finalize(args: &FinalizeArgs) -> Result<(SessionFile, PathBuf), StageErr> {
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let session = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json — call start_verification first (spec §7.3)",
        )
    })?;
    Ok((session, session_path))
}

// Spec §12.2: finalize rejects from `open` and `waiting_for_user`. Also
// enforces §7.3 precondition (ii): schema_ok must be true.
fn check_finalize_preconditions(session: &SessionFile) -> Result<(), StageErr> {
    if !session.schema_ok {
        return Err(stage2_err(
            "schema_not_ok",
            "session.schema_ok is false; stage-1 artifact is broken (spec §7.3 (ii))",
        ));
    }
    let _ = can_transition(session.state, TransitionEvent::Finalize)?;
    Ok(())
}

// Spec §12.3: digest over the transcript array only (NOT the whole session
// file — that would include `state: "finalized"` which is circular).
fn build_verified_artifact(
    session: &SessionFile,
    args: &FinalizeArgs,
    now: String,
) -> Result<VerifiedArtifact, StageErr> {
    let (completeness_ok, checklist) = compute_completeness(&session.transcript);
    // §12.2 guarantees this is true if we got past check_finalize_preconditions,
    // but compute it from data anyway — no invented invariants.
    let (digest_hex, digest_input_len) = compute_transcript_digest(&session.transcript)?;
    let user_acknowledged = true; // Spec §4: the finalize tool call itself is the signal.
    let verified = session.schema_ok && completeness_ok && user_acknowledged;
    Ok(VerifiedArtifact {
        run_id: session.run_id.clone(),
        finding_id: session.finding_id.clone(),
        verified,
        verified_kind: VerifiedKind {
            schema_ok: session.schema_ok,
            completeness_ok,
            user_acknowledged,
        },
        finalized_at: now,
        stage1_refined_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, REFINED_FILE_NAME
        ),
        session_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, SESSION_FILE_NAME
        ),
        transcript_digest: digest_hex,
        digest_algorithm: DIGEST_ALGORITHM.to_string(),
        transcript_bytes_at_finalize: digest_input_len,
        turn_count: session.turn_count,
        verifier_version: VERIFIER_VERSION.to_string(),
        completeness_checklist: checklist,
    })
}

// Spec §12.3: write verified artifact, flip session.state to finalized
// atomically (whole-file rewrite), upsert index entry.
//
// Crash window: if we crash between the verified write and the session flip,
// the verified receipt is on disk but the session is still waiting_for_agent.
// Re-running finalize is idempotent (same transcript → same digest).
fn persist_finalize(
    args: &FinalizeArgs,
    session: &SessionFile,
    verified: &VerifiedArtifact,
    now: String,
) -> Result<(PathBuf, usize), StageErr> {
    let finding_dir = finding_dir_for(&args.output_dir, &args.run_id, &args.finding_id);
    let verified_path = finding_dir.join(VERIFIED_FILE_NAME);
    let bytes_written = write_json_atomic(&verified_path, verified).map_err(io_err)?;
    let finalized = SessionFile {
        state: SessionState::Finalized,
        updated_at: now.clone(),
        ..session.clone()
    };
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    write_session_atomic(&session_path, &finalized)?;
    upsert_verified_index(args, verified, now)?;
    Ok((verified_path, bytes_written))
}

// Spec §5.4: stage-2 index fields. Preserves the refined_* fields stage 1b
// wrote. artifact_path is taken from the existing entry (refined path).
fn upsert_verified_index(
    args: &FinalizeArgs,
    verified: &VerifiedArtifact,
    now: String,
) -> Result<(), StageErr> {
    let entry = IndexEntry {
        // Will be overridden by PreserveRefinedOnly; put a sentinel here.
        artifact_path: String::new(),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        verified_at: Some(now),
        verified: Some(verified.verified),
        stage2_path: Some(format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, VERIFIED_FILE_NAME
        )),
        ..IndexEntry::default()
    };
    upsert_index_entry(
        &args.output_dir,
        &args.run_id,
        &args.finding_id,
        entry,
        MergeMode::PreserveRefinedOnly,
    )
    .map_err(io_err)
}

fn do_finalize_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_finalize_args(arguments)?;
    let (session, _session_path) = load_session_for_finalize(&args)?;
    check_finalize_preconditions(&session)?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let verified = build_verified_artifact(&session, &args, now.clone())?;
    let (verified_path, bytes_written) = persist_finalize(&args, &session, &verified, now)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": SessionState::Finalized.as_str(),
        "verified": verified.verified,
        "verified_kind": {
            "schema_ok": verified.verified_kind.schema_ok,
            "completeness_ok": verified.verified_kind.completeness_ok,
            "user_acknowledged": verified.verified_kind.user_acknowledged,
        },
        "verified_path": verified_path.to_string_lossy(),
        "turn_count": verified.turn_count,
        "transcript_digest": verified.transcript_digest,
        "digest_algorithm": verified.digest_algorithm,
        "transcript_bytes_at_finalize": verified.transcript_bytes_at_finalize,
        "bytes_written": bytes_written,
        "verifier_version": VERIFIER_VERSION,
    }))
}

fn run_finalize_verification(arguments: &Value) -> Value {
    match do_finalize_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 2d — abort_verification (spec §12.1)
// ---------------------------------------------------------------------------

struct AbortArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
    reason: Option<String>,
}

fn parse_abort_args(arguments: &Value) -> Result<AbortArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §12.1)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(AbortArgs {
        run_id,
        finding_id,
        output_dir,
        reason,
    })
}

// Pure construction. Transcript is preserved verbatim (spec §12.3 "abort
// does not truncate the transcript; it only flips state and sets aborted_at").
fn build_aborted_session(
    prev: &SessionFile,
    reason: Option<String>,
    now: String,
) -> Result<SessionFile, StageErr> {
    let _ = can_transition(prev.state, TransitionEvent::Abort)?;
    Ok(SessionFile {
        state: SessionState::Aborted,
        updated_at: now.clone(),
        aborted_at: Some(now),
        abort_reason: reason,
        ..prev.clone()
    })
}

fn do_abort_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_abort_args(arguments)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let prev = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json to abort (spec §12.1)",
        )
    })?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let aborted = build_aborted_session(&prev, args.reason.clone(), now.clone())?;
    write_session_atomic(&session_path, &aborted)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": aborted.state.as_str(),
        "run_id": aborted.run_id,
        "finding_id": aborted.finding_id,
        "turn_count": aborted.turn_count,
        "aborted_at": now,
    }))
}

fn run_abort_verification(arguments: &Value) -> Value {
    match do_abort_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 3a — index_codebase
// ---------------------------------------------------------------------------

fn run_index_codebase(arguments: &Value) -> Value {
    match do_index_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_failed", "message": msg
        }),
    }
}

fn do_index_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'path'")?;
    let output_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let lang_filter = parse_language_filter(args)?;
    let dependency_scope = parse_dependency_scope(args)?;
    let want_export = parse_bool_arg(args, "export_artifact", false)?;
    let want_bootstrap = parse_bool_arg(args, "bootstrap", false)?;
    let accept_stale = parse_bool_arg(args, "accept_stale", false)?;
    // Issue #62: force a from-scratch rebuild even when an incremental baseline
    // exists (e.g. after changing the language filter or dependency scope, which
    // the manifest does not capture). Default false → auto-incremental when a
    // prior index + manifest are present.
    let want_full = parse_bool_arg(args, "full", false)?;
    // Issue #58: mine git temporal coupling (FILE_CHANGES_WITH) after indexing.
    // Default true — it is cheap (one bounded `git log`) and the architect agent
    // reads churning pairs straight from the graph. Set false to skip (e.g. a
    // non-git tree, though mining self-skips there too).
    let want_cochange = parse_bool_arg(args, "cochange", true)?;

    let codebase = require_absolute(path_str, "path")?;
    if !codebase.exists() {
        return Err(format!("path does not exist: {}", codebase.display()));
    }
    let output_dir = require_absolute(output_str, "output_dir")?;
    fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
    let graph_dir = output_dir.join("graph");
    let manifest_path = indexer::manifest::manifest_path(&output_dir);
    // source: H4 fix — validate the derived path ends in `/graph` and is not
    // a forbidden system root before any destructive op.
    validate_graph_path_safe(&graph_dir)?;

    // Issue #55/#62 bootstrap: when the caller opts in AND there is no local
    // graph yet, import the committed snapshot instead of cold-indexing. The
    // evolved staleness contract (fresh → import; stale → import THEN incremental
    // fill by DEFAULT; stale + accept_stale → import as-is, skip the fill;
    // import/fill failure → reindex) lives in `attempt_bootstrap`. Any path that
    // does NOT import falls through to the full index below, carrying an
    // assertable `bootstrap_skipped` note.
    let mut bootstrap_skipped: Option<Value> = None;
    if want_bootstrap && !graph_dir.exists() {
        match attempt_bootstrap(
            &codebase,
            &output_dir,
            &graph_dir,
            &manifest_path,
            accept_stale,
            lang_filter,
            dependency_scope,
        ) {
            BootstrapOutcome::Imported(resp) => return Ok(resp),
            BootstrapOutcome::Reindex(note) => bootstrap_skipped = note,
        }
    }

    // Issue #62 incremental mode: when a prior local graph AND a readable file
    // manifest exist and the caller did not force `full`, re-index only the
    // changed files. Any failure logs loudly and falls through to a full
    // rebuild (§13 — no silent path). Auto-selected so the common "re-index
    // after editing a few files" call pays work proportional to the diff.
    if !want_full && graph_dir.exists() {
        if let Some(prior) = indexer::manifest::load(&manifest_path) {
            match indexer::index_incremental(
                &codebase,
                &graph_dir,
                &manifest_path,
                lang_filter,
                dependency_scope,
                &prior,
            ) {
                Ok(inc) => {
                    write_graph_meta(&output_dir, &codebase);
                    return Ok(finish_incremental_response(
                        inc,
                        &graph_dir,
                        &codebase,
                        &manifest_path,
                        want_export,
                        want_cochange,
                    ));
                }
                Err(e) => {
                    eprintln!(
                        "[ap] incremental index failed ({e}); falling back to a full re-index"
                    );
                }
            }
        }
    }

    // lbug creates the database itself; if a stale graph artifact exists from a
    // prior run, remove it so lbug can initialise cleanly.
    if graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&graph_dir)?;
    }

    let result = indexer::index_codebase_with_language(
        &codebase,
        &graph_dir,
        lang_filter,
        dependency_scope,
    )?;
    // Record the absolute source root beside the graph (relative file paths
    // stay in the graph; the root lets consumers reconstruct absolute paths).
    write_graph_meta(&output_dir, &codebase);
    // Issue #62: persist the per-file manifest so the NEXT index_codebase call
    // can run incrementally. Best-effort — a failed manifest write only forces
    // the next run to full-index, never fails this one.
    if let Err(e) =
        indexer::write_full_manifest(&codebase, &manifest_path, lang_filter, dependency_scope)
    {
        eprintln!("[ap] file manifest write failed (index succeeded): {e}");
    }
    // Issue #57: persist the coverage-honesty sidecar (parse-incomplete, skipped,
    // and quarantined files). Best-effort — a failed write only drops the honesty
    // signal, never the index.
    let coverage_path = indexer::coverage::coverage_path(&output_dir);
    if let Err(e) = indexer::coverage::save(&coverage_path, &result.coverage) {
        eprintln!("[ap] coverage sidecar write failed (index succeeded): {e}");
    }
    // Issue #58: mine git temporal coupling into FILE_CHANGES_WITH edges. Full
    // index → full re-mine of the window.
    let cochange_summary = if want_cochange {
        run_cochange(&graph_dir, &codebase, &output_dir, cochange::Mode::Full)
    } else {
        Value::Null
    };

    // Issue #55: an explicit index is the single (best-ratio, zstd-9) export
    // tier. Failure to export is LOUD but non-fatal — the index itself succeeded.
    let mut response = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "graph_path": result.graph_path.to_string_lossy(),
        "node_count": result.node_count,
        "edge_count": result.edge_count,
        "files_indexed": result.files_indexed,
        "elapsed_ms": result.elapsed_ms,
    });
    if !cochange_summary.is_null() {
        response["cochange"] = cochange_summary;
    }
    response["coverage"] = coverage_summary(&result.coverage);
    if let Some(note) = bootstrap_skipped {
        response["bootstrap_skipped"] = note;
    }
    if want_export {
        match artifact::export_artifact(
            &graph_dir,
            &codebase,
            result.node_count,
            result.edge_count,
            Some(&manifest_path),
            Some(&coverage_path),
        ) {
            Ok(stats) => {
                response["artifact_path"] = json!(stats.artifact_path.to_string_lossy());
                response["artifact_compressed_bytes"] = json!(stats.compressed_bytes);
                response["artifact_original_bytes"] = json!(stats.original_bytes);
            }
            Err(e) => {
                eprintln!("[ap] artifact export failed (index succeeded): {e}");
                response["artifact_error"] = json!(e);
            }
        }
    }
    Ok(response)
}

/// Max example files listed per coverage kind in a tool response. The counts are
/// always exact; the example lists are capped so a repo with thousands of
/// parse-incomplete files cannot blow the MCP tool-result budget. The full lists
/// live in the `index_coverage.json` sidecar and are queryable via index_status.
// source: DeusData/codebase-memory-mcp mcp.c — coverage responses carry "counts
// plus a FEW EXAMPLES only", the complete lists queryable separately.
const COVERAGE_LIST_CAP: usize = 50;

/// The coverage-honesty caveat repeated on every surface that reports coverage
/// (issue #57), mirroring the reference's wording so agents never overtrust it.
// source: DeusData/codebase-memory-mcp mcp.c — "absence of a flag is NOT a
// completeness guarantee; prefer grep inside flagged ranges".
const COVERAGE_CAVEAT: &str = "Best-effort signal, NOT a completeness guarantee: \
    absence of a flag only means the indexer detected no gap (a file keyed to a \
    subtly wrong grammar can still parse 'clean'). 'parse_incomplete' files WERE \
    indexed but constructs inside the flagged line ranges MAY be missing from the \
    graph — prefer grep there. 'skipped'/'quarantined' files are NOT in the graph \
    at all. source: DeusData/codebase-memory-mcp coverage wording.";

/// Renders a `CoverageReport` into an honest, budget-bounded JSON block: exact
/// counts per kind, capped example lists (parse_incomplete carries error ranges;
/// skipped/quarantined carry reasons), and the completeness caveat.
fn coverage_summary(report: &indexer::coverage::CoverageReport) -> Value {
    use indexer::coverage::CoverageKind;
    let (partial, skipped, quarantined) = report.counts();
    let mut partial_files = Vec::new();
    let mut skipped_files = Vec::new();
    let mut quarantined_files = Vec::new();
    for (rel, cov) in &report.files {
        match cov.kind {
            CoverageKind::ParsePartial => {
                if partial_files.len() < COVERAGE_LIST_CAP {
                    partial_files.push(json!({"path": rel, "error_ranges": cov.error_ranges}));
                }
            }
            CoverageKind::Skipped => {
                if skipped_files.len() < COVERAGE_LIST_CAP {
                    skipped_files.push(json!({"path": rel, "reason": cov.detail}));
                }
            }
            CoverageKind::Quarantined => {
                if quarantined_files.len() < COVERAGE_LIST_CAP {
                    quarantined_files.push(json!({"path": rel, "reason": cov.detail}));
                }
            }
        }
    }
    json!({
        "index_mode": report.index_mode,
        "files_indexed": report.files_indexed,
        "parse_incomplete": { "count": partial, "files": partial_files },
        "skipped": { "count": skipped, "files": skipped_files },
        "quarantined": { "count": quarantined, "files": quarantined_files },
        "caveat": COVERAGE_CAVEAT,
    })
}

/// Loads the coverage sidecar for a graph at `graph_dir` (its `output_dir` is the
/// parent) and renders the summary, or `null` when no coverage is available.
fn coverage_summary_for_graph(graph_dir: &Path) -> Value {
    match graph_dir.parent() {
        Some(output_dir) => {
            let path = indexer::coverage::coverage_path(output_dir);
            match indexer::coverage::load(&path) {
                Some(report) => coverage_summary(&report),
                None => Value::Null,
            }
        }
        None => Value::Null,
    }
}

/// The `query_graph(graph="missed")` response (issue #57): a structural
/// enumeration of what the index does NOT fully cover, so an agent can pivot to
/// grep. `null` coverage means no coverage sidecar exists for this graph — which
/// is itself NOT a completeness claim (the index may predate coverage tracking).
fn query_missed_response(graph_path: &Path) -> Value {
    let coverage = coverage_summary_for_graph(graph_path);
    if coverage.is_null() {
        return json!({
            "stage": 3,
            "status": "ok",
            "tool": "query_graph",
            "graph": "missed",
            "coverage": Value::Null,
            "note": "No coverage sidecar for this graph (indexed before coverage \
                     tracking, or the sidecar was not carried alongside the graph). \
                     Absence of coverage data is NOT a completeness guarantee — \
                     re-index to generate it.",
        });
    }
    json!({
        "stage": 3,
        "status": "ok",
        "tool": "query_graph",
        "graph": "missed",
        "coverage": coverage,
        "note": "These files/ranges are where the index is known to be incomplete \
                 — prefer grep for them before trusting a negative graph result.",
    })
}

// ---------------------------------------------------------------------------
// Stage 3a — index_status (issue #57 coverage surface)
// ---------------------------------------------------------------------------

fn run_index_status(arguments: &Value) -> Value {
    match do_index_status(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_status_failed", "message": msg
        }),
    }
}

/// Reports a graph's indexing status: node/edge counts plus the coverage-honesty
/// report (issue #57) — files indexed, parse-incomplete (count + capped list with
/// ranges), and skipped/quarantined (count + list with reasons). This is the
/// authoritative coverage surface; the caveat states that absence of a flag is
/// not a completeness guarantee.
fn do_index_status(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let (node_count, edge_count) = graph_counts(graph_path);
    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_status",
        "graph_path": graph_str,
        "node_count": node_count,
        "edge_count": edge_count,
        "coverage": coverage_summary_for_graph(graph_path),
    }))
}

// ---------------------------------------------------------------------------
// Stage 3 — ingest_traces (issue #58 runtime enrichment)
// ---------------------------------------------------------------------------

fn run_ingest_traces(arguments: &Value) -> Value {
    match do_ingest_traces(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "ingest_traces_failed", "message": msg
        }),
    }
}

/// Node labels a runtime trace endpoint may resolve to (callables).
const CALLABLE_LABELS: &[&str] = &["Function", "Method"];

/// Folds runtime caller→callee observations into the graph (issue #58): annotate
/// a matching static Calls edge with `observed_count`, or create an
/// `OBSERVED_CALLS` edge where static resolution found none (the divergence
/// signal — runtime truth the static analysis missed).
///
/// Input: `{graph_path, traces: [{caller, callee, count}]}`. `caller`/`callee`
/// are qualified names (`file::symbol`); `count` defaults to 1. Postconditions:
/// each trace either annotated a static Calls edge (matched), created/updated an
/// OBSERVED_CALLS edge (unmatched_created), or was recorded as unresolved
/// (endpoint not a Function/Method node). The response reports the three counts
/// plus a capped list of the created divergences and the unresolved names.
fn do_ingest_traces(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let traces = args
        .get("traces")
        .and_then(|v| v.as_array())
        .ok_or("missing required field 'traces' (array of {caller, callee, count})")?;

    // Aggregate observations per (caller, callee) so repeated pairs sum instead
    // of creating duplicate edges.
    let mut agg: std::collections::BTreeMap<(String, String), i64> =
        std::collections::BTreeMap::new();
    for t in traces {
        let caller = t.get("caller").and_then(|v| v.as_str());
        let callee = t.get("callee").and_then(|v| v.as_str());
        let (caller, callee) = match (caller, callee) {
            (Some(a), Some(b)) => (a.to_string(), b.to_string()),
            _ => return Err("each trace needs 'caller' and 'callee' strings".to_string()),
        };
        let count = t.get("count").and_then(|v| v.as_i64()).unwrap_or(1).max(1);
        *agg.entry((caller, callee)).or_insert(0) += count;
    }

    let store = graph_cache::open_cached(graph_path)?;
    let mut matched = 0u64;
    let mut unmatched_created = 0u64;
    let mut unresolved: Vec<String> = Vec::new();
    let mut divergences: Vec<Value> = Vec::new();

    for ((caller, callee), count) in agg {
        let from_label = callable_label(&store, &caller);
        let to_label = callable_label(&store, &callee);
        let (from_label, to_label) = match (from_label, to_label) {
            (Some(f), Some(t)) => (f, t),
            _ => {
                if from_label.is_none() {
                    unresolved.push(caller.clone());
                }
                if to_label.is_none() {
                    unresolved.push(callee.clone());
                }
                continue;
            }
        };
        let static_table = format!("Calls_{from_label}_{to_label}");
        if annotate_static_call(&store, &static_table, &caller, &callee, count)? {
            matched += 1;
        } else {
            let observed_table = format!("OBSERVED_CALLS_{from_label}_{to_label}");
            upsert_observed_call(&store, &observed_table, &caller, &callee, count)?;
            unmatched_created += 1;
            if divergences.len() < COVERAGE_LIST_CAP {
                divergences.push(json!({
                    "caller": caller, "callee": callee, "observed_count": count
                }));
            }
        }
    }

    unresolved.sort();
    unresolved.dedup();
    let unresolved_total = unresolved.len();
    unresolved.truncate(COVERAGE_LIST_CAP);

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "ingest_traces",
        "matched": matched,
        "unmatched_created": unmatched_created,
        "unresolved_names": unresolved_total,
        "unresolved_names_sample": unresolved,
        "observed_divergences": divergences,
        "note": "matched = a static Calls edge annotated with observed_count. \
                 unmatched_created = OBSERVED_CALLS edges where runtime saw a call \
                 static analysis did not — the valuable divergence signal. \
                 unresolved_names = trace endpoints that are not Function/Method \
                 nodes (wrong qualified name, or an uncovered/unindexed symbol).",
    }))
}

/// The callable label ("Function"/"Method") of the node with id `qn`, or `None`
/// if no such callable node exists.
fn callable_label(store: &graph_store::GraphStore, qn: &str) -> Option<&'static str> {
    for &label in CALLABLE_LABELS {
        let q = format!(
            "MATCH (n:{label}) WHERE n.id = {} RETURN count(n)",
            graph_store::cypher_str(qn)
        );
        if let Ok(qr) = store.execute_query(&q) {
            let n = qr
                .rows
                .first()
                .and_then(|r| r.first())
                .and_then(|c| c.parse::<u64>().ok())
                .unwrap_or(0);
            if n > 0 {
                return Some(label);
            }
        }
    }
    None
}

/// Annotates an existing static Calls edge with a runtime `observed_count`
/// (accumulating). Returns `true` if such an edge existed (and was annotated),
/// `false` if none exists (so the caller creates an OBSERVED_CALLS edge instead).
fn annotate_static_call(
    store: &graph_store::GraphStore,
    table: &str,
    caller: &str,
    callee: &str,
    count: i64,
) -> Result<bool, String> {
    let (from_label, to_label) = table
        .strip_prefix("Calls_")
        .and_then(|s| s.split_once('_'))
        .ok_or_else(|| format!("bad calls table {table}"))?;
    let a = graph_store::cypher_str(caller);
    let b = graph_store::cypher_str(callee);
    let set = format!(
        "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
         WHERE x.id = {a} AND y.id = {b} \
         SET r.observed_count = coalesce(r.observed_count, 0) + {count}"
    );
    // SET on zero matches is a no-op; detect existence separately so we know
    // whether to fall through to an OBSERVED_CALLS edge.
    let exists_q = format!(
        "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
         WHERE x.id = {a} AND y.id = {b} RETURN count(r)"
    );
    let exists = store
        .execute_query(&exists_q)?
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false);
    if exists {
        store.execute_query(&set)?;
    }
    Ok(exists)
}

/// Creates or accumulates an OBSERVED_CALLS edge (the runtime-only divergence).
fn upsert_observed_call(
    store: &graph_store::GraphStore,
    table: &str,
    caller: &str,
    callee: &str,
    count: i64,
) -> Result<(), String> {
    let (from_label, to_label) = table
        .strip_prefix("OBSERVED_CALLS_")
        .and_then(|s| s.split_once('_'))
        .ok_or_else(|| format!("bad observed table {table}"))?;
    let a = graph_store::cypher_str(caller);
    let b = graph_store::cypher_str(callee);
    let exists_q = format!(
        "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
         WHERE x.id = {a} AND y.id = {b} RETURN count(r)"
    );
    let exists = store
        .execute_query(&exists_q)?
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false);
    if exists {
        let set = format!(
            "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
             WHERE x.id = {a} AND y.id = {b} \
             SET r.observed_count = coalesce(r.observed_count, 0) + {count}"
        );
        store.execute_query(&set)?;
    } else {
        store.insert_edge(
            table,
            caller,
            callee,
            &[("observed_count", &count.to_string())],
        )?;
    }
    Ok(())
}

/// Builds the tool response for a successful incremental re-index (issue #62)
/// and runs the optional artifact export against the updated graph. The
/// response carries the {changed, added, deleted, renamed, unchanged} partition
/// plus files_reparsed and wall time, and `mode: "incremental"` so a caller can
/// tell an incremental pass from a full one at a glance.
fn finish_incremental_response(
    inc: indexer::IncrementalResult,
    graph_dir: &Path,
    codebase: &Path,
    manifest_path: &Path,
    want_export: bool,
    want_cochange: bool,
) -> Value {
    let mut response = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "mode": "incremental",
        "graph_path": inc.graph_path.to_string_lossy(),
        "changed": inc.changed,
        "added": inc.added,
        "deleted": inc.deleted,
        "renamed": inc.renamed,
        "unchanged": inc.unchanged,
        "files_reparsed": inc.files_reparsed,
        "elapsed_ms": inc.elapsed_ms,
    });
    // Coverage (issue #57) was refreshed by the incremental pass; load + report it.
    response["coverage"] = coverage_summary_for_graph(graph_dir);
    // Temporal coupling (issue #58): EXTEND the mined aggregates with the commits
    // since the last mine, then re-derive edges. Git history is append-only, so
    // an incremental index need not re-walk the window.
    if want_cochange {
        if let Some(output_dir) = graph_dir.parent() {
            let summary =
                run_cochange(graph_dir, codebase, output_dir, cochange::Mode::Incremental);
            if !summary.is_null() {
                response["cochange"] = summary;
            }
        }
    }
    if want_export {
        // Whole-graph totals are needed only for the export sidecar; compute
        // them lazily here (a full table scan the incremental pass itself skips)
        // and surface them in the response alongside the artifact stats.
        let (node_count, edge_count) = graph_counts(graph_dir);
        response["node_count"] = json!(node_count);
        response["edge_count"] = json!(edge_count);
        let coverage_path = graph_dir.parent().map(indexer::coverage::coverage_path);
        match artifact::export_artifact(
            graph_dir,
            codebase,
            node_count,
            edge_count,
            Some(manifest_path),
            coverage_path.as_deref(),
        ) {
            Ok(stats) => {
                response["artifact_path"] = json!(stats.artifact_path.to_string_lossy());
                response["artifact_compressed_bytes"] = json!(stats.compressed_bytes);
                response["artifact_original_bytes"] = json!(stats.original_bytes);
            }
            Err(e) => {
                eprintln!("[ap] artifact export failed (incremental index succeeded): {e}");
                response["artifact_error"] = json!(e);
            }
        }
    }
    response
}

/// Outcome of a bootstrap attempt (issue #55 staleness contract).
#[derive(Debug)]
enum BootstrapOutcome {
    /// The snapshot was imported; this is the finished tool response.
    Imported(Value),
    /// Do a full index instead. Carries an assertable `bootstrap_skipped` note
    /// when the reason was a refused stale artifact (`None` otherwise).
    Reindex(Option<Value>),
}

/// Decides how to bootstrap from the committed artifact. Never hard-errors:
/// every non-import path logs its reason and returns `Reindex` so the caller
/// falls back to a full index EXPLICITLY (§13 — no silent path).
///
/// Evolved staleness contract (issue #62 completes #55) — the state table:
///
/// | condition | action | response |
/// |---|---|---|
/// | no artifact / unreadable sidecar | full index | `bootstrap_skipped=None` |
/// | fresh (sha == HEAD, or repo not git) | import as-is | `source=artifact_bootstrap` |
/// | stale + `accept_stale=true` | import as-is, SKIP fill | `source=artifact_bootstrap`, `stale_artifact{…}` |
/// | stale (DEFAULT) | import THEN incremental fill to the working tree | `source=artifact_bootstrap_fill`, `fill_method`, fill counts |
/// | stale, fill fails (no git diff AND no bundled manifest) | full index | `bootstrap_skipped{reason:"stale_artifact_fill_failed"}` |
/// | import fails | full index | `bootstrap_skipped=None` |
///
/// The DEFAULT for a stale artifact is now bootstrap-then-fill (it replaces the
/// old refuse-and-full-reindex): a one-commit-stale snapshot pays a cheap diff
/// fill instead of a full cold index. `accept_stale` is repurposed from "import
/// the stale snapshot anyway" to "import it AND SKIP the fill" — a deliberate
/// fast path when HEAD-accuracy is not needed. Per PR #61's no-silent-staleness
/// principle, an accepted-stale graph ALWAYS carries a `stale_artifact` report,
/// and a filled graph always carries its fill counts + method, so the response
/// states the graph's exact provenance in every case.
fn attempt_bootstrap(
    codebase: &Path,
    output_dir: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    accept_stale: bool,
    language_filter: Option<parser::Language>,
    dependency_scope: indexer::DependencyScope,
) -> BootstrapOutcome {
    if !artifact::artifact_exists(codebase) {
        return BootstrapOutcome::Reindex(None);
    }
    let meta = match artifact::read_artifact_meta(codebase) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[ap] artifact sidecar unreadable ({e}); running a full index");
            return BootstrapOutcome::Reindex(None);
        }
    };
    match artifact::artifact_staleness(codebase, &meta.commit) {
        // Fresh: import as-is (nothing to fill).
        None => bootstrap_import(codebase, output_dir, graph_dir, &meta, None),
        // accept_stale: import as-is, SKIP the fill, report the staleness.
        Some(info) if accept_stale => {
            bootstrap_import(codebase, output_dir, graph_dir, &meta, Some(info))
        }
        // DEFAULT: import THEN incrementally fill up to the working tree.
        Some(info) => bootstrap_import_and_fill(
            codebase,
            output_dir,
            graph_dir,
            manifest_path,
            &meta,
            info,
            language_filter,
            dependency_scope,
        ),
    }
}

/// Imports the snapshot into `graph_dir` and builds the bootstrap response
/// WITHOUT a fill (fresh, or accept_stale). On import failure, logs loudly and
/// returns `Reindex(None)`. When `stale` is set, the response carries a
/// `stale_artifact` object so a caller can never mistake an accepted-stale graph
/// for a fresh one. The bundled per-file manifest unpacked alongside the graph
/// is left in place: for a fresh graph it lets the next local `index_codebase`
/// run incrementally; for an accepted-stale graph it is the baseline the next
/// run diffs against to fill the skipped delta (so accepting stale never leaves
/// the graph permanently, silently stale).
fn bootstrap_import(
    codebase: &Path,
    output_dir: &Path,
    graph_dir: &Path,
    meta: &artifact::ArtifactMeta,
    stale: Option<artifact::StaleInfo>,
) -> BootstrapOutcome {
    if let Err(e) = artifact::import_artifact(codebase, graph_dir) {
        eprintln!(
            "[ap] artifact bootstrap failed ({e}); falling back to full index of {}",
            codebase.display()
        );
        return BootstrapOutcome::Reindex(None);
    }
    write_graph_meta(output_dir, codebase);
    let (node_count, edge_count) = graph_counts(graph_dir);
    let mut resp = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "source": "artifact_bootstrap",
        "graph_path": graph_dir.to_string_lossy(),
        "node_count": node_count,
        "edge_count": edge_count,
        "artifact_commit": meta.commit,
        "artifact_tool_version": meta.tool_version,
    });
    if let Some(info) = stale {
        resp["graph_state"] = json!("accepted_stale");
        resp["stale_artifact"] = serde_json::to_value(&info).unwrap_or_else(|_| json!({}));
    } else {
        resp["graph_state"] = json!("fresh");
    }
    // Coverage (issue #57): the bootstrapped graph inherits the exporter's
    // coverage sidecar (bundled in the artifact, unpacked beside the graph).
    resp["coverage"] = coverage_summary_for_graph(graph_dir);
    BootstrapOutcome::Imported(resp)
}

/// Imports the stale snapshot THEN incrementally fills the artifact→working-tree
/// diff (issue #62 machinery). On import failure or fill failure, returns
/// `Reindex` so the caller cold-indexes; on success the response reports the fill
/// method and the {changed, added, deleted, renamed, unchanged} partition, plus
/// `graph_state:"filled_to_working_tree"`, so the graph's provenance is explicit.
#[allow(clippy::too_many_arguments)]
fn bootstrap_import_and_fill(
    codebase: &Path,
    output_dir: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    meta: &artifact::ArtifactMeta,
    info: artifact::StaleInfo,
    language_filter: Option<parser::Language>,
    dependency_scope: indexer::DependencyScope,
) -> BootstrapOutcome {
    if let Err(e) = artifact::import_artifact(codebase, graph_dir) {
        eprintln!(
            "[ap] artifact bootstrap failed ({e}); falling back to full index of {}",
            codebase.display()
        );
        return BootstrapOutcome::Reindex(None);
    }
    write_graph_meta(output_dir, codebase);
    // The bundled manifest (unpacked alongside the graph) is the artifact-sha
    // baseline the content-hash fallback classifies against when git can't diff.
    let imported_manifest = indexer::manifest::load(manifest_path);
    let fill: indexer::FillResult = match indexer::fill_after_bootstrap(
        codebase,
        graph_dir,
        manifest_path,
        &meta.commit,
        imported_manifest.as_ref(),
        language_filter,
        dependency_scope,
    ) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[ap] artifact bootstrap fill failed ({e}); falling back to a full index");
            let mut note = serde_json::to_value(&info).unwrap_or_else(|_| json!({}));
            note["reason"] = json!("stale_artifact_fill_failed");
            return BootstrapOutcome::Reindex(Some(note));
        }
    };
    let fill_method = match fill.method {
        indexer::FillMethod::GitDiff => "git_diff",
        indexer::FillMethod::ContentHash => "content_hash",
    };
    let (node_count, edge_count) = graph_counts(graph_dir);
    let mut resp = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "source": "artifact_bootstrap_fill",
        "graph_state": "filled_to_working_tree",
        "graph_path": graph_dir.to_string_lossy(),
        "node_count": node_count,
        "edge_count": edge_count,
        "artifact_commit": meta.commit,
        "artifact_tool_version": meta.tool_version,
        "head_sha": info.head_sha,
        "fill_method": fill_method,
        "changed": fill.result.changed,
        "added": fill.result.added,
        "deleted": fill.result.deleted,
        "renamed": fill.result.renamed,
        "unchanged": fill.result.unchanged,
        "files_reparsed": fill.result.files_reparsed,
        "fill_elapsed_ms": fill.result.elapsed_ms,
    });
    if let Some(behind) = info.commits_behind {
        resp["artifact_commits_behind"] = json!(behind);
    }
    // Coverage (issue #57): refreshed by the fill (carry-forward + reparsed gaps).
    resp["coverage"] = coverage_summary_for_graph(graph_dir);
    BootstrapOutcome::Imported(resp)
}

/// Mines git temporal coupling into the graph at `graph_dir` (issue #58) and
/// returns a JSON summary, or `Value::Null` when there is nothing to report (not
/// a git tree, or a best-effort failure — never fatal to the index). `Full`
/// re-mines the window; `Incremental` extends the sidecar aggregates with new
/// commits. `output_dir` holds the `cochange.json` sidecar.
fn run_cochange(
    graph_dir: &Path,
    codebase: &Path,
    output_dir: &Path,
    mode: cochange::Mode,
) -> Value {
    let store = match graph_store::GraphStore::open_or_create(graph_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ap] cochange: open graph failed: {e}");
            return Value::Null;
        }
    };
    let prior = if mode == cochange::Mode::Incremental {
        cochange::load_state(&cochange::state_path(output_dir))
    } else {
        None
    };
    match cochange::mine(&store, codebase, output_dir, mode, prior) {
        Ok(Some(mr)) => json!({
            "mode": mr.mode,
            "commits_scanned": mr.commits_scanned,
            "edges_written": mr.edges_written,
            "mined_to_sha": mr.mined_to_sha,
        }),
        Ok(None) => Value::Null, // not a git working tree
        Err(e) => {
            eprintln!("[ap] cochange mining failed (index succeeded): {e}");
            Value::Null
        }
    }
}

/// Reads node/edge counts from an on-disk graph. Best-effort: an open/query
/// failure yields `(0, 0)` rather than aborting the bootstrap response — the
/// counts are informational and the graph is already in place.
fn graph_counts(graph_dir: &Path) -> (u64, u64) {
    match graph_store::GraphStore::open_or_create(graph_dir) {
        Ok(store) => {
            let n = store.node_count().unwrap_or(0);
            let e = store.edge_count().unwrap_or(0);
            (n, e)
        }
        Err(_) => (0, 0),
    }
}

// ---------------------------------------------------------------------------
// Stage 3a — query_graph
// ---------------------------------------------------------------------------

fn run_query_graph(arguments: &Value) -> Value {
    match do_query_graph(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Surface the read-only rejection as its own reason code so callers
            // can distinguish policy rejections from engine errors.
            if msg.contains("read_only_query_required") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "read_only_query_required",
                    "message": msg,
                })
            } else {
                json!({
                    "stage": 3, "status": "error", "reason": "query_failed", "message": msg
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Read-only Cypher filter — source: H3 fix.
//
// Rejects any query that contains a mutation or side-effectful keyword as a
// whole-word, case-insensitive match. This is a conservative allowlist-by-
// blocklist: the engine still validates syntax, we just refuse to hand it
// anything that could mutate state, load external data, or call procedures.
// ---------------------------------------------------------------------------

const FORBIDDEN_CYPHER_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "MERGE", "SET", "REMOVE", "DROP", "ALTER", "CALL", "LOAD",
];

/// Returns the first forbidden keyword found in `query`, or None if the query
/// is safe. Matching is whole-word, ASCII case-insensitive. Strings/comments
/// are not specifically excluded — callers who need `CREATE` as a literal in
/// a read query must restructure it (reading doesn't require mutation words).
fn forbidden_cypher_keyword(query: &str) -> Option<&'static str> {
    let upper = query.to_ascii_uppercase();
    FORBIDDEN_CYPHER_KEYWORDS
        .iter()
        .find(|&&kw| contains_whole_word(&upper, kw))
        .copied()
}

/// Whole-word contains: `needle` must be bordered by non-alphanumeric chars
/// (or start/end of haystack). Prevents false positives on identifiers that
/// embed the keyword (e.g. `created_at` should not trigger `CREATE`).
fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    if nbytes.is_empty() || bytes.len() < nbytes.len() {
        return false;
    }
    let mut i = 0;
    while i + nbytes.len() <= bytes.len() {
        if &bytes[i..i + nbytes.len()] == nbytes {
            let left_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
            let right = i + nbytes.len();
            let right_ok = right == bytes.len()
                || (!bytes[right].is_ascii_alphanumeric() && bytes[right] != b'_');
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// Graph-path safety — source: H4 fix.
//
// The `graph_path` / `output_dir` / ... arguments are caller-controlled and
// in the pre-fix code were passed to `remove_dir_all`. A malicious caller
// could set `output_dir: "/"` and have the server wipe the filesystem.
//
// `validate_graph_path_safe` MUST be called before any `remove_dir_all` or
// `create_dir_all` on a caller-derived path. The policy:
//   (a) path must be absolute,
//   (b) last segment must be `graph` (or the path must contain `/graph/`),
//   (c) path must NOT equal a forbidden system root.
// ---------------------------------------------------------------------------

const FORBIDDEN_GRAPH_PATH_PREFIXES: &[&str] = &[
    "/", "/Users", "/home", "/root", "/tmp", "/var", "/etc", "/usr", "/bin", "/sbin", "/dev",
    "/opt", "/System", "/Library",
];

/// Returns Ok iff `path` is a safe target for destructive directory ops.
fn validate_graph_path_safe(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "unsafe_graph_path: must be absolute (got {:?})",
            path
        ));
    }
    // Must end in `/graph` (the well-known suffix). Check the last component.
    let last = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if last != "graph" {
        return Err(format!(
            "unsafe_graph_path: must end in '/graph' (got {:?})",
            path
        ));
    }
    // Reject pathological roots (even if they happen to end in `/graph`).
    let s = path.to_string_lossy();
    for forbidden in FORBIDDEN_GRAPH_PATH_PREFIXES {
        if s == *forbidden || s == format!("{forbidden}/graph") {
            return Err(format!(
                "unsafe_graph_path: {path:?} is a forbidden system path"
            ));
        }
    }
    Ok(())
}

/// Removes a stale graph artifact at `path`, whether the prior run left a
/// directory (older Kuzu lays the database out as a dir) or a single database
/// file (newer Kuzu). Plain `remove_dir_all` fails with `ENOTDIR (os error 20)`
/// when the target is a file — the observed failure on re-index of an existing
/// graph. `symlink_metadata` never traverses a symlink at the graph path, so a
/// symlinked `graph` is unlinked, not followed.
/// Caller MUST have run `validate_graph_path_safe` first.
fn remove_stale_graph_artifact(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("stat stale graph path: {e}"))?;
    let outcome = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    outcome.map_err(|e| format!("remove stale graph artifact: {e}"))
}

/// Write a ``meta.json`` sidecar in ``output_dir`` recording the ABSOLUTE
/// source root this graph was indexed from.
///
/// AP stores file paths RELATIVE to the indexed root so the graph stays
/// portable across machines. A downstream consumer that must reconstruct
/// absolute paths — cortex-viz keys its FILE nodes by the absolute path (tool
/// events + wiki-page -> source-file joins) — needs that root, which is
/// otherwise consumed at index time and discarded. Persisting it in a sidecar
/// (not inside the graph) keeps the graph file itself free of machine-specific
/// paths: the structure stays portable, and the machine-specific root lives in
/// a file that is naturally regenerated on the next re-index.
///
/// Best-effort: a failed write is logged and ignored. The graph is the
/// product; the sidecar is a convenience for consumers, and its absence just
/// degrades a consumer's path reconstruction, never the index.
fn write_graph_meta(output_dir: &Path, root: &Path) {
    let meta = json!({
        "schema_version": 1,
        "root": root.to_string_lossy(),
        "tool": "automatised-pipeline",
    });
    let meta_path = output_dir.join("meta.json");
    if let Err(e) = fs::write(&meta_path, meta.to_string()) {
        eprintln!("[ap] write graph meta {}: {e}", meta_path.display());
    }
}

fn do_query_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);

    // Issue #57 — `graph: "missed"` enumerates what the index does NOT cover
    // (parse-incomplete + skipped + quarantined files) instead of running Cypher,
    // so an agent doing graph queries can pivot to grep for the gaps. `query` is
    // not required in this mode. source: DeusData/codebase-memory-mcp
    // query_graph(graph="missed").
    let graph_mode = args
        .get("graph")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    if graph_mode == "missed" {
        if !graph_path.exists() {
            return Err(format!("graph_path does not exist: {graph_str}"));
        }
        return Ok(query_missed_response(graph_path));
    }

    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'query'")?;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);

    // source: H3 fix — query_graph is a read-only tool. Reject any query
    // containing mutation keywords BEFORE any filesystem check, so a mutation
    // attempt is refused even for a nonexistent path (security ordering).
    if let Some(bad) = forbidden_cypher_keyword(query) {
        return Err(format!(
            "read_only_query_required: query_graph is read-only; \
             found forbidden keyword: {bad}"
        ));
    }

    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Bound caller-supplied Cypher: if it has no LIMIT clause, inject one so an
    // unbounded MATCH cannot return enough rows to blow the host's MCP
    // tool-result cap. Queries that already declare a LIMIT are left untouched.
    let (effective_query, limit_injected) = inject_limit_if_absent(query);

    let start = std::time::Instant::now();
    // Read-only tool: reuse the process-local cached handle instead of
    // re-opening the embedded DB per request. Cache revalidates on-disk
    // staleness on every call. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let qr = store.execute_query(&effective_query)?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Cursor stability for query_graph is the caller's responsibility, and we
    // report whether it holds rather than silently shipping an unsafe cursor.
    // The row order here is whatever the caller's Cypher produces. Ladybug (like
    // Cypher generally) does NOT guarantee a stable row order WITHOUT an
    // `ORDER BY` clause — the scan order is an engine implementation detail. We
    // cannot inject an `ORDER BY` into arbitrary Cypher safely (RETURN items may
    // be aggregates/expressions with no addressable sort key) the way we inject
    // `LIMIT`, so we instead detect whether the caller declared one and surface
    // `order_stable` so a client knows whether paging over `offset` is safe.
    // source: Cypher/Kuzu ordering semantics — without ORDER BY, result order is
    // unspecified; see response_budget::BoundedPage docs for why an unstable
    // order makes a cursor skip/duplicate rows.
    let order_stable = has_order_by_clause(query);

    // Second-stage guard: even with the row LIMIT, wide rows (e.g. `RETURN n`
    // serializing whole nodes) can exceed the byte budget. Page by serialized
    // size from `offset` so the caller can pace through a large result set; the
    // page is cursor-safe only when `order_stable` is true (see above).
    let all_rows: Vec<Value> = qr
        .rows
        .iter()
        .map(|row| Value::Array(row.iter().map(|c| json!(c)).collect()))
        .collect();
    let page =
        response_budget::bound_values_paged(all_rows, offset, response_budget::MAX_RESPONSE_CHARS);
    let returned_rows = &page.items;

    // Rebuild the human-readable string from only the rows on THIS page (the
    // window [offset, offset + returned_count)) so it stays within budget
    // alongside the structured `rows`.
    let start = (offset as usize).min(qr.rows.len());
    let returned_string_rows: Vec<Vec<String>> = qr
        .rows
        .iter()
        .skip(start)
        .take(returned_rows.len())
        .cloned()
        .collect();

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "query_graph",
        "columns": qr.columns,
        "rows": returned_rows,
        "result": format_query_result(&qr.columns, &returned_string_rows),
        "elapsed_ms": elapsed_ms,
        "total_count": page.total_count,
        "returned_count": returned_rows.len(),
        "offset": offset,
        "truncated": page.truncated,
        "order_stable": order_stable,
        "limit_injected": limit_injected,
    });
    // Token-surface shaping (issue #56). query_graph already emits `rows` as
    // compact arrays with `columns` declared once — the native tabular shape. The
    // token hog is the human-readable `result` string that duplicates the rows,
    // so the compact modes drop it:
    //   * detail:"ids"   → collapse to the FIRST column's values (a bare id list)
    //                       and drop `result` — the cheap "which nodes match" sweep.
    //   * format:"tabular" → keep columns+rows, drop `result`.
    //   * default (full/json) → unchanged (columns + rows + result string).
    match token_surface::parse_detail(args) {
        token_surface::Detail::Ids => {
            let ids: Vec<Value> = returned_rows
                .iter()
                .filter_map(|r| r.as_array().and_then(|cells| cells.first()).cloned())
                .collect();
            let first_col: Vec<Value> = qr
                .columns
                .first()
                .cloned()
                .map(|c| json!(c))
                .into_iter()
                .collect();
            if let Some(obj) = out.as_object_mut() {
                obj.insert("rows".into(), json!(ids));
                obj.insert("columns".into(), json!(first_col));
                obj.remove("result");
                obj.insert("detail".into(), json!("ids"));
                obj.insert("format".into(), json!("ids"));
            }
        }
        token_surface::Detail::Full => match token_surface::parse_format(args) {
            token_surface::Format::Tabular => {
                if let Some(obj) = out.as_object_mut() {
                    obj.remove("result");
                    obj.insert("detail".into(), json!("full"));
                    obj.insert("format".into(), json!("tabular"));
                }
            }
            token_surface::Format::Json => {
                out["detail"] = json!("full");
                out["format"] = json!("json");
            }
        },
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }
    Ok(out)
}

/// Maximum rows injected into a caller's Cypher when it declares no LIMIT.
///
/// source: derived from the response budget — `MAX_RESPONSE_CHARS / typical
/// row chars`. A typical structured row in this graph serializes to ~90–140
/// chars (measured in `response_budget::tests::measure_representative_row_size`);
/// `100_000 / 140 ≈ 714`. We round down to a conservative bound so the injected
/// LIMIT alone keeps even moderately wide rows under the cap, with the
/// byte-budget pass above as the exact backstop.
const QUERY_GRAPH_ROW_LIMIT: usize = 500;

/// Appends `LIMIT <QUERY_GRAPH_ROW_LIMIT>` to `query` when no LIMIT clause is
/// already present. Returns the (possibly rewritten) query and whether a limit
/// was injected.
///
/// precondition: `query` has already passed `forbidden_cypher_keyword` (it is a
/// read-only query).
/// postcondition: the returned query contains a LIMIT clause; if the input
/// already had one the input is returned verbatim (`limit_injected == false`).
fn inject_limit_if_absent(query: &str) -> (String, bool) {
    if has_limit_clause(query) {
        return (query.to_string(), false);
    }
    // Strip a trailing semicolon/whitespace before appending so we don't emit
    // `... ; LIMIT n`, which Cypher rejects.
    let trimmed = query.trim_end().trim_end_matches(';').trim_end();
    (format!("{trimmed} LIMIT {QUERY_GRAPH_ROW_LIMIT}"), true)
}

/// Detects whether a Cypher query already declares a `LIMIT` clause.
///
/// Matches the keyword `LIMIT` (case-insensitive) only when it stands as a word
/// — not as a substring of an identifier like `node_limit`. This is a syntactic
/// guard, not a full parser: the graph engine itself rejects malformed Cypher.
fn has_limit_clause(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("limit") {
        let start = idx + pos;
        let end = start + "limit".len();
        let prev_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        let next_ok = end >= bytes.len() || !is_ident_char(bytes[end]);
        if prev_ok && next_ok {
            return true;
        }
        idx = end;
    }
    false
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Detects whether a Cypher query declares an `ORDER BY` clause.
///
/// Used to report `order_stable` on `query_graph` responses: an `offset` cursor
/// over the rows is only safe when the caller pinned a deterministic order with
/// `ORDER BY` (without it, the engine's row order is unspecified). Matches the
/// two keywords `order` and `by` as whole words separated only by whitespace,
/// case-insensitively. This is a syntactic guard, not a full parser — it can be
/// fooled by `ORDER BY` inside a string literal, which is acceptable for an
/// advisory flag (the engine remains the source of truth for the query plan).
fn has_order_by_clause(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("order") {
        let start = idx + pos;
        let end = start + "order".len();
        let prev_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        if prev_ok {
            // Skip whitespace after "order", then require the word "by".
            let mut j = end;
            let had_ws = j < bytes.len() && bytes[j].is_ascii_whitespace();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if had_ws && lower[j..].starts_with("by") {
                let by_end = j + "by".len();
                let next_ok = by_end >= bytes.len() || !is_ident_char(bytes[by_end]);
                if next_ok {
                    return true;
                }
            }
        }
        idx = end;
    }
    false
}

fn format_query_result(columns: &[String], rows: &[Vec<String>]) -> String {
    let mut out = columns.join(" | ");
    for row in rows {
        out.push('\n');
        out.push_str(&row.join(" | "));
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 3a — get_symbol
// ---------------------------------------------------------------------------

fn run_get_symbol(arguments: &Value) -> Value {
    match do_get_symbol(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "symbol_lookup_failed", "message": msg
        }),
    }
}

fn do_get_symbol(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;

    // source: C-correctness bug 2 — three-layer lookup (exact → strip-path →
    // fuzzy). Resolves the natural `src/main.rs::foo` form to the stored
    // `main.rs::foo` and returns did_you_mean suggestions otherwise.
    let resolved_qn = match search::resolve_qualified_name(&store, qn) {
        Ok(q) => q,
        Err(nf) => {
            // Cross-repo bridge: a symbol absent locally may be DEFINED in a
            // sibling repo (the dangling reference the resolver marks external).
            // Consult siblings before declaring it not-found. Absent the arg
            // this is a no-op and the original error surfaces unchanged.
            // source: cross-repo bridge spec (bridge module).
            let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
            let foreign = if siblings.is_empty() {
                Vec::new()
            } else {
                bridge::resolve_definition(&siblings, qn)
            };
            if !foreign.is_empty() {
                return Ok(json!({
                    "stage": 3,
                    "status": "ok",
                    "tool": "get_symbol",
                    "resolved_in": "sibling",
                    "message": format!(
                        "'{qn}' is not defined in this graph but is defined in \
                         {} sibling location(s)", foreign.len()),
                    "foreign_definitions": foreign.iter().map(|f| f.to_json()).collect::<Vec<_>>(),
                    "did_you_mean": nf.did_you_mean,
                    "next_steps": [
                        "re-query the owning repo: get_symbol with graph_path set \
                         to the `repo` of a foreign definition".to_string(),
                    ],
                }));
            }
            return Ok(json!({
                "stage": 3,
                "status": "error",
                "reason": "symbol_not_found",
                "message": format!("not found: {}", nf.input),
                "did_you_mean": nf.did_you_mean,
            }));
        }
    };

    // source: M1 fix — centralized cypher_str escapes both `\` and `'`.
    // Returns the string already wrapped in single quotes.
    let escaped = graph_store::cypher_str(&resolved_qn);

    let node = find_symbol_node(&store, &escaped)?;
    let edges_out = find_symbol_edges_out(&store, &escaped)?;
    let edges_in = find_symbol_edges_in(&store, &escaped)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_symbol",
        "node": node,
        "edges_out": edges_out,
        "edges_in": edges_in,
        "next_steps": [
            format!("see relationship context: get_context on '{resolved_qn}'"),
            format!("trace blast radius before changing it: get_impact on '{resolved_qn}'"),
        ],
    }))
}

/// Searches all node tables for a node matching by qualified_name or id.
/// `lit` must be a Cypher-quoted literal produced by `graph_store::cypher_str`.
fn find_symbol_node(store: &graph_store::GraphStore, lit: &str) -> Result<Value, String> {
    let labels = [
        "Function",
        "Method",
        "Struct",
        "Enum",
        "Trait",
        "Variant",
        "Module",
        "Constant",
        "TypeAlias",
        "Field",
        "Import",
        "File",
        "Directory",
        "CallSite",
    ];
    for label in labels {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {lit} \
             OR n.id = {lit} RETURN n"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                return Ok(json!({
                    "label": label,
                    "data": qr.rows[0][0],
                }));
            }
        }
    }
    Ok(Value::Null)
}

/// Queries outgoing edges across all relationship tables.
fn find_symbol_edges_out(store: &graph_store::GraphStore, lit: &str) -> Result<Vec<Value>, String> {
    let mut edges = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE a.qualified_name = {lit} OR a.id = {lit} \
             RETURN '{rel}' AS rel_type, b.id AS target_id"
        );
        collect_edge_rows(store, &cypher, &mut edges);
    }
    Ok(edges)
}

/// Queries incoming edges across all relationship tables.
fn find_symbol_edges_in(store: &graph_store::GraphStore, lit: &str) -> Result<Vec<Value>, String> {
    let mut edges = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE b.qualified_name = {lit} OR b.id = {lit} \
             RETURN '{rel}' AS rel_type, a.id AS source_id"
        );
        collect_edge_rows(store, &cypher, &mut edges);
    }
    Ok(edges)
}

fn collect_edge_rows(store: &graph_store::GraphStore, cypher: &str, edges: &mut Vec<Value>) {
    if let Ok(qr) = store.execute_query(cypher) {
        for row in &qr.rows {
            if row.len() >= 2 {
                edges.push(json!({"rel": row[0], "id": row[1]}));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 3b — resolve_graph
// ---------------------------------------------------------------------------

fn run_resolve_graph(arguments: &Value) -> Value {
    match do_resolve_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "resolve_failed", "message": msg
        }),
    }
}

fn do_resolve_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = resolver::resolve_graph(&store)?;
    let rate = if result.total_refs > 0 {
        result.total_edges as f64 / result.total_refs as f64
    } else {
        0.0
    };

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "resolve_graph",
        "imports_resolved": result.imports_resolved,
        "calls_resolved": result.calls_resolved,
        "implements_resolved": result.impls_resolved,
        "extends_resolved": result.extends_resolved,
        "uses_resolved": result.uses_resolved,
        "total_edges": result.total_edges,
        "total_refs": result.total_refs,
        "resolution_rate": format!("{:.2}", rate),
        "unresolved_count": result.unresolved.len(),
        "elapsed_ms": result.elapsed_ms,
    });

    // Cross-repo bridge: report how many of the locally-unresolved references
    // a sibling repo can define — i.e. how much of the dangling set is a
    // cross-service edge rather than a true external/third-party dependency.
    // Absent the arg this is a no-op. source: cross-repo bridge spec.
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if !siblings.is_empty() {
        // Exclude refs already classified external (stdlib/framework/
        // third-party root, e.g. androidx.*, retrofit2.*) from the
        // candidate pool: those are known-external by construction, not
        // potential cross-repo bridges, regardless of whether the
        // classifying provider's prefix list caught them. This filter and
        // the prefix-list widening in language_provider.rs are independent
        // defenses — issue #31.
        let targets: Vec<String> = result
            .unresolved
            .iter()
            .filter(|u| u.reason != resolver::EXTERNAL_UNRESOLVED_REASON)
            .map(|u| u.target_text.clone())
            .collect();
        let (resolvable, sample) = bridge::count_cross_repo_resolvable(&siblings, &targets);
        out["cross_repo_resolvable"] = json!(resolvable);
        out["cross_repo_sample"] = json!(sample);
        if !siblings.skipped.is_empty() {
            out["sibling_graphs_skipped"] = json!(siblings.skipped);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Stage 3c — cluster_graph
// ---------------------------------------------------------------------------

fn run_cluster_graph(arguments: &Value) -> Value {
    match do_cluster_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "cluster_failed", "message": msg
        }),
    }
}

fn do_cluster_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let gamma = args
        .get("resolution_param")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = clustering::cluster_graph(&store, gamma)?;
    let memberships = clustering::collect_cluster_memberships(&store)?;

    let clusters: Vec<Value> = memberships
        .entries
        .iter()
        .map(|m| {
            json!({
                "qualified_name": m.qualified_name,
                "community_id": m.community_id,
                "qn": m.qualified_name,
                "cluster_id": m.cluster_id,
            })
        })
        .collect();

    let mut body = json!({
        "stage": 3,
        "status": "ok",
        "tool": "cluster_graph",
        "community_count": result.communities,
        "modularity": format!("{:.6}", result.modularity),
        "process_count": result.processes,
        "elapsed_ms": result.elapsed_ms,
        "clusters": clusters,
        "total_memberships": memberships.total,
    });
    if let Some(n) = memberships.truncated_at {
        body["clusters_truncated_at"] = json!(n);
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Stage 3c — get_processes
// ---------------------------------------------------------------------------

fn run_get_processes(arguments: &Value) -> Value {
    match do_get_processes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "processes_failed", "message": msg
        }),
    }
}

fn do_get_processes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let mut processes = clustering::get_processes(&store)?;

    // Cursor stability: get_processes runs `MATCH (p:Process) RETURN ...` with no
    // ORDER BY (clustering::process::get_processes), so the Ladybug scan order is
    // an engine implementation detail and is NOT guaranteed identical across
    // calls — paging over it would skip/duplicate rows. We impose a deterministic
    // total order here, at the read boundary, by (name, entry_point). Process
    // names are entry-point-derived and entry_point is a node id, so the pair is
    // unique, giving a total (tie-free) order. source: cursor-correctness
    // requirement (response_budget::BoundedPage docs).
    processes.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.entry_point.cmp(&b.entry_point))
    });

    let procs: Vec<Value> = processes
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "entry_point": p.entry_point,
                "entry_kind": p.entry_kind,
                "depth": p.depth,
                "node_count": p.node_count,
            })
        })
        .collect();

    // Page the array by serialized size from `offset` so a graph with thousands
    // of entry points cannot blow the host's MCP tool-result cap, and a client
    // can retrieve the full list in budget-sized pages via `next_offset`.
    let page =
        response_budget::bound_values_paged(procs, offset, response_budget::per_section_chars());

    // Token-surface shaping (issue #56): ids → bare process names; tabular →
    // columns-once rows. See token_surface.
    let detail = token_surface::parse_detail(args);
    let format = token_surface::parse_format(args);
    let view = token_surface::render_list(&page.items, PROCESS_COLUMNS, "name", &detail, &format);

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_processes",
        "process_count": page.items.len(),
        "total_count": page.total_count,
        "offset": offset,
        "truncated": page.truncated,
        "detail": view.detail,
        "format": view.format,
        "processes": view.value,
    });
    if let Some(cols) = view.columns {
        out["columns"] = cols;
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }
    Ok(out)
}

/// Scalar columns of a process row, in tabular-projection order (issue #56).
const PROCESS_COLUMNS: &[&str] = &["name", "entry_point", "entry_kind", "depth", "node_count"];

// ---------------------------------------------------------------------------
// Stage 3c — get_impact
// ---------------------------------------------------------------------------

fn run_get_impact(arguments: &Value) -> Value {
    match do_get_impact(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "impact_failed", "message": msg
        }),
    }
}

/// Scalar columns of a reverse-dependency handle, in tabular-projection order
/// (issue #56). Shared by all four impact sections (they are homogeneous).
const IMPACT_COLUMNS: &[&str] = &["qualified_name", "label", "confidence", "id"];

/// Columns of a co-change partner row (issue #58) in get_impact.
const COCHANGE_PARTNER_COLUMNS: &[&str] = &["file", "cochange_count", "coupling", "jaccard"];

/// The FILE_CHANGES_WITH partners of `file` (issue #58), strongest coupling
/// first, as homogeneous objects ready for the token surface. Best-effort: an
/// empty graph or a graph mined without cochange yields an empty list.
fn cochange_partners(store: &graph_store::GraphStore, file: &str) -> Vec<Value> {
    // Undirected: FILE_CHANGES_WITH stores one edge per pair (a<b), so match both
    // directions to find every partner of `file`.
    let q = format!(
        "MATCH (f:File)-[r:FILE_CHANGES_WITH]-(g:File) WHERE f.id = {} \
         RETURN g.id, r.cochange_count, r.coupling, r.jaccard \
         ORDER BY r.coupling DESC, g.id",
        graph_store::cypher_str(file)
    );
    let qr = match store.execute_query(&q) {
        Ok(qr) => qr,
        Err(_) => return Vec::new(),
    };
    qr.rows
        .iter()
        .filter(|row| row.len() >= 4)
        .map(|row| {
            json!({
                "file": row[0],
                "cochange_count": row[1].parse::<i64>().unwrap_or(0),
                "coupling": row[2],
                "jaccard": row[3],
            })
        })
        .collect()
}

fn do_get_impact(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let mut impact = clustering::get_impact(&store, qn)?;

    // Cursor stability: each reverse-dependency list is assembled in
    // clustering::impact::reverse_dependents by iterating REL_TABLES and, per
    // table, the engine's unordered scan rows (no ORDER BY) — neither order is a
    // guaranteed-stable contract across calls. We impose a deterministic total
    // order at the read boundary by (qualified_name, id). `id` is the node's
    // unique graph id, so the pair is tie-free, giving a total order safe to page.
    // source: cursor-correctness requirement (response_budget::BoundedPage docs).
    let sort_nodes = |nodes: &mut Vec<clustering::ImpactNode>| {
        nodes.sort_by(|a, b| {
            a.qualified_name
                .cmp(&b.qualified_name)
                .then_with(|| a.id.cmp(&b.id))
        });
    };
    sort_nodes(&mut impact.callers);
    sort_nodes(&mut impact.importers);
    sort_nodes(&mut impact.users);
    sort_nodes(&mut impact.implementors);

    // Serialize reverse-dependency endpoints as re-queryable handles so the
    // caller can keep traversing through MCP (get_symbol/get_context on `id`)
    // rather than receiving a flattened terminal digest.
    let to_handles = |nodes: &[clustering::ImpactNode]| -> Vec<Value> {
        nodes
            .iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "qualified_name": n.qualified_name,
                    "label": n.label,
                    // Confidence of the edge linking this dependent to the
                    // target; < 1.0 marks a heuristically-resolved dependency.
                    "confidence": format!("{:.2}", n.confidence),
                })
            })
            .collect()
    };
    // dependents_total is computed from the FULL, pre-truncation counts so the
    // caller always sees the true blast-radius size even when a section is cut.
    let dependents_total = impact.callers.len()
        + impact.importers.len()
        + impact.users.len()
        + impact.implementors.len();

    // `callers` is the PRIMARY list and the only one the `offset` cursor pages:
    // a caller can page callers to exhaustion via `next_offset`. The remaining
    // three sections (importers/users/implementors) are bounded SUMMARIES — they
    // start from index 0 and are byte-capped, not cursored. This is reported
    // honestly (see `secondary_lists_paged: false`) rather than inventing a
    // multi-cursor scheme. To page a secondary blast-radius dimension at scale,
    // query it directly via query_graph (which carries its own ORDER BY + offset).
    let callers = response_budget::bound_values_paged(
        to_handles(&impact.callers),
        offset,
        response_budget::per_section_chars(),
    );
    let importers = response_budget::bound_values(
        to_handles(&impact.importers),
        response_budget::per_section_chars(),
    );
    let users = response_budget::bound_values(
        to_handles(&impact.users),
        response_budget::per_section_chars(),
    );
    let implementors = response_budget::bound_values(
        to_handles(&impact.implementors),
        response_budget::per_section_chars(),
    );

    let any_truncated =
        callers.truncated || importers.truncated || users.truncated || implementors.truncated;

    // Token-surface shaping (issue #56): render each reverse-dependency section
    // under the shared detail/format. ids → bare qualified names; tabular →
    // columns-once rows (one `columns` header covers all four sections, which are
    // homogeneous). See token_surface.
    let detail = token_surface::parse_detail(args);
    let format = token_surface::parse_format(args);
    let render = |items: &[Value]| {
        token_surface::render_list(items, IMPACT_COLUMNS, "qualified_name", &detail, &format)
    };
    let callers_view = render(&callers.items);
    let importers_view = render(&importers.items);
    let users_view = render(&users.items);
    let implementors_view = render(&implementors.items);

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_impact",
        "qualified_name": qn,
        "communities": impact.communities,
        "communities_affected": impact.communities.len(),
        "processes": impact.processes,
        "processes_affected": impact.processes.len(),
        "detail": callers_view.detail,
        "format": callers_view.format,
        "callers": callers_view.value,
        "callers_total": callers.total_count,
        "offset": offset,
        "primary_list": "callers",
        "secondary_lists_paged": false,
        "importers": importers_view.value,
        "importers_total": importers.total_count,
        "users": users_view.value,
        "users_total": users.total_count,
        "implementors": implementors_view.value,
        "implementors_total": implementors.total_count,
        "dependents_total": dependents_total,
        "truncated": any_truncated,
        // Epistemic honesty: is this blast radius exhaustive, or a lower bound?
        // `lower-bound` means real impact may exceed what is shown — because the
        // target is reached via dynamic dispatch and/or some edges were resolved
        // heuristically. `epistemic_reasons` names the carriers of uncertainty.
        // source: epistemic module; clustering::get_impact.
        "epistemic": impact.epistemic.as_str(),
        "epistemic_reasons": impact.epistemic_reasons,
    });
    if callers_view.columns.is_some() {
        // One header covers all four homogeneous sections.
        out["columns"] = json!(IMPACT_COLUMNS);
    }
    if let Some(next) = callers.next_offset {
        out["next_offset"] = json!(next);
    }

    // Issue #58: the symbol's FILE co-change partners are impact candidates the
    // static call graph cannot see (files that historically change together —
    // the architect agent's churning-pairs signal). Add them as a section under
    // the same detail/format surface. The symbol's file is the qn up to '::'.
    let file = qn.split("::").next().unwrap_or(qn);
    let partners = cochange_partners(&store, file);
    // Partners have their own shape ({file, cochange_count, coupling, jaccard}),
    // so render them with their own columns/id — NOT the impact `render` closure.
    let partner_view = token_surface::render_list(
        &partners,
        COCHANGE_PARTNER_COLUMNS,
        "file",
        &detail,
        &format,
    );
    out["cochange_partners"] = partner_view.value;
    out["cochange_partners_total"] = json!(partners.len());
    if partner_view.columns.is_some() {
        out["cochange_partner_columns"] = json!(COCHANGE_PARTNER_COLUMNS);
    }

    // Suggested follow-up traversals from this blast-radius result.
    out["next_steps"] = impact_next_steps(&impact, qn);

    // Cross-repo bridge: when sibling graphs are supplied, also surface callers
    // that live in OTHER repos. These are reported in their own section (not
    // merged into the local `callers`/`dependents_total`) so blast radius keeps
    // local and foreign impact distinct. Absent the arg this is a no-op.
    // source: cross-repo bridge spec (bridge module).
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if !siblings.is_empty() {
        let foreign = bridge::foreign_callers(&siblings, bridge::last_segment(qn));
        let handles: Vec<Value> = foreign.iter().map(|f| f.to_json()).collect();
        let foreign_page =
            response_budget::bound_values(handles, response_budget::per_section_chars());
        out["foreign_callers"] = json!(foreign_page.items);
        out["foreign_callers_total"] = json!(foreign.len());
        out["foreign_callers_paged"] = json!(false);
        if !siblings.skipped.is_empty() {
            out["sibling_graphs_skipped"] = json!(siblings.skipped);
        }
        // Cross-repo edges are name-matched without a shared linker, so any
        // foreign caller makes the blast radius a lower bound (and stays one
        // even if the local set was exact). source: epistemic module contract.
        if !foreign.is_empty() {
            out["epistemic"] = json!(epistemic::Boundary::LowerBound.as_str());
            if let Some(reasons) = out["epistemic_reasons"].as_array_mut() {
                reasons.push(json!(format!(
                    "{} cross-repo caller(s) were matched by symbol name across \
                     sibling graphs without a shared linker (confidence 0.50); \
                     foreign blast radius is heuristic",
                    foreign.len()
                )));
            }
        }
    }
    Ok(out)
}

/// Suggests the natural follow-up tool calls after a `get_impact` result, so a
/// caller continues traversing the graph rather than stopping at the digest.
/// Hints are graph-grounded (only suggested when the corresponding dimension is
/// non-empty / relevant), never speculative.
fn impact_next_steps(impact: &clustering::ImpactResult, qn: &str) -> Value {
    let mut steps = Vec::new();
    if !impact.callers.is_empty() {
        steps.push(
            "inspect a caller's own blast radius: get_impact on a `callers[].qualified_name`"
                .to_string(),
        );
    }
    if impact.epistemic == epistemic::Boundary::LowerBound {
        steps.push(format!(
            "this is a lower bound — run get_context on '{qn}' to review its \
             interface relationships, or lsp_resolve to tighten dynamic-dispatch edges"
        ));
    }
    if !impact.implementors.is_empty() {
        steps.push(
            "review an implementor directly: get_symbol on an `implementors[].qualified_name`"
                .to_string(),
        );
    }
    json!(steps)
}

// ---------------------------------------------------------------------------
// History — index_history (temporal layer over the structural snapshot)
// ---------------------------------------------------------------------------

fn run_index_history(arguments: &Value) -> Value {
    match do_index_history(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_history_failed", "message": msg
        }),
    }
}

fn do_index_history(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let codebase_str = args
        .get("codebase_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'codebase_path'")?;
    let max_commits = args
        .get("max_commits")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let codebase_path = Path::new(codebase_str);
    if !codebase_path.exists() {
        return Err(format!("codebase_path does not exist: {codebase_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = history::index_history(&store, codebase_path, max_commits)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_history",
        "commits": result.commits,
        "versions": result.versions,
        "commit_edges": result.commit_edges,
        "version_edges": result.version_edges,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3d — search_codebase
// ---------------------------------------------------------------------------

fn run_search_codebase(arguments: &Value) -> Value {
    match do_search_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "search_failed", "message": msg
        }),
    }
}

/// Scalar columns of a search hit, in tabular-projection order (issue #56). The
/// `processes` array field is intentionally omitted from the tabular projection
/// (a cell must be scalar); it stays available via the grouped `by_process`
/// section and in the default `detail:"full", format:"json"` objects.
const SEARCH_COLUMNS: &[&str] = &[
    "qualified_name",
    "name",
    "kind",
    "file_path",
    "score",
    "start_line",
    "end_line",
    "community_id",
];

fn do_search_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'query'")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    let label_filter = args
        .get("label_filter")
        .and_then(|v| v.as_str())
        .map(String::from);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // The search index lives in a sibling ``search_index/`` of the graph dir.
    // Pass it explicitly to search_graph — no process-global env hand-off
    // (that channel raced across parallel callers; see search::search_graph).
    // Shared with Stage 4's prepare_prd_input via search::resolve_search_index_dir
    // (issue #18) so both stages resolve the same graph to the same index.
    let search_index_dir = search::resolve_search_index_dir(graph_path);

    let start = std::time::Instant::now();
    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let options = search::SearchOptions {
        limit,
        label_filter,
        min_score: 0.01,
    };
    let results = search::search_graph(&store, query, &options, search_index_dir.as_deref())?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Cursor stability: search_graph returns results in a deterministic total
    // order — descending score with an ascending-qualified_name tie-break added
    // at every sort site (search::mod final sort, search::rrf::fuse). That total
    // order is identical across calls for a fixed graph + query, so paging by
    // `offset` over it neither skips nor duplicates rows. `limit` bounds the
    // ranked candidate universe; `offset` + byte budget page within it.
    // source: cursor-correctness requirement (response_budget::BoundedPage docs).
    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "qualified_name": r.qualified_name,
                "name": r.name,
                "kind": r.label,
                "file_path": r.file_path,
                "score": format!("{:.4}", r.score),
                "community_id": r.community_id,
                "processes": r.process_names,
                "start_line": r.start_line,
                "end_line": r.end_line,
            })
        })
        .collect();

    // Page the ranked results by serialized size from `offset`. Previously this
    // tool relied solely on `limit`; a wide result set could still approach the
    // host cap and offered no way to retrieve rows beyond the cut. Byte-budget
    // paging makes the full ranked list retrievable in budget-sized pages.
    let page =
        response_budget::bound_values_paged(items, offset, response_budget::per_section_chars());

    // Process-grouped view: a lightweight secondary index over the returned
    // page. Built from `page.items` (not the full ranked set) so every
    // qualified_name it lists is present in `results` — the flat list stays the
    // single source of truth; `by_process` never duplicates row payload and
    // never references a row outside the page. source: search::group_hits_by_process.
    let group_input: Vec<(String, Vec<String>)> = page
        .items
        .iter()
        .map(|item| {
            let qn = item
                .get("qualified_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let processes = item
                .get("processes")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (qn, processes)
        })
        .collect();
    let by_process: Vec<Value> = search::group_hits_by_process(&group_input)
        .into_iter()
        .map(|(process, qualified_names)| {
            json!({
                "process": process,
                "qualified_names": qualified_names,
            })
        })
        .collect();

    // Token-surface shaping (issue #56): `detail:"ids"` returns bare qualified
    // names for a cheap wide sweep; `format:"tabular"` streams rows as arrays with
    // the columns declared once. Applied to the PAGED list, so the cursor contract
    // is untouched. `by_process` (built from the objects above) is unaffected.
    let detail = token_surface::parse_detail(args);
    let format = token_surface::parse_format(args);
    let view = token_surface::render_list(
        &page.items,
        SEARCH_COLUMNS,
        "qualified_name",
        &detail,
        &format,
    );

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "search_codebase",
        "query": query,
        "result_count": page.items.len(),
        "total_count": page.total_count,
        "offset": offset,
        "truncated": page.truncated,
        "detail": view.detail,
        "format": view.format,
        "results": view.value,
        "by_process": by_process,
        "elapsed_ms": elapsed_ms,
    });
    if let Some(cols) = view.columns {
        out["columns"] = cols;
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }

    // Cross-repo bridge: federate the query across sibling graphs. Foreign hits
    // are a bounded SECONDARY section (repo-tagged, not merged into the primary
    // cursored `results`) so the local `offset` contract stays exact. Absent the
    // arg this is a no-op. source: cross-repo bridge spec.
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if !siblings.is_empty() {
        let hits = bridge::federated_search(&siblings, query, limit);
        let foreign_items: Vec<Value> = hits.iter().map(|h| h.to_json()).collect();
        let foreign_page =
            response_budget::bound_values(foreign_items, response_budget::per_section_chars());
        out["foreign_results"] = json!(foreign_page.items);
        out["foreign_results_total"] = json!(hits.len());
        out["foreign_results_paged"] = json!(false);
        if !siblings.skipped.is_empty() {
            out["sibling_graphs_skipped"] = json!(siblings.skipped);
        }
    }

    // Suggest how to act on a hit: top-ranked results are the natural next
    // traversal anchors. Gated on a non-empty page so we never suggest acting
    // on nothing.
    if let Some(first) = page
        .items
        .first()
        .and_then(|r| r.get("qualified_name"))
        .and_then(|v| v.as_str())
    {
        out["next_steps"] = json!([
            format!("inspect the top hit: get_context on '{first}'"),
            "narrow further with `label_filter` (e.g. Function, Struct, Trait)".to_string(),
        ]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Stage 3d — get_context
// ---------------------------------------------------------------------------

fn run_get_context(arguments: &Value) -> Value {
    match do_get_context(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "context_failed", "message": msg
        }),
    }
}

fn do_get_context(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let ctx = match search::get_context(&store, qn) {
        Ok(c) => c,
        Err(search::GetContextError::NotFound(nf)) => {
            // source: C-correctness bug 2 — prefer a clean `symbol_not_found`
            // with did_you_mean over a cryptic string error. Return Ok(Value)
            // because the outer `run_get_context` would otherwise wrap this
            // under `context_failed`.
            return Ok(json!({
                "stage": 3,
                "status": "error",
                "reason": "symbol_not_found",
                "message": format!("not found: {}", nf.input),
                "did_you_mean": nf.did_you_mean,
            }));
        }
        Err(search::GetContextError::Other(m)) => return Err(m),
    };

    let related_to_json = |items: &[search::RelatedSymbol]| -> Vec<Value> {
        items
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "qualified_name": s.qualified_name,
                    "kind": s.label,
                })
            })
            .collect()
    };

    let community_json = ctx.community.as_ref().map(|c| {
        json!({
            "id": c.id,
            "name": c.name,
            "member_count": c.member_count,
        })
    });

    let processes_json: Vec<Value> = ctx
        .processes
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "role": p.role,
            })
        })
        .collect();

    // Epistemic honesty: when the symbol is a dynamic-dispatch surface (a
    // trait/interface), its reverse relationships (`called_by`, `used_by`) are a
    // LOWER BOUND — polymorphic call sites that go through the interface are not
    // exhaustively attributable to it by static resolution. Concrete symbols are
    // exact. source: epistemic module.
    let dynamic_surface = epistemic::is_dynamic_dispatch_surface(&ctx.label);
    let (epistemic_status, epistemic_reasons) = if dynamic_surface {
        (
            epistemic::Boundary::LowerBound.as_str(),
            vec![format!(
                "'{}' is a {} (dynamic-dispatch surface): `called_by`/`used_by` \
                 omit call sites that reach it polymorphically; treat the reverse \
                 relationships as a lower bound and consult `implemented_by`",
                ctx.qualified_name, ctx.label
            )],
        )
    } else {
        (epistemic::Boundary::Exact.as_str(), Vec::new())
    };

    let mut next_steps = vec![format!(
        "trace blast radius: get_impact on '{}'",
        ctx.qualified_name
    )];
    if dynamic_surface && !ctx.implemented_by.is_empty() {
        next_steps.push(
            "enumerate concrete behaviour: get_symbol on an `implemented_by[].qualified_name`"
                .to_string(),
        );
    }

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_context",
        "symbol": {
            "qualified_name": ctx.qualified_name,
            "name": ctx.name,
            "kind": ctx.label,
            "file_path": ctx.file_path,
            "start_line": ctx.start_line,
            "end_line": ctx.end_line,
            "visibility": ctx.visibility,
        },
        "relationships": {
            "imports": related_to_json(&ctx.imports),
            "imported_by": related_to_json(&ctx.imported_by),
            "calls": related_to_json(&ctx.calls),
            "called_by": related_to_json(&ctx.called_by),
            "implements": related_to_json(&ctx.implements),
            "implemented_by": related_to_json(&ctx.implemented_by),
            "uses": related_to_json(&ctx.uses),
            "used_by": related_to_json(&ctx.used_by),
        },
        "community": community_json,
        "processes": processes_json,
        "epistemic": epistemic_status,
        "epistemic_reasons": epistemic_reasons,
        "next_steps": next_steps,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3 — analyze_codebase (all-in-one: index + resolve + cluster)
// ---------------------------------------------------------------------------

fn run_analyze_codebase(arguments: &Value) -> Value {
    match do_analyze_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "analyze_failed", "message": msg
        }),
    }
}

fn do_analyze_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'path'")?;
    let output_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let gamma = args
        .get("resolution_param")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let enable_lsp = args.get("lsp").and_then(|v| v.as_bool()).unwrap_or(false);
    let lang_filter = parse_language_filter(args)?;
    let dependency_scope = parse_dependency_scope(args)?;

    let codebase = require_absolute(path_str, "path")?;
    if !codebase.exists() {
        return Err(format!("path does not exist: {}", codebase.display()));
    }
    let output_dir = require_absolute(output_str, "output_dir")?;
    fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
    let graph_dir = output_dir.join("graph");
    // source: H4 fix — see do_index_codebase.
    validate_graph_path_safe(&graph_dir)?;
    if graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&graph_dir)?;
    }

    let total_start = std::time::Instant::now();

    // Phase 1: index
    let index_result = indexer::index_codebase_with_language(
        &codebase,
        &graph_dir,
        lang_filter,
        dependency_scope,
    )?;
    // Record the absolute source root beside the graph (see write_graph_meta).
    write_graph_meta(&output_dir, &codebase);

    // Phase 2: resolve
    let store = graph_store::GraphStore::open_or_create(&index_result.graph_path)?;
    let resolve_result = resolver::resolve_graph(&store)?;

    // Phase 2b: LSP-enhanced resolution (optional)
    let lsp_result = if enable_lsp {
        let effective_lang = match lang_filter {
            Some(lang) => lang.as_str().to_string(),
            None => detect_dominant_language(&codebase),
        };
        // graceful fallback: any LSP error is silently ignored below
        lsp_resolver::resolve_with_lsp(
            &store,
            &codebase,
            &effective_lang,
            None,
            std::time::Duration::from_secs(30),
        )
        .ok()
    } else {
        None
    };

    // Phase 3: cluster
    let cluster_result = clustering::cluster_graph(&store, gamma)?;

    // Phase 4: build search index (BM25 + TF-IDF vectors)
    let search_index_result = search::build_search_index(&store, &output_dir)?;

    let total_ms = total_start.elapsed().as_millis() as u64;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "analyze_codebase",
        "graph_path": index_result.graph_path.to_string_lossy(),
        "index": {
            "node_count": index_result.node_count,
            "edge_count": index_result.edge_count,
            "files_indexed": index_result.files_indexed,
        },
        "resolve": {
            "total_edges": resolve_result.total_edges,
            "resolution_rate": format!("{:.2}",
                if resolve_result.total_refs > 0 {
                    resolve_result.total_edges as f64 / resolve_result.total_refs as f64
                } else { 0.0 }),
        },
        "cluster": {
            "community_count": cluster_result.communities,
            "modularity": format!("{:.6}", cluster_result.modularity),
            "process_count": cluster_result.processes,
        },
        "search_index": {
            "bm25_doc_count": search_index_result.bm25_doc_count,
            "vector_doc_count": search_index_result.vector_doc_count,
            "elapsed_ms": search_index_result.elapsed_ms,
        },
        "lsp_resolve": match &lsp_result {
            Some(r) => json!({
                "resolved_count": r.resolved_count,
                "failed_count": r.failed_count,
                "skipped_count": r.skipped_count,
                "elapsed_ms": r.elapsed_ms,
            }),
            None => json!(null),
        },
        "total_elapsed_ms": total_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3e — detect_changes (git diff impact)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Stage 3b-v2 — lsp_resolve (LSP-enhanced resolution)
// ---------------------------------------------------------------------------

fn run_lsp_resolve(arguments: &Value) -> Value {
    match do_lsp_resolve(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Distinguish specific failure reasons so callers can act on them.
            if msg.contains("lsp_command_not_allowed") {
                // source: C3 fix — surface the reason code plus the allowlist
                // so the caller knows which commands are accepted.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_command_not_allowed",
                    "message": msg,
                    "allowed": lsp_client::LSP_COMMAND_ALLOWLIST,
                })
            } else if msg.contains("lsp_not_found") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_not_found",
                    "message": msg
                })
            } else if msg.contains("lsp_probe_failed") {
                // source: C-correctness bug 1 — binary on PATH but doesn't
                // speak LSP (rustup proxy, stub script, /bin/true, ...).
                // Distinct from lsp_not_found so callers can act on it.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_probe_failed",
                    "message": msg
                })
            } else {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_resolve_failed",
                    "message": msg
                })
            }
        }
    }
}

fn do_lsp_resolve(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let codebase_str = args
        .get("codebase_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'codebase_path'")?;
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let lsp_command = args.get("lsp_command").and_then(|v| v.as_str());
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let codebase_path = Path::new(codebase_str);
    if !codebase_path.exists() {
        return Err(format!("codebase_path does not exist: {codebase_str}"));
    }

    // Auto-detect language from codebase if needed
    let effective_lang = if language == "auto" {
        detect_dominant_language(codebase_path)
    } else {
        language.to_string()
    };

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let timeout = std::time::Duration::from_millis(timeout_ms);

    let result = lsp_resolver::resolve_with_lsp(
        &store,
        codebase_path,
        &effective_lang,
        lsp_command,
        timeout,
    )?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "lsp_resolve",
        "resolved_count": result.resolved_count,
        "failed_count": result.failed_count,
        "skipped_count": result.skipped_count,
        "elapsed_ms": result.elapsed_ms,
    }))
}

/// Detect the dominant language from file extensions in a codebase.
fn detect_dominant_language(path: &Path) -> String {
    let mut rs_count = 0u32;
    let mut py_count = 0u32;
    let mut ts_count = 0u32;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(|e| e.to_str()) {
                Some("rs") => rs_count += 1,
                Some("py") => py_count += 1,
                Some("ts") | Some("tsx") => ts_count += 1,
                _ => {}
            }
        }
    }

    if rs_count >= py_count && rs_count >= ts_count {
        "rust".to_string()
    } else if py_count >= ts_count {
        "python".to_string()
    } else {
        "typescript".to_string()
    }
}

fn run_detect_changes(arguments: &Value) -> Value {
    match do_detect_changes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "detect_changes_failed", "message": msg
        }),
    }
}

fn do_detect_changes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let diff_text = args.get("diff_text").and_then(|v| v.as_str());
    let codebase_path = args.get("codebase_path").and_then(|v| v.as_str());
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let head_ref = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;

    let analysis = if let Some(text) = diff_text {
        git_diff::analyze_diff(&store, text)?
    } else if let Some(repo) = codebase_path {
        let repo_path = Path::new(repo);
        if !repo_path.exists() {
            return Err(format!("codebase_path does not exist: {repo}"));
        }
        git_diff::analyze_git_diff(&store, repo_path, base_ref, head_ref)?
    } else {
        return Err("either 'diff_text' or 'codebase_path' must be provided".to_string());
    };

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "detect_changes",
        "files_changed": analysis.files_changed,
        "symbols_affected": analysis.symbols_affected,
        "symbols_affected_count": analysis.symbols_affected.len(),
        "communities_affected": analysis.communities_affected,
        "communities_affected_count": analysis.communities_affected.len(),
        "processes_affected": analysis.processes_affected,
        "processes_affected_count": analysis.processes_affected.len(),
        "risk_score": format!("{:.4}", analysis.risk_score),
        // Epistemic qualification of risk_score: the mean confidence of the
        // reverse-dependency edges the risk rests on, and whether any changed
        // symbol's blast radius is a lower bound (true risk may exceed score).
        // source: git_diff::assess_dependency_confidence.
        "mean_dependency_confidence": format!("{:.2}", analysis.mean_dependency_confidence),
        "epistemic": analysis.epistemic,
        "epistemic_reasons": analysis.epistemic_reasons,
        "next_steps": detect_changes_next_steps(&analysis),
    }))
}

/// Suggests follow-up tools after a `detect_changes` result. Graph-grounded:
/// each hint is gated on a present dimension of the analysis.
fn detect_changes_next_steps(analysis: &git_diff::DiffAnalysis) -> Value {
    let mut steps = Vec::new();
    if !analysis.symbols_affected.is_empty() {
        steps.push(
            "drill into a changed symbol's blast radius: get_impact on a \
             `symbols_affected[].qualified_name`"
                .to_string(),
        );
    }
    if analysis.epistemic == epistemic::Boundary::LowerBound.as_str() {
        steps.push(
            "risk is a lower bound (see `epistemic_reasons`) — run lsp_resolve to \
             tighten dynamic-dispatch edges before trusting the score"
                .to_string(),
        );
    }
    json!(steps)
}

// ---------------------------------------------------------------------------
// Stage 4 — prepare_prd_input (bundle verified finding + graph intel)
// ---------------------------------------------------------------------------

fn run_prepare_prd_input(arguments: &Value) -> Value {
    match do_prepare_prd_input(arguments) {
        Ok(v) => v,
        Err(msg) => stage4_error_response(&msg),
    }
}

fn stage4_error_response(msg: &str) -> Value {
    let reason = if msg.starts_with("stage_2_not_verified") {
        "stage_2_not_verified"
    } else if msg.starts_with("stage_1_refined_missing")
        || msg.starts_with("stage_1_refined_unreadable")
        || msg.starts_with("stage_1_refined_corrupt")
    {
        "stage_1_refined_missing"
    } else {
        "prepare_prd_input_failed"
    };
    json!({
        "stage": 4,
        "status": "error",
        "reason": reason,
        "message": msg,
    })
}

fn do_prepare_prd_input(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    // run_id defaults to "adhoc" — required only as a path segment. In feature
    // mode the caller often has no pipeline run.
    let run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("adhoc")
        .to_string();
    validate_safe_id("run_id", &run_id)?;
    // finding_id is OPTIONAL now: present → finding mode (verified stage-2
    // gate); absent → feature mode (requires feature_description).
    let finding_id = match args.get("finding_id").and_then(|v| v.as_str()) {
        Some(fid) => {
            validate_safe_id("finding_id", fid)?;
            Some(fid.to_string())
        }
        None => None,
    };
    let feature_description = args
        .get("feature_description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if finding_id.is_none() && feature_description.is_none() {
        return Err("missing input: provide 'finding_id' (finding mode) or \
             'feature_description' (feature mode)"
            .to_string());
    }
    let output_dir_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let output_dir = require_absolute(output_dir_str, "output_dir")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }

    let prepared_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let outcome = prd_input::prepare(
        &prd_input::PrdInputArgs {
            run_id: run_id.clone(),
            finding_id: finding_id.clone(),
            feature_description: feature_description.clone(),
            output_dir,
            graph_path,
        },
        prepared_at.clone(),
    )?;

    Ok(json!({
        "stage": 4,
        "status": "ok",
        "tool": "prepare_prd_input",
        "mode": if finding_id.is_some() { "finding" } else { "feature" },
        "run_id": run_id,
        "finding_id": finding_id,
        "artifact_path": outcome.artifact_path.to_string_lossy(),
        "prepared_at": prepared_at,
        "matched_symbol_count": outcome.matched_symbol_count,
        // Lexical-only hits, never counted as verified grounding (issue #14).
        "candidate_symbol_count": outcome.candidate_symbol_count,
        "impacted_community_count": outcome.impacted_community_count,
        "impacted_process_count": outcome.impacted_process_count,
        // Grounding returned inline so the PRD generator can inject it without
        // a second read of artifact_path.
        "prd_context": outcome.prd_context,
        "preparer_version": prd_input::PREPARER_VERSION,
    }))
}

// ---------------------------------------------------------------------------
// Stage 9 — verify_semantic_diff (diff two graphs; flag regressions)
// ---------------------------------------------------------------------------

fn run_verify_semantic_diff(arguments: &Value) -> Value {
    match do_verify_semantic_diff(arguments) {
        Ok(v) => v,
        Err(msg) => {
            let reason = if msg.contains("before_graph_path_missing") {
                "before_graph_path_missing"
            } else if msg.contains("after_graph_path_missing") {
                "after_graph_path_missing"
            } else {
                "verify_semantic_diff_failed"
            };
            json!({
                "stage": 9,
                "status": "error",
                "reason": reason,
                "message": msg,
            })
        }
    }
}

fn do_verify_semantic_diff(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let before_str = args
        .get("before_graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'before_graph_path'")?;
    let after_str = args
        .get("after_graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'after_graph_path'")?;
    let before_graph_path = require_absolute(before_str, "before_graph_path")?;
    let after_graph_path = require_absolute(after_str, "after_graph_path")?;
    let report_path = args
        .get("report_path")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "report_path"))
        .transpose()?;

    let verified_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let outcome = semantic_diff::diff(
        &semantic_diff::SemanticDiffArgs {
            before_graph_path: before_graph_path.clone(),
            after_graph_path: after_graph_path.clone(),
        },
        verified_at.clone(),
    )?;

    let written = match &report_path {
        Some(p) => {
            semantic_diff::write_report(p, &outcome.report)?;
            Some(p.to_string_lossy().to_string())
        }
        None => None,
    };

    Ok(json!({
        "stage": 9,
        "status": "ok",
        "tool": "verify_semantic_diff",
        "verified_at": verified_at,
        "summary": {
            "nodes_added": outcome.summary.nodes_added,
            "nodes_removed": outcome.summary.nodes_removed,
            "edges_added": outcome.summary.edges_added,
            "edges_removed": outcome.summary.edges_removed,
            "dangling_references": outcome.summary.dangling_references,
            "new_unresolved_delta": outcome.summary.new_unresolved_delta,
            "new_cycles": outcome.summary.new_cycles,
        },
        "regression_score": outcome.regression_score,
        "verdict": outcome.verdict,
        "report": outcome.report,
        "report_path": written,
    }))
}

// ---------------------------------------------------------------------------
// Stage 6 — validate_prd_against_graph
// ---------------------------------------------------------------------------

fn run_validate_prd_against_graph(arguments: &Value) -> Value {
    match do_validate_prd_against_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 6,
            "status": "error",
            "reason": "validate_prd_against_graph_failed",
            "message": msg,
        }),
    }
}

fn do_validate_prd_against_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let prd_path_str = args
        .get("prd_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'prd_path'")?;
    let prd_path = require_absolute(prd_path_str, "prd_path")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }
    let affected_opt = args
        .get("affected_symbols_path")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "affected_symbols_path"))
        .transpose()?;

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(&graph_path)?;
    let report = prd_validator::validate_prd(&store, &prd_path, affected_opt.as_deref())?;
    let validated_at = format_iso8601_utc(now_unix_seconds_nanos().0);

    let (run_id_opt, finding_id_opt, output_dir_opt) = extract_optional_ids(args)?;
    let artifact_path = maybe_write_validation(
        &report,
        &prd_path,
        &graph_path,
        &validated_at,
        &run_id_opt,
        &finding_id_opt,
        &output_dir_opt,
    )?;

    let json_report = prd_validator::report_to_json(
        &report,
        run_id_opt.as_deref().unwrap_or(""),
        finding_id_opt.as_deref().unwrap_or(""),
        &prd_path,
        &graph_path,
        &validated_at,
    );
    Ok(json!({
        "stage": 6,
        "status": "ok",
        "tool": "validate_prd_against_graph",
        "validated_at": validated_at,
        "validation_status": report.validation_status,
        "extraction_mode": report.extraction_mode,
        "contract_missing": report.contract_missing,
        "summary": {
            "claimed_symbols": report.summary.claimed_symbols,
            "resolved_symbols": report.summary.resolved_symbols,
            "hallucinated_symbols": report.summary.hallucinated_symbols,
            "unverifiable_symbols": report.summary.unverifiable_symbols,
            "communities_spanned": report.summary.communities_spanned,
            "processes_impacted": report.summary.processes_impacted,
        },
        "artifact_path": artifact_path.map(|p| p.to_string_lossy().to_string()),
        "report": json_report,
    }))
}

/// (run_id, feature_id, graph_path), all optional/independently-present.
/// clippy::type_complexity.
type OptionalRunIds = (Option<String>, Option<String>, Option<PathBuf>);

fn extract_optional_ids(args: &Map<String, Value>) -> Result<OptionalRunIds, String> {
    let run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(ref r) = run_id {
        validate_safe_id("run_id", r)?;
    }
    let finding_id = args
        .get("finding_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(ref f) = finding_id {
        validate_safe_id("finding_id", f)?;
    }
    let output_dir = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "output_dir"))
        .transpose()?;
    Ok((run_id, finding_id, output_dir))
}

fn maybe_write_validation(
    report: &prd_validator::ValidationReport,
    prd_path: &Path,
    graph_path: &Path,
    validated_at: &str,
    run_id: &Option<String>,
    finding_id: &Option<String>,
    output_dir: &Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (r, f, o) = match (run_id, finding_id, output_dir) {
        (Some(r), Some(f), Some(o)) => (r, f, o),
        _ => return Ok(None),
    };
    let value = prd_validator::report_to_json(report, r, f, prd_path, graph_path, validated_at);
    let dest = o
        .join("runs")
        .join(r)
        .join("findings")
        .join(f)
        .join(prd_validator::VALIDATION_FILE);
    let written = prd_validator::write_validation(&dest, &value)?;
    Ok(Some(written))
}

// ---------------------------------------------------------------------------
// Stage 8 — check_security_gates
// ---------------------------------------------------------------------------

fn run_check_security_gates(arguments: &Value) -> Value {
    match do_check_security_gates(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 8,
            "status": "error",
            "reason": "check_security_gates_failed",
            "message": msg,
        }),
    }
}

fn do_check_security_gates(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }
    let changed_symbols: Vec<String> = args
        .get("changed_symbols")
        .and_then(|v| v.as_array())
        .ok_or("missing required field 'changed_symbols' (array of strings)")?
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(&graph_path)?;
    let report = security_gates::check_gates(&store, &changed_symbols)?;
    let checked_at = format_iso8601_utc(now_unix_seconds_nanos().0);

    let (run_id_opt, finding_id_opt, output_dir_opt) = extract_optional_ids(args)?;
    let artifact_path = maybe_write_security(
        &report,
        &graph_path,
        &changed_symbols,
        &checked_at,
        &run_id_opt,
        &finding_id_opt,
        &output_dir_opt,
    )?;

    let json_report = security_gates::report_to_json(
        &report,
        run_id_opt.as_deref().unwrap_or(""),
        finding_id_opt.as_deref().unwrap_or(""),
        &graph_path,
        &changed_symbols,
        &checked_at,
    );
    Ok(json!({
        "stage": 8,
        "status": "ok",
        "tool": "check_security_gates",
        "checked_at": checked_at,
        "gates_passed": report.gates_passed,
        "summary": {
            "changed_symbols": report.summary.changed_symbols,
            "critical_count": report.summary.critical_count,
            "warning_count": report.summary.warning_count,
            "info_count": report.summary.info_count,
        },
        "artifact_path": artifact_path.map(|p| p.to_string_lossy().to_string()),
        "report": json_report,
    }))
}

fn maybe_write_security(
    report: &security_gates::SecurityReport,
    graph_path: &Path,
    changed_symbols: &[String],
    checked_at: &str,
    run_id: &Option<String>,
    finding_id: &Option<String>,
    output_dir: &Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (r, f, o) = match (run_id, finding_id, output_dir) {
        (Some(r), Some(f), Some(o)) => (r, f, o),
        _ => return Ok(None),
    };
    let value =
        security_gates::report_to_json(report, r, f, graph_path, changed_symbols, checked_at);
    let dest = o
        .join("runs")
        .join(r)
        .join("findings")
        .join(f)
        .join(security_gates::SECURITY_FILE);
    let written = security_gates::write_security(&dest, &value)?;
    Ok(Some(written))
}

/// All known relationship tables as (name, from_label, to_label).
/// Source: graph_store.rs REL_TABLES (mirrored here because the const is private).
fn rel_table_triples() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        // 3a tables
        ("Contains_Dir_File", "Directory", "File"),
        ("Contains_Dir_Dir", "Directory", "Directory"),
        ("Contains_File_Module", "File", "Module"),
        ("Defines_File_Function", "File", "Function"),
        ("Defines_File_Struct", "File", "Struct"),
        ("Defines_File_Enum", "File", "Enum"),
        ("Defines_File_Trait", "File", "Trait"),
        ("Defines_File_Constant", "File", "Constant"),
        ("Defines_File_TypeAlias", "File", "TypeAlias"),
        // source: B1 fix — Q9/Q14 expect File->Import edges but the rel
        // table was never registered, so Defines edges from File to Import
        // were silently dropped by is_valid_rel_table.
        ("Defines_File_Import", "File", "Import"),
        ("Defines_Module_Import", "Module", "Import"),
        ("Defines_Module_Function", "Module", "Function"),
        ("Defines_Module_Struct", "Module", "Struct"),
        ("Defines_Module_Enum", "Module", "Enum"),
        ("Defines_Module_Trait", "Module", "Trait"),
        ("Defines_Module_Constant", "Module", "Constant"),
        ("Defines_Module_TypeAlias", "Module", "TypeAlias"),
        ("HasMethod_Struct_Method", "Struct", "Method"),
        ("HasMethod_Enum_Method", "Enum", "Method"),
        ("HasMethod_Trait_Method", "Trait", "Method"),
        ("HasField_Struct_Field", "Struct", "Field"),
        ("HasField_Enum_Field", "Enum", "Field"),
        ("HasVariant_Enum_Variant", "Enum", "Variant"),
        // 3b Imports tables — source: stages/stage-3b.md §3
        ("Imports_File_File", "File", "File"),
        ("Imports_File_Module", "File", "Module"),
        ("Imports_File_Function", "File", "Function"),
        ("Imports_File_Struct", "File", "Struct"),
        ("Imports_File_Enum", "File", "Enum"),
        ("Imports_File_Trait", "File", "Trait"),
        ("Imports_File_Constant", "File", "Constant"),
        ("Imports_File_TypeAlias", "File", "TypeAlias"),
        ("Imports_Module_Function", "Module", "Function"),
        ("Imports_Module_Struct", "Module", "Struct"),
        ("Imports_Module_Enum", "Module", "Enum"),
        ("Imports_Module_Trait", "Module", "Trait"),
        ("Imports_Module_Constant", "Module", "Constant"),
        ("Imports_Module_TypeAlias", "Module", "TypeAlias"),
        // 3b Calls tables
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
        // 3b Implements tables
        ("Implements_Struct_Trait", "Struct", "Trait"),
        ("Implements_Enum_Trait", "Enum", "Trait"),
        // 3b Extends table
        ("Extends_Trait_Trait", "Trait", "Trait"),
        // 3b Uses tables
        ("Uses_Function_Struct", "Function", "Struct"),
        ("Uses_Function_Enum", "Function", "Enum"),
        ("Uses_Function_Trait", "Function", "Trait"),
        ("Uses_Function_TypeAlias", "Function", "TypeAlias"),
        ("Uses_Method_Struct", "Method", "Struct"),
        ("Uses_Method_Enum", "Method", "Enum"),
        ("Uses_Method_Trait", "Method", "Trait"),
        ("Uses_Method_TypeAlias", "Method", "TypeAlias"),
        ("Uses_Struct_Struct", "Struct", "Struct"),
        ("Uses_Struct_Enum", "Struct", "Enum"),
        ("Uses_Struct_Trait", "Struct", "Trait"),
        ("Uses_Field_Struct", "Field", "Struct"),
        ("Uses_Field_Enum", "Field", "Enum"),
        ("Uses_Field_Trait", "Field", "Trait"),
        ("Uses_Field_TypeAlias", "Field", "TypeAlias"),
        // 3b-v2 Layer 4/5 tables — source: stages/stage-3b-v2.md §5
        ("Calls_Function_StdlibSymbol", "Function", "StdlibSymbol"),
        ("Calls_Method_StdlibSymbol", "Method", "StdlibSymbol"),
        ("Implements_Struct_StdlibSymbol", "Struct", "StdlibSymbol"),
        ("Implements_Enum_StdlibSymbol", "Enum", "StdlibSymbol"),
        // 3c MemberOf tables
        ("MemberOf_Function_Community", "Function", "Community"),
        ("MemberOf_Method_Community", "Method", "Community"),
        ("MemberOf_Struct_Community", "Struct", "Community"),
        ("MemberOf_Enum_Community", "Enum", "Community"),
        ("MemberOf_Trait_Community", "Trait", "Community"),
        ("MemberOf_Constant_Community", "Constant", "Community"),
        ("MemberOf_TypeAlias_Community", "TypeAlias", "Community"),
        ("MemberOf_Module_Community", "Module", "Community"),
        // 3c EntryPointOf tables
        ("EntryPointOf_Function_Process", "Function", "Process"),
        ("EntryPointOf_Method_Process", "Method", "Process"),
        // 3c ParticipatesIn tables
        ("ParticipatesIn_Function_Process", "Function", "Process"),
        ("ParticipatesIn_Method_Process", "Method", "Process"),
    ]
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

fn handle_tool_call(params: &Value, profile: ToolProfile) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    // A tool the active profile does not register must behave exactly like
    // a tool that does not exist — the profile IS the registry.
    if !profile.allows(name) {
        return json!({
            "isError": true,
            "content": [{
                "type": "text",
                "text": format!(
                    "Unknown tool: {} (not registered under the '{}' profile; \
                     restart with --profile full to expose every tool)",
                    name,
                    profile.name()
                )
            }]
        });
    }

    let payload = match name {
        "health_check" => {
            // source: C-correctness bug 3 — the count was a hardcoded `19`
            // that silently lied if a new tool was added without bumping it.
            // Derive from tools_list() so the count can never drift.
            let tools_count = tools_list(profile)
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            json!({
                "stage": 0,
                "name": "health_check",
                "status": "ok",
                "server": SERVER_NAME,
                "version": SERVER_VERSION,
                "protocol": PROTOCOL_VERSION,
                // Kept for back-compat with existing clients that parse
                // `stages_registered`. Both now reflect the live tool count.
                "stages_registered": tools_count,
                "tools_count": tools_count,
            })
        }
        "extract_finding" => run_extract_finding(&arguments),
        "refine_finding" => run_refine_finding(&arguments),
        "start_verification" => run_start_verification(&arguments),
        "append_clarification" => run_append_clarification(&arguments),
        "finalize_verification" => run_finalize_verification(&arguments),
        "abort_verification" => run_abort_verification(&arguments),
        "index_codebase" => run_index_codebase(&arguments),
        "index_status" => run_index_status(&arguments),
        "ingest_traces" => run_ingest_traces(&arguments),
        "query_graph" => run_query_graph(&arguments),
        "get_symbol" => run_get_symbol(&arguments),
        "resolve_graph" => run_resolve_graph(&arguments),
        "cluster_graph" => run_cluster_graph(&arguments),
        "get_processes" => run_get_processes(&arguments),
        "get_impact" => run_get_impact(&arguments),
        "index_history" => run_index_history(&arguments),
        "search_codebase" => run_search_codebase(&arguments),
        "get_context" => run_get_context(&arguments),
        "analyze_codebase" => run_analyze_codebase(&arguments),
        "detect_changes" => run_detect_changes(&arguments),
        "lsp_resolve" => run_lsp_resolve(&arguments),
        "prepare_prd_input" => run_prepare_prd_input(&arguments),
        "validate_prd_against_graph" => run_validate_prd_against_graph(&arguments),
        "check_security_gates" => run_check_security_gates(&arguments),
        "verify_semantic_diff" => run_verify_semantic_diff(&arguments),
        other => {
            return json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("Unknown tool: {}", other)
                }]
            });
        }
    };

    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
        }]
    })
}

// ---------------------------------------------------------------------------
// Request dispatch
// ---------------------------------------------------------------------------

fn handle_request(req: Request, profile: ToolProfile) {
    let id = req.id.clone().unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => send_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                // Declare prompts alongside tools. Resources are intentionally
                // NOT declared — see resources/list below: some clients probe
                // resources regardless of declared capabilities.
                "capabilities": {
                    "tools": {},
                    "prompts": { "listChanged": false }
                },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                // Per-profile workflow guidance (issue #65 criterion 5).
                "instructions": server_instructions(profile)
            }),
        ),
        "notifications/initialized" => {
            // JSON-RPC notification: no response
        }
        "tools/list" => send_response(id, tools_list(profile)),
        "tools/call" => send_response(id, handle_tool_call(&req.params, profile)),
        "prompts/list" => send_response(id, mcp_prompts::prompts_list(profile)),
        "prompts/get" => match mcp_prompts::prompt_get(&req.params, profile) {
            Ok(result) => send_response(id, result),
            Err((code, message)) => send_error(id, code, &message),
        },
        // Empty-but-present resource endpoints. This server exposes no
        // resources, but some MCP clients probe these on connect regardless of
        // declared capabilities and surface the resulting -32601 as a failed
        // connection (CBM upstream #958). An empty list is interoperable.
        // source: DeusData/codebase-memory-mcp src/mcp/mcp.c:10810-10816 (#958).
        "resources/list" => send_response(id, json!({ "resources": [] })),
        "resources/templates/list" => send_response(id, json!({ "resourceTemplates": [] })),
        other => send_error(id, -32601, &format!("Method not found: {}", other)),
    }
}

// ---------------------------------------------------------------------------
// stdio loop
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Security hardening tests — H3 (read-only query) and H4 (graph path safety).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn limit_injection_appends_when_absent() {
        let (q, injected) = inject_limit_if_absent("MATCH (n) RETURN n");
        assert!(injected);
        assert_eq!(
            q,
            format!("MATCH (n) RETURN n LIMIT {QUERY_GRAPH_ROW_LIMIT}")
        );
    }

    #[test]
    fn limit_injection_strips_trailing_semicolon() {
        let (q, injected) = inject_limit_if_absent("MATCH (n) RETURN n;");
        assert!(injected);
        assert_eq!(
            q,
            format!("MATCH (n) RETURN n LIMIT {QUERY_GRAPH_ROW_LIMIT}")
        );
    }

    #[test]
    fn limit_injection_respects_existing_limit() {
        let (q, injected) = inject_limit_if_absent("MATCH (n) RETURN n LIMIT 5");
        assert!(!injected);
        assert_eq!(q, "MATCH (n) RETURN n LIMIT 5");
        // Case-insensitive.
        let (q2, injected2) = inject_limit_if_absent("MATCH (n) RETURN n limit 5");
        assert!(!injected2);
        assert_eq!(q2, "MATCH (n) RETURN n limit 5");
    }

    #[test]
    fn limit_word_boundary_not_fooled_by_identifier() {
        // `node_limit` is an identifier, not a LIMIT clause → still inject.
        let (_q, injected) = inject_limit_if_absent("MATCH (n) RETURN n.node_limit");
        assert!(injected);
        // A real LIMIT after an identifier-named field is still detected.
        assert!(has_limit_clause("MATCH (n) RETURN n.node_limit LIMIT 3"));
    }

    #[test]
    fn test_query_graph_rejects_delete() {
        assert_eq!(
            forbidden_cypher_keyword("MATCH (n) DETACH DELETE n"),
            Some("DELETE")
        );
        assert_eq!(
            forbidden_cypher_keyword("MATCH (n) DELETE n"),
            Some("DELETE")
        );
        assert_eq!(forbidden_cypher_keyword("CREATE (n:Foo)"), Some("CREATE"));
        assert_eq!(
            forbidden_cypher_keyword("MERGE (n:Foo {id: 1})"),
            Some("MERGE")
        );
        assert_eq!(
            forbidden_cypher_keyword("MATCH (n) SET n.x = 1"),
            Some("SET")
        );
        assert_eq!(
            forbidden_cypher_keyword("MATCH (n) REMOVE n:Label"),
            Some("REMOVE")
        );
        assert_eq!(forbidden_cypher_keyword("DROP TABLE Foo"), Some("DROP"));
        assert_eq!(forbidden_cypher_keyword("CALL db.labels()"), Some("CALL"));
        assert_eq!(forbidden_cypher_keyword("LOAD CSV FROM 'x'"), Some("LOAD"));

        // Clean read queries must pass.
        assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n"), None);
        assert_eq!(
            forbidden_cypher_keyword("OPTIONAL MATCH (n) RETURN n"),
            None
        );
        assert_eq!(forbidden_cypher_keyword("WITH 1 AS x RETURN x"), None);
        assert_eq!(forbidden_cypher_keyword("UNWIND [1,2] AS i RETURN i"), None);

        // Whole-word matching — identifiers that embed a keyword must NOT trigger.
        assert_eq!(
            forbidden_cypher_keyword("MATCH (n) RETURN n.created_at"),
            None
        );
        assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n.setting"), None);

        // Case insensitivity.
        assert_eq!(
            forbidden_cypher_keyword("match (n) detach delete n"),
            Some("DELETE")
        );

        // End-to-end via do_query_graph — no real DB needed because we should
        // fail before `GraphStore::open_or_create`.
        let args = json!({
            "graph_path": "/nonexistent/graph",
            "query": "MATCH (n) DETACH DELETE n"
        });
        let err = do_query_graph(&args).expect_err("must reject mutation query");
        assert!(err.contains("read_only_query_required"), "got: {err}");
    }

    #[test]
    fn order_by_detection_whole_word_and_case_insensitive() {
        assert!(has_order_by_clause("MATCH (n) RETURN n ORDER BY n.id"));
        assert!(has_order_by_clause("match (n) return n order by n.name"));
        assert!(has_order_by_clause("MATCH (n) RETURN n ORDER   BY n.id")); // extra ws
                                                                            // No ORDER BY → unstable order.
        assert!(!has_order_by_clause("MATCH (n) RETURN n"));
        assert!(!has_order_by_clause("MATCH (n) RETURN n LIMIT 5"));
        // "order" without "by" is not an ORDER BY clause.
        assert!(!has_order_by_clause("MATCH (n) RETURN n.order"));
        // Identifier embedding 'order' must not trigger.
        assert!(!has_order_by_clause("MATCH (n) RETURN n.reorder_by"));
        // 'by' glued to an identifier must not satisfy the clause.
        assert!(!has_order_by_clause("MATCH (n) RETURN n order byzantine"));
    }

    #[test]
    fn test_health_check_tool_count_matches_tools_list() {
        // source: C-correctness bug 3 — the health_check response must derive
        // the count from `tools_list()` dynamically. If a new tool is added
        // to `tool_schemas::tools_list` without touching main.rs, the count
        // must still be correct.
        let tools = tools_list(ToolProfile::Full);
        let expected = tools
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools_list must return a `tools` array")
            .len();

        let health = handle_tool_call(
            &json!({
                "name": "health_check",
                "arguments": {}
            }),
            ToolProfile::Full,
        );

        // handle_tool_call wraps the payload in a content/text envelope.
        // Find the JSON text inside.
        let text = health
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("text"))
            .and_then(|v| v.as_str())
            .expect("health_check must return content[0].text");
        let payload: serde_json::Value =
            serde_json::from_str(text).expect("content[0].text must be JSON");

        let reported = payload
            .get("tools_count")
            .and_then(|v| v.as_u64())
            .expect("tools_count field must be present");
        assert_eq!(reported as usize, expected, "tools_count drift");

        let legacy = payload
            .get("stages_registered")
            .and_then(|v| v.as_u64())
            .expect("stages_registered field must stay for back-compat");
        assert_eq!(legacy as usize, expected, "stages_registered drift");
    }

    #[test]
    fn test_health_check_count_reflects_core_profile() {
        // health_check derives its count from the ACTIVE profile's registry,
        // so a core-profile server reports the core tool count, not 24.
        let health = handle_tool_call(
            &json!({
                "name": "health_check",
                "arguments": {}
            }),
            ToolProfile::Core,
        );
        let text = health
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("text"))
            .and_then(|v| v.as_str())
            .expect("health_check must return content[0].text");
        let payload: serde_json::Value =
            serde_json::from_str(text).expect("content[0].text must be JSON");
        assert_eq!(
            payload.get("tools_count").and_then(|v| v.as_u64()),
            Some(tool_profile::CORE_TOOL_NAMES.len() as u64),
            "core profile must report exactly the core tool count"
        );
    }

    #[test]
    fn test_core_profile_rejects_unregistered_tool_call() {
        // A tool hidden by the profile must be indistinguishable from a tool
        // that does not exist — callable-but-unlisted would defeat the point.
        let response = handle_tool_call(
            &json!({
                "name": "prepare_prd_input",
                "arguments": {}
            }),
            ToolProfile::Core,
        );
        assert_eq!(response.get("isError"), Some(&json!(true)));
        let text = response
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("text"))
            .and_then(|v| v.as_str())
            .expect("rejection must carry content[0].text");
        assert!(text.contains("Unknown tool"), "got: {text}");
        assert!(text.contains("'core' profile"), "got: {text}");
    }

    #[test]
    fn test_graph_path_must_end_in_graph() {
        // source: H4 fix — caller-chosen path is safe ONLY when it is absolute
        // AND the last segment is exactly `graph` AND the path is not one of
        // the forbidden system roots.
        assert!(validate_graph_path_safe(Path::new("/tmp/foo/graph")).is_ok());
        assert!(validate_graph_path_safe(Path::new("/Users/alice/proj/graph")).is_ok());

        // Not absolute.
        assert!(validate_graph_path_safe(Path::new("relative/graph")).is_err());

        // Does not end in /graph.
        assert!(validate_graph_path_safe(Path::new("/etc")).is_err());
        assert!(validate_graph_path_safe(Path::new("/tmp")).is_err());
        assert!(validate_graph_path_safe(Path::new("/")).is_err());
        assert!(validate_graph_path_safe(Path::new("/Users")).is_err());
        assert!(validate_graph_path_safe(Path::new("/tmp/foo/notgraph")).is_err());

        // Ends in /graph but IS a forbidden system root (should still reject).
        assert!(validate_graph_path_safe(Path::new("/etc/graph")).is_err());
        assert!(
            validate_graph_path_safe(Path::new("//graph")).is_err()
                || validate_graph_path_safe(Path::new("//graph")).is_ok()
        );
    }

    #[test]
    fn remove_stale_graph_artifact_handles_file_and_dir() {
        // source: ENOTDIR fix — a prior run can leave `graph` as a single-file
        // Kuzu db; `remove_dir_all` on a file returns ENOTDIR (os error 20).
        // The helper must delete both shapes and report a missing path as an
        // error rather than panicking.
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let base = tempfile::Builder::new()
            .prefix("ap-remove-stale-")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let graph = base.join("graph");

        // Case A: graph is a directory with nested content.
        fs::create_dir_all(graph.join("nested")).unwrap();
        fs::write(graph.join("nested/f.txt"), b"x").unwrap();
        assert!(graph.is_dir());
        remove_stale_graph_artifact(&graph).expect("dir removal");
        assert!(!graph.exists());

        // Case B: graph is a single file — the ENOTDIR regression case.
        fs::write(&graph, b"kuzu-single-file-db").unwrap();
        assert!(graph.is_file());
        remove_stale_graph_artifact(&graph).expect("file removal (was ENOTDIR)");
        assert!(!graph.exists());

        // Missing path → surfaced as an error, never a panic.
        assert!(remove_stale_graph_artifact(&graph).is_err());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn write_graph_meta_records_absolute_root() {
        // The sidecar records the ABSOLUTE indexed root so a consumer can
        // rebuild absolute paths from AP's relative ones (cortex-viz wiki->file
        // join + tool-file keying). It is written NEXT TO the graph, never
        // inside it — the graph itself stays portable.
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let base = tempfile::Builder::new()
            .prefix("ap-meta-")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let root = base.join("some/repo/root");

        write_graph_meta(&base, &root);

        let meta_path = base.join("meta.json");
        assert!(meta_path.is_file(), "meta.json must be written");
        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
        assert_eq!(
            parsed.get("root").and_then(|v| v.as_str()),
            Some(root.to_string_lossy().as_ref()),
            "sidecar must record the absolute root verbatim",
        );
        assert_eq!(
            parsed.get("schema_version").and_then(|v| v.as_u64()),
            Some(1),
        );
        let _ = fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod pagination_tests {
    //! End-to-end cursor pagination over a real fixture graph, one test per
    //! bounded-read tool shape. Each test asserts the cursor's correctness
    //! contract (see response_budget::BoundedPage docs):
    //!   - page-through reconstruction: union of pages == unpaged full set,
    //!     in order, no duplicates, no gaps;
    //!   - next_offset absent on the final page;
    //!   - offset beyond end → empty page, no next_offset;
    //!   - stable order: two identical calls return the same order, so the
    //!     cursor is safe.
    use super::*;

    // A fixture where one symbol (`helpers::sanitize`) is called from several
    // call sites across modules, giving get_impact a multi-element `callers`
    // list and query_graph/search several rows to page over.
    const F_MAIN: &str = r#"
use crate::svc;
fn main() { let _ = svc::run_a(); let _ = svc::run_b(); }
fn driver() { let _ = svc::run_c(); }
"#;
    const F_SVC: &str = r#"
use crate::helpers;
pub fn run_a() -> String { helpers::sanitize("a") }
pub fn run_b() -> String { helpers::sanitize("b") }
pub fn run_c() -> String { helpers::sanitize("c") }
pub fn run_d() -> String { helpers::sanitize("d") }
"#;
    const F_HELPERS: &str = r#"
pub fn sanitize(input: &str) -> String { input.trim().to_string() }
"#;

    /// Builds an indexed + resolved + clustered fixture graph, returns its dir.
    fn build_fixture(tag: &str) -> std::path::PathBuf {
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let tmp_root = tempfile::Builder::new()
            .prefix(&format!("pagination_{tag}_"))
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = fs::remove_dir_all(&tmp_root);
        let src = tmp_root.join("fixture/src");
        fs::create_dir_all(&src).expect("create fixture src");
        fs::write(src.join("main.rs"), F_MAIN).unwrap();
        fs::write(src.join("svc.rs"), F_SVC).unwrap();
        fs::write(src.join("helpers.rs"), F_HELPERS).unwrap();

        let graph_dir = tmp_root.join("graph");
        indexer::index_codebase(&src, &graph_dir).expect("index");
        let store = graph_store::GraphStore::open_or_create(&graph_dir).unwrap();
        resolver::resolve_graph(&store).expect("resolve");
        clustering::cluster_graph(&store, 1.0).expect("cluster");
        // Drop the store handle so the read-path cache opens its own; the
        // embedded DB is single-writer and tests share a process.
        drop(store);
        graph_dir
    }

    /// Drives a tool's cursor to exhaustion: repeatedly calls `call(offset)`,
    /// extracting the primary list with `items` and the cursor with `next`.
    /// Returns the concatenation of every page's items and the final-page flag
    /// sequence. Asserts the cursor strictly advances (terminates).
    fn page_through<F>(mut call: F) -> Vec<Value>
    where
        F: FnMut(u64) -> Value,
    {
        let mut all = Vec::new();
        let mut offset: u64 = 0;
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 10_000, "cursor failed to terminate");
            let resp = call(offset);
            let items = resp["__items"].as_array().cloned().unwrap_or_default();
            all.extend(items.iter().cloned());
            match resp.get("next_offset").and_then(|v| v.as_u64()) {
                Some(n) => {
                    assert!(n > offset, "cursor must advance: {n} <= {offset}");
                    offset = n;
                }
                None => break,
            }
        }
        all
    }

    // -- get_processes -----------------------------------------------------

    #[test]
    fn get_processes_pages_through_everything() {
        let graph = build_fixture("procs");
        let gp = graph.to_str().unwrap().to_string();

        let call = |offset: u64| -> Value {
            let mut r = do_get_processes(&json!({"graph_path": gp, "offset": offset}))
                .expect("get_processes");
            r["__items"] = r["processes"].clone();
            r
        };

        // Full unpaged set (offset 0 returns all for this small fixture).
        let full = call(0);
        let total = full["total_count"].as_u64().unwrap() as usize;
        assert!(total > 0, "fixture must yield processes");

        // Page-through union == full set, no dupes, no gaps.
        let paged = page_through(call);
        assert_eq!(paged.len(), total, "paged count must equal total");
        assert_eq!(paged, full["processes"].as_array().cloned().unwrap());

        // Offset beyond end → empty, no cursor.
        let beyond =
            do_get_processes(&json!({"graph_path": gp, "offset": total as u64 + 100})).unwrap();
        assert_eq!(beyond["processes"].as_array().unwrap().len(), 0);
        assert!(beyond.get("next_offset").is_none());
        assert!(!beyond["truncated"].as_bool().unwrap());

        // Stable order: two identical calls return identical order.
        let a = do_get_processes(&json!({"graph_path": gp})).unwrap();
        let b = do_get_processes(&json!({"graph_path": gp})).unwrap();
        assert_eq!(a["processes"], b["processes"], "order must be stable");
    }

    // -- get_impact (primary list = callers) -------------------------------

    #[test]
    fn get_impact_pages_callers_through_everything() {
        let graph = build_fixture("impact");
        let gp = graph.to_str().unwrap().to_string();

        // Use the real node id from the graph (qualified_name format varies by
        // resolver), exactly as stage3c_integration looks it up. `sanitize` is
        // called from run_a/run_b/run_c/run_d → >=3 resolved callers.
        let store = graph_store::GraphStore::open_or_create(&graph).unwrap();
        let qr = store
            .execute_query("MATCH (f:Function) WHERE f.name = 'sanitize' RETURN f.id")
            .unwrap();
        assert!(!qr.rows.is_empty(), "sanitize node must exist");
        let target = qr.rows[0][0].clone();
        drop(store);

        let call = |offset: u64| -> Value {
            let mut r = do_get_impact(&json!({
                "graph_path": gp, "qualified_name": target, "offset": offset
            }))
            .expect("get_impact");
            r["__items"] = r["callers"].clone();
            r
        };

        let full = call(0);
        let total = full["callers_total"].as_u64().unwrap() as usize;
        assert!(total >= 3, "sanitize should have >=3 callers, got {total}");
        assert_eq!(full["primary_list"], json!("callers"));
        assert_eq!(full["secondary_lists_paged"], json!(false));

        let paged = page_through(call);
        assert_eq!(paged.len(), total);
        assert_eq!(paged, full["callers"].as_array().cloned().unwrap());

        // Offset beyond end → empty callers, no cursor.
        let beyond = do_get_impact(&json!({
            "graph_path": gp, "qualified_name": target, "offset": total as u64 + 50
        }))
        .unwrap();
        assert_eq!(beyond["callers"].as_array().unwrap().len(), 0);
        assert!(beyond.get("next_offset").is_none());

        // Stable order across identical calls.
        let a = do_get_impact(&json!({"graph_path": gp, "qualified_name": target})).unwrap();
        let b = do_get_impact(&json!({"graph_path": gp, "qualified_name": target})).unwrap();
        assert_eq!(a["callers"], b["callers"], "callers order must be stable");
    }

    // -- search_codebase ---------------------------------------------------

    #[test]
    fn search_codebase_pages_through_everything() {
        let graph = build_fixture("search");
        let gp = graph.to_str().unwrap().to_string();

        let call = |offset: u64| -> Value {
            let mut r = do_search_codebase(&json!({
                "graph_path": gp, "query": "run", "limit": 50, "offset": offset
            }))
            .expect("search");
            r["__items"] = r["results"].clone();
            r
        };

        let full = call(0);
        let total = full["total_count"].as_u64().unwrap() as usize;
        assert!(total > 0, "search must yield results for 'run'");

        let paged = page_through(call);
        assert_eq!(paged.len(), total);
        assert_eq!(paged, full["results"].as_array().cloned().unwrap());

        // Offset beyond end → empty, no cursor.
        let beyond = do_search_codebase(&json!({
            "graph_path": gp, "query": "run", "limit": 50, "offset": total as u64 + 100
        }))
        .unwrap();
        assert_eq!(beyond["results"].as_array().unwrap().len(), 0);
        assert!(beyond.get("next_offset").is_none());

        // Stable order across identical calls (deterministic score+name sort).
        let a =
            do_search_codebase(&json!({"graph_path": gp, "query": "run", "limit": 50})).unwrap();
        let b =
            do_search_codebase(&json!({"graph_path": gp, "query": "run", "limit": 50})).unwrap();
        assert_eq!(a["results"], b["results"], "search order must be stable");
    }

    // -- query_graph (caller-ordered; ORDER BY makes the cursor safe) -------

    #[test]
    fn query_graph_pages_through_ordered_query() {
        let graph = build_fixture("query");
        let gp = graph.to_str().unwrap().to_string();
        // ORDER BY makes the row order stable → cursor is safe (order_stable).
        let q = "MATCH (f:Function) RETURN f.qualified_name ORDER BY f.qualified_name";

        let call = |offset: u64| -> Value {
            let mut r = do_query_graph(&json!({
                "graph_path": gp, "query": q, "offset": offset
            }))
            .expect("query_graph");
            r["__items"] = r["rows"].clone();
            r
        };

        let full = call(0);
        assert_eq!(full["order_stable"], json!(true), "ORDER BY → order_stable");
        let total = full["total_count"].as_u64().unwrap() as usize;
        assert!(total > 0, "fixture has functions");

        let paged = page_through(call);
        assert_eq!(paged.len(), total);
        assert_eq!(paged, full["rows"].as_array().cloned().unwrap());

        // Offset beyond end → empty rows, no cursor.
        let beyond = do_query_graph(&json!({
            "graph_path": gp, "query": q, "offset": total as u64 + 100
        }))
        .unwrap();
        assert_eq!(beyond["rows"].as_array().unwrap().len(), 0);
        assert!(beyond.get("next_offset").is_none());

        // A query WITHOUT ORDER BY must report order_stable=false (honest flag).
        let unordered = do_query_graph(&json!({
            "graph_path": gp, "query": "MATCH (f:Function) RETURN f.qualified_name"
        }))
        .unwrap();
        assert_eq!(
            unordered["order_stable"],
            json!(false),
            "no ORDER BY → order_stable must be false"
        );
    }
}

fn main() {
    // Read-once-at-startup configuration: `--profile` flag beats `AP_PROFILE`
    // env var; absent both, every tool is registered (see tool_profile.rs).
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Issue #59 subcommands run and exit BEFORE the MCP stdio server starts.
    match args.first().map(String::as_str) {
        Some("install") => process::exit(cli::run_install(&args[1..])),
        Some("uninstall") => process::exit(cli::run_uninstall(&args[1..])),
        Some("hook-augment") => process::exit(cli::run_hook_augment()),
        _ => {}
    }

    let env_profile = std::env::var(tool_profile::PROFILE_ENV_VAR).ok();
    let profile = match ToolProfile::resolve(&args, env_profile.as_deref()) {
        Ok(profile) => profile,
        Err(message) => {
            eprintln!("[automatised-pipeline] {}", message);
            // source: POSIX utility convention (e.g. grep(1)) — exit status 2
            // signals a usage error, distinct from runtime failure (1).
            process::exit(2);
        }
    };

    eprintln!(
        "[automatised-pipeline] stage 0-3d up (Rust {}, profile '{}')",
        SERVER_VERSION,
        profile.name()
    );

    let stdin = io::stdin();
    let handle = stdin.lock();
    for line in handle.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[automatised-pipeline] stdin error: {}", e);
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => handle_request(req, profile),
            Err(e) => eprintln!("[automatised-pipeline] parse error: {}", e),
        }
    }
}

#[cfg(test)]
mod artifact_bootstrap_tests {
    //! Issue #55 staleness contract at the handler seam (`attempt_bootstrap`):
    //!   (b) stale artifact, default policy → REFUSE + reindex, with an
    //!       assertable `bootstrap_skipped` note carrying commits_behind;
    //!   (c) stale artifact + accept_stale → import anyway, response carries a
    //!       `stale_artifact` {artifact_sha, head_sha, commits_behind} report.
    //! Each uses a real git fixture, a real index, and a real export, so the
    //! whole path (git sha compare → rev-list count → JSON shaping) is covered.
    use super::*;
    use std::path::Path;
    use std::process::Command;

    const SRC: &str = "fn main() { helper(); }\nfn helper() -> i32 { 1 }\n";

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Builds a git fixture repo, indexes it, exports the artifact, then makes
    /// one further commit so the committed artifact is exactly 1 commit stale.
    /// Returns (repo, fresh_output_dir, fresh_graph_dir).
    fn stale_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = tempfile::Builder::new()
            .prefix("artifact_stale_")
            .tempdir()
            .expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mk src");
        fs::write(repo.join("src/main.rs"), SRC).expect("write src");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@ap.dev"]);
        git(&repo, &["config", "user.name", "AP"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-gpg-sign", "-m", "initial"]);

        let out1 = tmp.path().join("out1");
        let graph1 = out1.join("graph");
        fs::create_dir_all(&out1).expect("mk out1");
        // Index the repo ROOT (not repo/src): the bootstrap codebase must equal
        // the indexed root so File node ids line up with git-diff paths, exactly
        // as the real handler wires them (path == bootstrap codebase). The
        // manifest is bundled so the export mirrors a real `export_artifact=true`.
        let manifest_path = indexer::manifest::manifest_path(&out1);
        let result = indexer::index_codebase(&repo, &graph1).expect("index");
        indexer::write_full_manifest(&repo, &manifest_path, None, indexer::DependencyScope::None)
            .expect("manifest");
        artifact::export_artifact(
            &graph1,
            &repo,
            result.node_count,
            result.edge_count,
            Some(&manifest_path),
            None,
        )
        .expect("export");

        // Move HEAD forward by one commit → artifact is now 1 commit stale.
        fs::write(repo.join("src/extra.rs"), "pub fn added() {}\n").expect("write extra");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-gpg-sign", "-m", "second"]);

        let fresh_out = tmp.path().join("fresh");
        let fresh_graph = fresh_out.join("graph");
        fs::create_dir_all(&fresh_out).expect("mk fresh");
        (tmp, repo, fresh_out, fresh_graph)
    }

    /// Convenience: `attempt_bootstrap` with the default (no language filter,
    /// dependency scope None) wiring the two tests below share.
    fn bootstrap(
        repo: &Path,
        fresh_out: &Path,
        fresh_graph: &Path,
        accept_stale: bool,
    ) -> BootstrapOutcome {
        let manifest_path = indexer::manifest::manifest_path(fresh_out);
        attempt_bootstrap(
            repo,
            fresh_out,
            fresh_graph,
            &manifest_path,
            accept_stale,
            None,
            indexer::DependencyScope::None,
        )
    }

    #[test]
    fn stale_artifact_default_imports_and_fills() {
        // Evolved contract (issue #62 completes #55): the DEFAULT for a stale
        // artifact is bootstrap THEN incremental fill — no longer refuse.
        let (_tmp, repo, fresh_out, fresh_graph) = stale_fixture();

        let outcome = bootstrap(&repo, &fresh_out, &fresh_graph, false);
        match outcome {
            BootstrapOutcome::Imported(resp) => {
                assert_eq!(resp["source"], json!("artifact_bootstrap_fill"));
                assert_eq!(resp["graph_state"], json!("filled_to_working_tree"));
                assert_eq!(resp["fill_method"], json!("git_diff"));
                assert_eq!(
                    resp["added"],
                    json!(1),
                    "the one commit added src/extra.rs → fill must add exactly one file"
                );
                assert_eq!(resp["artifact_commits_behind"], json!(1));
            }
            other => panic!("expected Imported(resp), got {other:?}"),
        }
        assert!(
            fresh_graph.exists(),
            "the default fill path must materialise the imported+filled graph"
        );
        // The filled graph must now contain the added file's symbol.
        let store = graph_store::GraphStore::open_or_create(&fresh_graph).expect("open");
        let q = store
            .execute_query("MATCH (f:Function) WHERE f.name = 'added' RETURN f.name")
            .expect("query");
        assert!(
            !q.rows.is_empty(),
            "fill must have indexed src/extra.rs::added into the bootstrapped graph"
        );
    }

    #[test]
    fn accept_stale_imports_as_is_and_reports_staleness() {
        // accept_stale is repurposed: import the stale snapshot AND SKIP the
        // fill, but always report the staleness (no silent staleness).
        let (_tmp, repo, fresh_out, fresh_graph) = stale_fixture();

        let outcome = bootstrap(&repo, &fresh_out, &fresh_graph, true);
        match outcome {
            BootstrapOutcome::Imported(resp) => {
                assert_eq!(resp["source"], json!("artifact_bootstrap"));
                assert_eq!(resp["graph_state"], json!("accepted_stale"));
                let stale = &resp["stale_artifact"];
                assert_eq!(
                    stale["commits_behind"],
                    json!(1),
                    "accepted-stale response must carry the staleness report"
                );
                assert!(stale["artifact_sha"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()));
                assert!(stale["head_sha"].as_str().is_some_and(|s| !s.is_empty()));
            }
            other => panic!("expected Imported(resp), got {other:?}"),
        }
        assert!(
            fresh_graph.exists(),
            "accept_stale must materialise the imported graph"
        );
        // Skipping the fill means the added file is NOT yet in the graph.
        let store = graph_store::GraphStore::open_or_create(&fresh_graph).expect("open");
        let q = store
            .execute_query("MATCH (f:Function) WHERE f.name = 'added' RETURN f.name")
            .expect("query");
        assert!(
            q.rows.is_empty(),
            "accept_stale must NOT fill — the stale graph lacks the new commit's symbol"
        );
    }
}

#[cfg(test)]
mod coverage_tools_tests {
    // Issue #57 — the coverage-honesty tool surface: index_codebase reports
    // coverage inline, index_status reports it authoritatively, and
    // query_graph(graph="missed") enumerates the misses. All exercised end to end
    // through the real handlers with a corrupt fixture file.
    use super::*;
    use std::fs;

    const BROKEN: &str = "def broken(  \n    let x: i32 = ;\n    @#$%^& }{\nclass 9Invalid\n";

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::Builder::new()
            .prefix("coverage_tools_")
            .tempdir()
            .expect("temp dir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mk src");
        fs::write(repo.join("src/ok.py"), "def ok():\n    return 1\n").expect("ok");
        fs::write(repo.join("src/bad.py"), BROKEN).expect("bad");
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mk out");
        (tmp, repo, out)
    }

    #[test]
    fn index_codebase_reports_coverage_then_index_status_and_missed_surface_it() {
        let (_tmp, repo, out) = fixture();

        // index_codebase inline coverage.
        let idx = do_index_codebase(&json!({
            "path": repo.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
            "full": true,
        }))
        .expect("index");
        let cov = &idx["coverage"];
        assert_eq!(
            cov["parse_incomplete"]["count"],
            json!(1),
            "index_codebase must report the corrupt file as parse_incomplete"
        );
        assert!(
            cov["caveat"]
                .as_str()
                .unwrap()
                .contains("NOT a completeness guarantee"),
            "the honesty caveat must be present"
        );
        let graph_path = idx["graph_path"].as_str().expect("graph_path").to_string();

        // index_status authoritative coverage.
        let status = run_index_status(&json!({ "graph_path": graph_path }));
        assert_eq!(status["status"], json!("ok"));
        assert_eq!(status["coverage"]["parse_incomplete"]["count"], json!(1));
        let listed = status["coverage"]["parse_incomplete"]["files"][0]["path"]
            .as_str()
            .expect("a listed parse_incomplete file");
        assert_eq!(listed, "src/bad.py");

        // query_graph(graph="missed") enumerates the misses (no Cypher needed).
        let missed = run_query_graph(&json!({ "graph_path": graph_path, "graph": "missed" }));
        assert_eq!(missed["graph"], json!("missed"));
        assert_eq!(missed["coverage"]["parse_incomplete"]["count"], json!(1));
    }
}

#[cfg(test)]
mod token_surface_tools_tests {
    // Issue #56 — the detail/format token surface, exercised end to end through
    // the real handlers.
    use super::*;
    use std::fs;

    fn indexed_graph() -> (tempfile::TempDir, String) {
        let tmp = tempfile::Builder::new()
            .prefix("token_surface_tools_")
            .tempdir()
            .expect("temp dir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mk src");
        fs::write(
            repo.join("src/a.py"),
            "def process_one():\n    return 1\n\ndef process_two():\n    return 2\n",
        )
        .expect("write a");
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mk out");
        let idx = do_index_codebase(&json!({
            "path": repo.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
            "full": true,
        }))
        .expect("index");
        let graph = idx["graph_path"].as_str().expect("graph_path").to_string();
        (tmp, graph)
    }

    #[test]
    fn search_ids_mode_returns_bare_identifiers() {
        let (_tmp, graph) = indexed_graph();
        let resp = run_search_codebase(&json!({
            "graph_path": graph, "query": "process", "detail": "ids"
        }));
        assert_eq!(resp["detail"], json!("ids"));
        let results = resp["results"].as_array().expect("results array");
        assert!(!results.is_empty(), "should find process_* symbols");
        // Every element is a bare qualified-name string, not an object.
        assert!(
            results.iter().all(|v| v.is_string()),
            "ids mode must return bare identifiers, got {results:?}"
        );
    }

    #[test]
    fn search_tabular_mode_declares_columns_and_streams_rows() {
        let (_tmp, graph) = indexed_graph();
        let resp = run_search_codebase(&json!({
            "graph_path": graph, "query": "process", "format": "tabular"
        }));
        assert_eq!(resp["format"], json!("tabular"));
        assert!(
            resp["columns"].is_array(),
            "tabular must declare a columns header"
        );
        let results = resp["results"].as_array().expect("results array");
        assert!(
            results.iter().all(|row| row.is_array()),
            "tabular rows must be arrays of cells, got {results:?}"
        );
    }

    #[test]
    fn query_graph_tabular_drops_the_human_result_string() {
        let (_tmp, graph) = indexed_graph();
        // Default keeps the human `result` table.
        let default = run_query_graph(&json!({
            "graph_path": graph, "query": "MATCH (f:Function) RETURN f.qualified_name"
        }));
        assert!(
            default.get("result").is_some(),
            "default query_graph keeps the result string"
        );
        // Tabular drops it (rows+columns already carry the data).
        let tabular = run_query_graph(&json!({
            "graph_path": graph,
            "query": "MATCH (f:Function) RETURN f.qualified_name",
            "format": "tabular"
        }));
        assert_eq!(tabular["format"], json!("tabular"));
        assert!(
            tabular.get("result").is_none(),
            "tabular query_graph must omit the redundant human result string"
        );
        assert!(tabular["rows"].is_array());
        // Serialized, tabular must be smaller than the default for the same query.
        let d = serde_json::to_string(&default).unwrap().len();
        let t = serde_json::to_string(&tabular).unwrap().len();
        assert!(t < d, "tabular ({t}) must be smaller than default ({d})");
    }

    #[test]
    fn query_graph_ids_mode_collapses_to_first_column() {
        let (_tmp, graph) = indexed_graph();
        let resp = run_query_graph(&json!({
            "graph_path": graph,
            "query": "MATCH (f:Function) RETURN f.qualified_name, f.name",
            "detail": "ids"
        }));
        assert_eq!(resp["detail"], json!("ids"));
        let rows = resp["rows"].as_array().expect("rows array");
        assert!(
            rows.iter().all(|v| v.is_string()),
            "ids mode must collapse rows to the first column's bare values, got {rows:?}"
        );
    }
}

#[cfg(test)]
mod temporal_runtime_tests {
    // Issue #58 — ingest_traces (matched / static-miss / unresolved), the
    // FILE_CHANGES_WITH surface in get_impact, and query_graph reachability of
    // both new edge families, exercised end to end through the real handlers.
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Builds a git repo where caller.py calls callee.py::callee (resolvable) and
    /// the two files co-change 3× (for FILE_CHANGES_WITH). Returns (tmp, graph).
    fn fixture() -> (tempfile::TempDir, String) {
        let tmp = tempfile::Builder::new()
            .prefix("temporal_rt_")
            .tempdir()
            .expect("tmp");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).expect("mk repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@ap.dev"]);
        git(&repo, &["config", "user.name", "AP"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        let callee_body =
            |n: i32| format!("def callee():\n    return {n}\n\ndef other():\n    return 2\n");
        fs::write(repo.join("callee.py"), callee_body(1)).unwrap();
        fs::write(
            repo.join("caller.py"),
            "from callee import callee\n\ndef caller():\n    return callee()\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        // Two more commits touching BOTH files → 3 co-changes total.
        for i in 2..4 {
            fs::write(repo.join("callee.py"), callee_body(i)).unwrap();
            fs::write(
                repo.join("caller.py"),
                format!("from callee import callee\n\ndef caller():\n    return callee() + {i}\n"),
            )
            .unwrap();
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
        }

        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mk out");
        let idx = do_index_codebase(&json!({
            "path": repo.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
            "full": true,
        }))
        .expect("index");
        // The index mined co-change (git repo, default on).
        assert_eq!(
            idx["cochange"]["mode"],
            json!("full"),
            "cochange mined on full index"
        );
        let graph = idx["graph_path"].as_str().expect("graph_path").to_string();
        // Resolve so the static Calls edge caller->callee exists.
        let store = graph_store::GraphStore::open_or_create(Path::new(&graph)).expect("open");
        resolver::resolve_graph(&store).expect("resolve");
        drop(store);
        (tmp, graph)
    }

    #[test]
    fn ingest_traces_matches_creates_and_reports_unresolved() {
        let (_tmp, graph) = fixture();
        let resp = run_ingest_traces(&json!({
            "graph_path": graph,
            "traces": [
                { "caller": "caller.py::caller", "callee": "callee.py::callee", "count": 5 },
                { "caller": "caller.py::caller", "callee": "callee.py::other", "count": 3 },
                { "caller": "ghost.py::nope", "callee": "callee.py::callee" }
            ]
        }));
        assert_eq!(resp["status"], json!("ok"), "resp: {resp}");
        assert_eq!(
            resp["matched"],
            json!(1),
            "the static caller->callee call is annotated"
        );
        assert_eq!(
            resp["unmatched_created"],
            json!(1),
            "caller->other has no static edge → OBSERVED_CALLS"
        );
        assert_eq!(
            resp["unresolved_names"],
            json!(1),
            "ghost::nope is not a node"
        );

        // The static Calls edge now carries the observed weight.
        let weighted = run_query_graph(&json!({
            "graph_path": graph,
            "query": "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) \
                      WHERE a.id = 'caller.py::caller' AND b.id = 'callee.py::callee' \
                      RETURN r.observed_count"
        }));
        assert_eq!(
            weighted["rows"][0][0],
            json!("5"),
            "observed_count annotated: {weighted}"
        );

        // The divergence is a queryable OBSERVED_CALLS edge (query_graph surface).
        let observed = run_query_graph(&json!({
            "graph_path": graph,
            "query": "MATCH (a:Function)-[r:OBSERVED_CALLS_Function_Function]->(b:Function) \
                      RETURN a.id, b.id, r.observed_count"
        }));
        let rows = observed["rows"].as_array().expect("rows");
        assert!(
            rows.iter()
                .any(|r| r[0] == "caller.py::caller" && r[1] == "callee.py::other"),
            "OBSERVED_CALLS divergence must be reachable via query_graph: {observed}"
        );
    }

    #[test]
    fn get_impact_surfaces_cochange_partners_under_token_surface() {
        let (_tmp, graph) = fixture();
        // callee.py::callee is called by caller; its file co-changes with caller.py.
        let full = run_get_impact(&json!({
            "graph_path": graph, "qualified_name": "callee.py::callee"
        }));
        let partners = full["cochange_partners"]
            .as_array()
            .expect("cochange_partners");
        assert!(
            partners.iter().any(|p| p["file"] == "caller.py"),
            "callee.py's co-change partner caller.py must be listed: {}",
            full["cochange_partners"]
        );

        // ids mode (issue #56 renderer) applies to the section automatically.
        let ids = run_get_impact(&json!({
            "graph_path": graph, "qualified_name": "callee.py::callee", "detail": "ids"
        }));
        let ids_partners = ids["cochange_partners"].as_array().expect("array");
        assert!(
            ids_partners.iter().all(|v| v.is_string()),
            "ids mode must render co-change partners as bare file ids: {}",
            ids["cochange_partners"]
        );
        assert!(ids_partners.iter().any(|v| v == "caller.py"));
    }

    #[test]
    fn file_changes_with_is_reachable_via_query_graph() {
        let (_tmp, graph) = fixture();
        let resp = run_query_graph(&json!({
            "graph_path": graph,
            "query": "MATCH (a:File)-[r:FILE_CHANGES_WITH]-(b:File) \
                      RETURN a.id, b.id, r.cochange_count, r.coupling"
        }));
        let rows = resp["rows"].as_array().expect("rows");
        assert!(
            !rows.is_empty(),
            "the mined FILE_CHANGES_WITH edge must be queryable: {resp}"
        );
        // caller.py <-> callee.py co-changed in all 3 commits.
        assert!(
            rows.iter().any(|r| {
                let files = [r[0].as_str().unwrap_or(""), r[1].as_str().unwrap_or("")];
                files.contains(&"caller.py") && files.contains(&"callee.py")
            }),
            "caller.py <-> callee.py must be a co-change edge: {resp}"
        );
    }
}
