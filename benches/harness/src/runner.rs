// runner.rs — black-box MCP invocation + per-query dispatch.
//
// Architecture: harness spawns one MCP process per corpus, drives it over
// stdio JSON-RPC, and keeps it alive across all queries for that corpus.
// Pipeline per corpus:
//   1. indexCodebase into a tempdir graph
//   2. resolveGraph
//   3. clusterGraph
//   4. for each label: call the tool, capture JSON, score
//
// We DO NOT link against the main crate.  The binary is a black-box
// consumer of its own published MCP surface.

use crate::corpora::{CorpusConfig, GroundTruthLabel};
use crate::queries;
use crate::scoring::{
    self, score_adjusted_rand, score_exact_match, score_f1, score_precision_recall_mean, ScoreType,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Outcome of running one corpus through the harness.
#[derive(Debug, Clone)]
pub struct CorpusRun {
    pub name: String,
    pub language: String,
    pub per_query_scores: HashMap<String, f64>,
    pub per_query_samples: HashMap<String, usize>,
    pub per_query_elapsed_ms: HashMap<String, u128>,
    pub end_result_score: f64,
    pub index_elapsed_ms: u128,
    pub labels_run: usize,
    pub labels_skipped: usize,
    pub setup_error: Option<String>,
    /// Ground-truth references whose source-file path no longer exists under
    /// the corpus tree (issue #132). Non-empty means the corpus is scoring
    /// against deleted symbols; the harness fails loudly instead of letting
    /// those silently score 0.
    pub stale_ground_truth: Vec<String>,
}

/// Run one full corpus.  Returns a CorpusRun even on failure — the
/// setup_error field explains what happened.
pub fn run_corpus(corpus: &CorpusConfig, binary: &Path) -> CorpusRun {
    let mut run = CorpusRun {
        name: corpus.name.clone(),
        language: corpus.language.clone(),
        per_query_scores: HashMap::new(),
        per_query_samples: HashMap::new(),
        per_query_elapsed_ms: HashMap::new(),
        end_result_score: 0.0,
        index_elapsed_ms: 0,
        labels_run: 0,
        labels_skipped: 0,
        setup_error: None,
        stale_ground_truth: Vec::new(),
    };

    // Ground-truth staleness guard (issue #132): detect labels that reference
    // deleted source files BEFORE scoring, so their zeros are never mistaken
    // for a retrieval regression. Loud + enumerated, per query.
    run.stale_ground_truth =
        crate::corpora::stale_ground_truth(&corpus.source_path, &corpus.labels);
    for rel in &run.stale_ground_truth {
        eprintln!(
            "[bench][STALE GROUND TRUTH] corpus={}: references deleted source path {:?} \
             (this expectation silently scores 0 — fix or remove the label)",
            corpus.name, rel
        );
    }

    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            run.setup_error = Some(format!("tempdir: {e}"));
            return run;
        }
    };
    let output_dir = tmp.path().join("out");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        run.setup_error = Some(format!("create output dir: {e}"));
        return run;
    }
    let graph_path = output_dir.join("graph");

    let mut client = match McpClient::spawn(binary) {
        Ok(c) => c,
        Err(e) => {
            run.setup_error = Some(format!("spawn mcp: {e}"));
            return run;
        }
    };
    if let Err(e) = client.initialize() {
        run.setup_error = Some(format!("initialize: {e}"));
        return run;
    }

    let started = Instant::now();
    if let Err(e) = index_corpus(&mut client, &corpus.source_path, &output_dir) {
        run.setup_error = Some(format!("index_codebase: {e}"));
        return run;
    }
    run.index_elapsed_ms = started.elapsed().as_millis();

    // Best-effort resolve + cluster; their absence shouldn't zero every query.
    let _ = client.call_tool(
        "resolve_graph",
        &json!({"graph_path": graph_path.to_string_lossy()}),
    );
    let _ = client.call_tool(
        "cluster_graph",
        &json!({"graph_path": graph_path.to_string_lossy()}),
    );

    // Per-query accumulator: sum + count so we can mean at the end.
    let mut sums: HashMap<String, f64> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut elapsed: HashMap<String, u128> = HashMap::new();

    for label in &corpus.labels {
        let spec = match queries::lookup(&label.query_id) {
            Some(s) => s,
            None => {
                run.labels_skipped += 1;
                continue;
            }
        };
        let start = Instant::now();
        let score = match dispatch_label(
            &mut client,
            &graph_path,
            &corpus.corpus_dir,
            spec.tool,
            spec.score_type,
            label,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[bench] {}/{}: dispatch error: {}",
                    corpus.name, label.query_id, e
                );
                0.0
            }
        };
        let dt = start.elapsed().as_millis();
        *sums.entry(label.query_id.clone()).or_insert(0.0) += score;
        *counts.entry(label.query_id.clone()).or_insert(0) += 1;
        *elapsed.entry(label.query_id.clone()).or_insert(0) += dt;
        run.labels_run += 1;
    }

    for (q, total) in &sums {
        let n = counts.get(q).copied().unwrap_or(1).max(1);
        run.per_query_scores.insert(q.clone(), total / n as f64);
        run.per_query_samples.insert(q.clone(), n);
        if let Some(e) = elapsed.get(q) {
            run.per_query_elapsed_ms.insert(q.clone(), *e);
        }
    }

    run.end_result_score = scoring::weighted_mean(&run.per_query_scores, &queries::weights());
    run
}

/// Index a source tree via MCP.  Returns Err if the tool's response
/// indicates failure.  The output_dir must contain `graph/` on success.
fn index_corpus(client: &mut McpClient, source: &Path, output_dir: &Path) -> Result<(), String> {
    let resp = client.call_tool(
        "index_codebase",
        &json!({
            "path": source.to_string_lossy(),
            "output_dir": output_dir.to_string_lossy(),
        }),
    )?;
    let text = extract_text(&resp)?;
    let payload: Value =
        serde_json::from_str(&text).map_err(|e| format!("index payload parse: {e}"))?;
    if payload.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return Err(format!("index status: {}", payload));
    }
    Ok(())
}

/// Dispatches one label: calls the right tool with the right args, passes
/// the response to the right scorer.
fn dispatch_label(
    client: &mut McpClient,
    graph_path: &Path,
    corpus_dir: &Path,
    tool: &str,
    score_type: ScoreType,
    label: &GroundTruthLabel,
) -> Result<f64, String> {
    let graph = graph_path.to_string_lossy().to_string();
    let args = build_tool_args(tool, &graph, corpus_dir, &label.input)?;
    let resp = client.call_tool(tool, &args)?;
    let payload = parse_tool_payload(&resp)?;
    score_response(tool, score_type, &payload, &label.expected)
}

/// Label `input` keys that name a fixture file on disk. A relative value is
/// anchored to the corpus directory before being forwarded to the tool, so
/// ground truth never has to embed an absolute, developer-machine-specific
/// path (issue #210) — the corpus is the only stable anchor across checkouts
/// and CI runners.
const FIXTURE_PATH_KEYS: &[&str] = &["prd_path", "affected_symbols_path"];

/// Assemble MCP tool args from the label's `input` plus graph_path.
fn build_tool_args(
    tool: &str,
    graph_path: &str,
    corpus_dir: &Path,
    input: &Value,
) -> Result<Value, String> {
    let mut obj = serde_json::Map::new();
    obj.insert("graph_path".to_string(), json!(graph_path));
    if let Some(input_obj) = input.as_object() {
        for (k, v) in input_obj {
            if FIXTURE_PATH_KEYS.contains(&k.as_str()) {
                if let Some(rel) = v.as_str() {
                    let resolved = resolve_fixture_path(corpus_dir, rel)?;
                    obj.insert(k.clone(), json!(resolved));
                    continue;
                }
            }
            obj.insert(k.clone(), v.clone());
        }
    }
    // Tool-specific: query_graph needs `query`, not `qualified_name`.
    // The label author is responsible for providing `query` when the tool
    // is query_graph; we just pass through.
    let _ = tool;
    Ok(Value::Object(obj))
}

/// Resolve a fixture-file reference against the corpus directory. Absolute
/// paths pass through unchanged (so a caller who genuinely needs one can
/// still supply it); relative paths are joined to `corpus_dir` and must
/// exist on disk — a dangling reference is a stale-label bug and must fail
/// loudly here, not as a downstream tool error with no context.
fn resolve_fixture_path(corpus_dir: &Path, rel: &str) -> Result<String, String> {
    let candidate = Path::new(rel);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        corpus_dir.join(candidate)
    };
    let canonical = resolved
        .canonicalize()
        .map_err(|e| format!("fixture path {:?} (resolved from {:?}): {e}", resolved, rel))?;
    Ok(canonical.to_string_lossy().to_string())
}

/// Parse the MCP envelope `{content:[{text: "..."}]}` and return the inner
/// JSON payload the tool produced.
fn parse_tool_payload(resp: &Value) -> Result<Value, String> {
    let text = extract_text(resp)?;
    serde_json::from_str(&text).map_err(|e| format!("payload parse: {e}"))
}

/// Pull `content[0].text` out of an MCP response.
fn extract_text(resp: &Value) -> Result<String, String> {
    resp.get("content")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("tool response missing content[0].text: {resp}"))
}

/// Route a payload+expected pair to the correct scorer.
fn score_response(
    tool: &str,
    score_type: ScoreType,
    payload: &Value,
    expected: &Value,
) -> Result<f64, String> {
    match score_type {
        ScoreType::ExactMatch => score_exact_from_payload(tool, payload, expected),
        ScoreType::F1Set => score_f1_from_payload(tool, payload, expected),
        ScoreType::AdjustedRand => score_ari_from_payload(payload, expected),
        ScoreType::PrecisionRecallMean => score_prmean_from_payload(payload, expected),
    }
}

/// Exact-match scoring: the tool is expected to produce a single qualified
/// name.
///   - search_codebase: results[0].qualified_name
///   - get_symbol: status=="ok" AND node is non-null — Q10 ("what module
///     is X in?") succeeds iff the symbol resolves at all; the spec doesn't
///     require us to extract a separate module concept the graph doesn't
///     carry yet.
fn score_exact_from_payload(tool: &str, payload: &Value, expected: &Value) -> Result<f64, String> {
    let expected_str =
        expected_field_str(expected, &["qualified_name", "value", "exists"]).unwrap_or_default();
    let actual_str = match tool {
        "search_codebase" => payload
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|r| r.get("qualified_name"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        "get_symbol" => get_symbol_resolved_qn(payload, &expected_str),
        _ => payload.to_string(),
    };
    Ok(score_exact_match(&expected_str, &actual_str))
}

/// get_symbol responds with `{status, node: {label, data: "<string>"}, ...}`.
/// `data` is a stringified representation of the row returned by the graph
/// store (lbug serialises rows as `Vec<String>`), so we cannot json-project
/// into it.  A resolved symbol is one where `status == "ok"` AND `node` is
/// non-null AND `data` contains the expected qualified name as a substring.
/// Returning the expected string on match (else empty) lets the exact-match
/// scorer work unchanged.
fn get_symbol_resolved_qn(payload: &Value, expected: &str) -> String {
    let status = payload.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "ok" {
        return String::new();
    }
    let node = match payload.get("node") {
        Some(n) if !n.is_null() => n,
        _ => return String::new(),
    };
    let data = node
        .get("data")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if expected.is_empty() || !data.contains(expected) {
        return String::new();
    }
    expected.to_string()
}

/// F1 scoring: extract a set of qualified names from the payload, compare to
/// the expected set.  The extraction key depends on the tool.
fn score_f1_from_payload(tool: &str, payload: &Value, expected: &Value) -> Result<f64, String> {
    let expected_set = expected_string_array(expected)?;
    let actual_set = extract_actual_set(tool, payload, expected);
    Ok(score_f1(&expected_set, &actual_set))
}

/// Maps an `expected` object's recognized key (see `expected_string_array`)
/// to the specific `get_context` relationship arrays that answer it. A label
/// asking "callers" must be scored against the relations that actually carry
/// call-like references — unioning every relationship kind (as a prior
/// version of this function did) counts unrelated `imports`/`implements`
/// edges as false positives and tanked precision on every q4/q5/q6 label
/// (root-cause verified live against `indexing_handlers.rs::do_index_codebase`:
/// 1 true caller vs a 21-item union — see issue #214 loss-ledger comment).
///
/// `called_by`/`calls` alone under-counts: a class instantiation
/// (`new ConsoleLogger(...)`) is graphed as a `Uses`/`UsedBy` edge, not
/// `Calls` (verified live on `logger.ts::ConsoleLogger`: `calls`/`called_by`
/// both empty, `used_by` holds the true caller). "Who calls/instantiates X"
/// is answered by the union of the call-edge and use-edge families.
fn get_context_relation_keys(expected: &Value) -> &'static [&'static str] {
    let obj = match expected.as_object() {
        Some(o) => o,
        None => return &[],
    };
    if obj.contains_key("callers") {
        &["called_by", "used_by"]
    } else if obj.contains_key("callees") {
        &["calls", "uses"]
    } else if obj.contains_key("implementors") {
        &["implemented_by"]
    } else if obj.contains_key("interfaces") {
        &["implements"]
    } else if obj.contains_key("imports") {
        &["imports"]
    } else {
        // Unrecognized expected shape: fall back to the full union so an
        // unanticipated label still gets *some* signal instead of an empty
        // actual set.
        &[
            "calls",
            "called_by",
            "implements",
            "implemented_by",
            "imports",
            "imported_by",
            "uses",
            "used_by",
        ]
    }
}

fn extract_actual_set(tool: &str, payload: &Value, expected: &Value) -> Vec<String> {
    match tool {
        "get_context" => {
            let mut out: Vec<String> = Vec::new();
            for rel in get_context_relation_keys(expected) {
                if let Some(arr) = payload
                    .get("relationships")
                    .and_then(|r| r.get(*rel))
                    .and_then(|v| v.as_array())
                {
                    for item in arr {
                        if let Some(qn) = item.get("qualified_name").and_then(|v| v.as_str()) {
                            out.push(qn.to_string());
                        }
                    }
                }
            }
            out
        }
        "get_impact" => {
            // The real get_impact response has no `affected` field (it
            // never did — verified against src/clustering/impact.rs and the
            // tool's own MCP schema); the previous version of this function
            // read `payload.get("affected")` and always got None, so q7
            // (weight 0.15, the single highest-weighted query) always scored
            // against an empty actual set. Every existing q7 label happens
            // to expect `[]` too, so the miss was silently vacuous instead
            // of loud — see issue #214. Blast radius is the union of the
            // four real reverse-dependency lists the tool documents:
            // callers, importers, users, implementors.
            let mut out = Vec::new();
            for key in ["callers", "importers", "users", "implementors"] {
                if let Some(arr) = payload.get(key).and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(qn) = item.get("qualified_name").and_then(|v| v.as_str()) {
                            out.push(qn.to_string());
                        }
                    }
                }
            }
            out
        }
        "query_graph" => {
            let mut out = Vec::new();
            if let Some(rows) = payload.get("rows").and_then(|v| v.as_array()) {
                for row in rows {
                    if let Some(arr) = row.as_array() {
                        if let Some(first) = arr.first().and_then(|v| v.as_str()) {
                            out.push(first.to_string());
                        }
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn score_ari_from_payload(payload: &Value, expected: &Value) -> Result<f64, String> {
    // expected.partition = [{qn:"...", cluster: 0}, ...]
    // payload.clusters = [{qn:"...", cluster_id: N}, ...] (if present)
    let part = expected
        .get("partition")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "q12 expected.partition missing".to_string())?;
    let mut expected_labels: Vec<i64> = Vec::with_capacity(part.len());
    let mut actual_labels: Vec<i64> = Vec::with_capacity(part.len());
    let actual_map = build_cluster_map(payload);
    for row in part {
        let qn = row
            .get("qn")
            .and_then(|v| v.as_str())
            .ok_or("partition row missing qn")?;
        let c = row
            .get("cluster")
            .and_then(|v| v.as_i64())
            .ok_or("partition row missing cluster")?;
        expected_labels.push(c);
        actual_labels.push(*actual_map.get(qn).unwrap_or(&-1));
    }
    Ok(score_adjusted_rand(&expected_labels, &actual_labels))
}

fn build_cluster_map(payload: &Value) -> HashMap<String, i64> {
    let mut m = HashMap::new();
    if let Some(arr) = payload.get("clusters").and_then(|v| v.as_array()) {
        for item in arr {
            if let (Some(qn), Some(cid)) = (
                item.get("qn").and_then(|v| v.as_str()),
                item.get("cluster_id").and_then(|v| v.as_i64()),
            ) {
                m.insert(qn.to_string(), cid);
            }
        }
    }
    m
}

fn score_prmean_from_payload(payload: &Value, expected: &Value) -> Result<f64, String> {
    // source: B3 fix — validate_prd_against_graph returns
    // `{report: {findings: [{axis, severity, ...}]}}`, not a flat
    // `flagged_present` array. `parse_tool_payload` already unwraps
    // `content[0].text`, so `payload.report.findings[].axis` is the
    // canonical path; we still fall back to the flat form for older tools.
    let flagged = extract_axis_set(payload);
    let truth = expected_string_array(expected)?;
    Ok(score_precision_recall_mean(&flagged, &truth))
}

/// Collect the set of `axis` strings from a validate_prd_against_graph
/// response. Tries `report.findings[].axis` first (the real shape), then
/// falls back to a flat `flagged_present` array for compatibility with
/// any future tool that might emit the simpler form.
fn extract_axis_set(payload: &Value) -> Vec<String> {
    if let Some(findings) = payload
        .get("report")
        .and_then(|r| r.get("findings"))
        .and_then(|v| v.as_array())
    {
        let mut out: Vec<String> = findings
            .iter()
            .filter_map(|f| f.get("axis").and_then(|v| v.as_str()).map(String::from))
            .collect();
        out.sort();
        out.dedup();
        return out;
    }
    extract_strs(payload, "flagged_present")
}

fn extract_strs(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Pull a field out of the `expected` object by any of the candidate keys.
/// Returns the stringified form (booleans become "true"/"false").
fn expected_field_str(expected: &Value, keys: &[&str]) -> Option<String> {
    let obj = expected.as_object()?;
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if let Some(b) = v.as_bool() {
                return Some(b.to_string());
            }
        }
    }
    None
}

/// Pull the canonical string-array out of an `expected` object — supports
/// the common keys used by q4-q14 labels.
fn expected_string_array(expected: &Value) -> Result<Vec<String>, String> {
    let obj = expected
        .as_object()
        .ok_or_else(|| format!("expected must be an object, got: {expected}"))?;
    for k in [
        "callers",
        "callees",
        "implementors",
        "interfaces",
        "symbols",
        "imports",
        "unresolved",
        "affected",
        "truly_present",
        "fields",
    ] {
        if let Some(arr) = obj.get(k).and_then(|v| v.as_array()) {
            return Ok(arr
                .iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect());
        }
    }
    Err(format!(
        "expected has no recognized array field: {expected}"
    ))
}

/// MCP client that owns a child process + its stdio pipes + a request id.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn the MCP binary with piped stdio.
    pub fn spawn(binary: &Path) -> Result<Self, String> {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn {:?}: {e}", binary))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(McpClient {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// Send the initialize handshake; required before any tool calls.
    pub fn initialize(&mut self) -> Result<(), String> {
        let _ = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "bench_end_result", "version": "0.0.1"},
            }),
        )?;
        self.notification("notifications/initialized", json!({}))?;
        Ok(())
    }

    /// Call one MCP tool.  Returns the raw `result` object (the
    /// `{content: [...]}` envelope).
    pub fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        self.request("tools/call", json!({"name": name, "arguments": args}))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&msg)?;
        self.read_response(id)
    }

    fn notification(&mut self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.send(&msg)
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let line = serde_json::to_string(msg).map_err(|e| format!("serialize: {e}"))?;
        writeln!(self.stdin, "{}", line).map_err(|e| format!("write: {e}"))?;
        self.stdin.flush().map_err(|e| format!("flush: {e}"))
    }

    fn read_response(&mut self, id: u64) -> Result<Value, String> {
        // Read lines until we get one whose id matches.  Ignore notifications.
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if Instant::now() > deadline {
                return Err(format!("timeout waiting for id={id}"));
            }
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("mcp server closed stdout".to_string());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(trimmed)
                .map_err(|e| format!("json parse {trimmed:?}: {e}"))?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(format!("mcp error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
