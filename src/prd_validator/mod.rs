// prd_validator — Stage 6: validate a PRD's claimed changes against the
// resolved + clustered code graph.
//
// Three validation axes, all expressible as Cypher queries over G:
//   1. Symbol hallucination          — claimed symbols that don't exist.
//   2. Community-consistency         — scope claim vs communities touched.
//   3. Process-impact contradiction  — "does not affect <process>" claim.
//
// Read-only w.r.t. the graph. LLM-free. Deterministic given the same PRD +
// same graph.
//
// source: stages/stage-6.md §4 (extraction contract + regex fallback),
//         §5 (validation axes), §6 (output schema).

mod axis_community;
mod axis_process;
mod axis_symbol;
mod verdict;

use crate::graph_store::GraphStore;
use crate::search;
use axis_community::{communities_for_resolved, distinct_count, emit_community_consistency};
use axis_process::{emit_process_impact, processes_for_resolved};
use axis_symbol::{emit_symbol_hallucination, emit_unresolved_info};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use verdict::{classify_unresolved, ClaimVerdict};
// Shared with `doc_claims`: a claim about an absent symbol is unverifiable, not
// absent, when its file is outside the indexed graph (issue #13).
pub(crate) use verdict::unverifiable_reason;

// source: stages/stage-6.md §4.2 — structured affected-symbols contract filename.
#[allow(dead_code)] // referenced by docs and tests; exported for downstream consumers.
pub const AFFECTED_SYMBOLS_FILE: &str = "stage-5.affected_symbols.json";

// source: stages/stage-6.md §6.2 — validation artifact filename.
pub const VALIDATION_FILE: &str = "stage-6.validation.json";

// source: stages/stage-6.md §4.3 — regex fallback token minimum length.
// Matches prd_input.rs MIN_TOKEN_LEN rationale (Lucene StandardAnalyzer).
const FALLBACK_MIN_TOKEN_LEN: usize = 3;

// source: stages/stage-6.md §4.3 — cap extracted tokens so a pathological PRD
// can't explode the graph lookup budget. Mirrors prd_input.rs MAX_TOKENS.
const FALLBACK_MAX_TOKENS: usize = 64;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct ValidationReport {
    pub validation_status: String,
    pub findings: Vec<ValidationFinding>,
    pub summary: ValidationSummary,
    pub extraction_mode: String,
    pub contract_missing: bool,
    pub affected_symbol_count: usize,
    pub scope_claim_count: usize,
}

pub struct ValidationFinding {
    pub axis: String,
    pub severity: String,
    pub message: String,
    pub symbol: Option<String>,
    pub details: Value,
}

pub struct ValidationSummary {
    pub claimed_symbols: u64,
    pub resolved_symbols: u64,
    pub hallucinated_symbols: u64,
    // source: issue #13 — symbols whose containing file is outside the
    // indexer's coverage (unsupported language, or not present in the
    // graph at all) can neither be confirmed nor refuted; they are kept
    // out of `hallucinated_symbols` so validation_status never fails on
    // a coverage gap alone. Mirrors epistemic.rs's exact/lower-bound
    // philosophy: say what the graph cannot verify, don't call it wrong.
    pub unverifiable_symbols: u64,
    pub communities_spanned: u64,
    pub processes_impacted: u64,
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

pub fn validate_prd(
    store: &GraphStore,
    prd_path: &Path,
    affected_symbols_path: Option<&Path>,
) -> Result<ValidationReport, String> {
    let prd_text = read_prd_text(prd_path)?;
    let (claims, scope_claims, mode, contract_missing) =
        load_claims(&prd_text, affected_symbols_path);
    let resolved = resolve_claims(store, &claims);
    let mut findings: Vec<ValidationFinding> = Vec::new();
    let verdicts = classify_unresolved(store, &resolved);
    emit_symbol_hallucination(&resolved, &verdicts, &mut findings);
    let communities = communities_for_resolved(store, &resolved);
    emit_community_consistency(&scope_claims, &communities, &mut findings);
    let processes = processes_for_resolved(store, &resolved);
    emit_process_impact(&scope_claims, &processes, &mut findings);
    emit_unresolved_info(&resolved, mode == "regex_fallback", &mut findings);
    let status = compute_status(&findings);
    let hallucinated_symbols = verdicts
        .iter()
        .filter(|v| matches!(v, ClaimVerdict::Hallucinated))
        .count() as u64;
    let unverifiable_symbols = verdicts
        .iter()
        .filter(|v| matches!(v, ClaimVerdict::Unverifiable(_)))
        .count() as u64;
    Ok(ValidationReport {
        validation_status: status,
        summary: ValidationSummary {
            claimed_symbols: claims.len() as u64,
            resolved_symbols: resolved.iter().filter(|r| r.resolved_qn.is_some()).count() as u64,
            hallucinated_symbols,
            unverifiable_symbols,
            communities_spanned: distinct_count(&communities) as u64,
            processes_impacted: processes.len() as u64,
        },
        findings,
        extraction_mode: mode,
        contract_missing,
        affected_symbol_count: claims.len(),
        scope_claim_count: scope_claims.len(),
    })
}

fn read_prd_text(prd_path: &Path) -> Result<String, String> {
    if !prd_path.exists() {
        // PRD path is optional for the symbol-hallucination axis when the
        // structured contract is present. Return empty-string so the regex
        // fallback path yields zero tokens rather than panicking on missing.
        return Ok(String::new());
    }
    fs::read_to_string(prd_path)
        .map_err(|e| format!("prd_read_failed: {}: {}", prd_path.display(), e))
}

// ---------------------------------------------------------------------------
// Claim extraction — contract-first with regex fallback
// ---------------------------------------------------------------------------

struct SymbolClaim {
    token: String,       // raw text as it appeared (qualified_name or identifier)
    change_kind: String, // add | modify | remove | rename | unknown
    #[allow(dead_code)]
    // retained from structured contract; future axes will surface it in findings.
    rationale: String,
}

#[derive(Clone, Debug)]
enum ScopeClaim {
    CommunityScope {
        #[allow(dead_code)] // surfaced in future "expected community name" axis (stage-6 §5 V2).
        assertion: String,
    },
    ProcessExclusion {
        processes: Vec<String>,
    },
}

fn load_claims(
    prd_text: &str,
    affected_symbols_path: Option<&Path>,
) -> (Vec<SymbolClaim>, Vec<ScopeClaim>, String, bool) {
    if let Some(path) = affected_symbols_path {
        if path.exists() {
            if let Ok(raw) = fs::read_to_string(path) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    let (claims, scopes) = parse_structured_claims(&v);
                    return (claims, scopes, "structured".into(), false);
                }
            }
        }
    }
    // Fallback — regex-only extraction, high recall, low precision.
    let claims = regex_extract_symbols(prd_text);
    (claims, Vec::new(), "regex_fallback".into(), true)
}

fn parse_structured_claims(v: &Value) -> (Vec<SymbolClaim>, Vec<ScopeClaim>) {
    let mut claims = Vec::new();
    if let Some(arr) = v.get("affected_symbols").and_then(|x| x.as_array()) {
        for item in arr {
            let qn = str_field(item, "qualified_name");
            if qn.is_empty() {
                continue;
            }
            claims.push(SymbolClaim {
                token: qn,
                change_kind: str_field_default(item, "change_kind", "unknown"),
                rationale: str_field(item, "rationale"),
            });
        }
    }
    let mut scopes = Vec::new();
    if let Some(arr) = v.get("scope_claims").and_then(|x| x.as_array()) {
        for item in arr {
            match str_field(item, "kind").as_str() {
                "community_scope" => scopes.push(ScopeClaim::CommunityScope {
                    assertion: str_field(item, "assertion"),
                }),
                "process_exclusion" => {
                    let procs: Vec<String> = item
                        .get("processes")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    scopes.push(ScopeClaim::ProcessExclusion { processes: procs });
                }
                _ => {}
            }
        }
    }
    (claims, scopes)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn str_field_default(v: &Value, key: &str, default: &str) -> String {
    let s = str_field(v, key);
    if s.is_empty() {
        default.to_string()
    } else {
        s
    }
}

// Regex fallback (implemented without an external regex crate — the "rules"
// state no new crates). We do a hand-written scan that matches the three
// patterns from stages/stage-6.md §4.3:
//   1. Backticked qualified name `A::B::C`
//   2. Backticked identifier `word_like`  (len >= 3)
//   3. File path with extension `src/main.rs`
fn regex_extract_symbols(text: &str) -> Vec<SymbolClaim> {
    let mut out: Vec<SymbolClaim> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for raw in extract_backticked(text) {
        push_token(&mut out, &mut seen, raw);
        if out.len() >= FALLBACK_MAX_TOKENS {
            return out;
        }
    }
    for raw in extract_file_paths(text) {
        push_token(&mut out, &mut seen, raw);
        if out.len() >= FALLBACK_MAX_TOKENS {
            return out;
        }
    }
    out
}

fn push_token(out: &mut Vec<SymbolClaim>, seen: &mut BTreeSet<String>, token: String) {
    if token.len() < FALLBACK_MIN_TOKEN_LEN {
        return;
    }
    if !seen.insert(token.clone()) {
        return;
    }
    out.push(SymbolClaim {
        token,
        change_kind: "unknown".into(),
        rationale: "regex_fallback".into(),
    });
}

fn extract_backticked(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'`' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'`' {
                let slice = &text[start..j];
                if is_identifier_or_qn(slice) {
                    out.push(slice.to_string());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_identifier_or_qn(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        && s.chars().any(|c| c.is_ascii_alphabetic() || c == '_')
}

fn extract_file_paths(text: &str) -> Vec<String> {
    // Match `word(/word)+\.(rs|ts|tsx|py|go|js)` — file-path-looking tokens.
    let mut out = Vec::new();
    for raw in text
        .split(|c: char| c.is_ascii_whitespace() || c == '`' || c == '(' || c == ')' || c == ',')
    {
        if looks_like_file_path(raw) {
            out.push(raw.trim_end_matches(&['.', ',', ';', ':'][..]).to_string());
        }
    }
    out
}

fn looks_like_file_path(s: &str) -> bool {
    let exts = [".rs", ".ts", ".tsx", ".py", ".go", ".js"];
    s.contains('/')
        && exts.iter().any(|e| s.ends_with(e))
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-' | '.'))
}

// ---------------------------------------------------------------------------
// Claim resolution — use search::resolve_qualified_name for strip-prefix + fuzz
// ---------------------------------------------------------------------------

struct ResolvedClaim<'a> {
    claim: &'a SymbolClaim,
    resolved_qn: Option<String>,
    did_you_mean: Vec<String>,
}

fn resolve_claims<'a>(store: &GraphStore, claims: &'a [SymbolClaim]) -> Vec<ResolvedClaim<'a>> {
    claims
        .iter()
        .map(|claim| resolve_one(store, claim))
        .collect()
}

fn resolve_one<'a>(store: &GraphStore, claim: &'a SymbolClaim) -> ResolvedClaim<'a> {
    match search::resolve_qualified_name(store, &claim.token) {
        Ok(qn) => ResolvedClaim {
            claim,
            resolved_qn: Some(qn),
            did_you_mean: Vec::new(),
        },
        Err(nf) => ResolvedClaim {
            claim,
            resolved_qn: None,
            did_you_mean: nf.did_you_mean,
        },
    }
}

// ---------------------------------------------------------------------------
// Status + artifact
// ---------------------------------------------------------------------------

fn compute_status(findings: &[ValidationFinding]) -> String {
    let mut has_critical = false;
    let mut has_warning = false;
    for f in findings {
        match f.severity.as_str() {
            "critical" => has_critical = true,
            "warning" => has_warning = true,
            _ => {}
        }
    }
    if has_critical {
        "fail".into()
    } else if has_warning {
        "warning".into()
    } else {
        "ok".into()
    }
}

pub fn report_to_json(
    report: &ValidationReport,
    run_id: &str,
    finding_id: &str,
    prd_path: &Path,
    graph_path: &Path,
    validated_at: &str,
) -> Value {
    let findings: Vec<Value> = report
        .findings
        .iter()
        .map(|f| {
            json!({
                "axis": f.axis, "severity": f.severity, "message": f.message,
                "symbol": f.symbol, "details": f.details,
            })
        })
        .collect();
    json!({
        "run_id": run_id,
        "finding_id": finding_id,
        "prd_path": prd_path.to_string_lossy(),
        "graph_path": graph_path.to_string_lossy(),
        "validated_at": validated_at,
        "contract_missing": report.contract_missing,
        "extraction_mode": report.extraction_mode,
        "affected_symbol_count": report.affected_symbol_count,
        "scope_claim_count": report.scope_claim_count,
        "findings": findings,
        "validation_status": report.validation_status,
        "summary": {
            "claimed_symbols": report.summary.claimed_symbols,
            "resolved_symbols": report.summary.resolved_symbols,
            "hallucinated_symbols": report.summary.hallucinated_symbols,
            "unverifiable_symbols": report.summary.unverifiable_symbols,
            "communities_spanned": report.summary.communities_spanned,
            "processes_impacted": report.summary.processes_impacted,
        },
    })
}

pub fn write_validation(path: &Path, value: &Value) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {}", e))?;
    fs::write(path, bytes).map_err(|e| format!("write {:?}: {}", path, e))?;
    Ok(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Pure-helper tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
