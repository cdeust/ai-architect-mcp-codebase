// resolver — Stage 3b resolution pass.
//
// Reads existing nodes from the graph and adds cross-file semantic edges:
// Imports, Calls, Implements, Extends, Uses.
// Runs AFTER index_codebase (3a). Modifies the graph in-place.
//
// source: stages/stage-3b.md §4, §5

use crate::ambiguity_policy::{self, Context as PolicyContext, Resolution as PolicyResolution};
use crate::graph_store::{is_known_rel_table, GraphStore, PropEdgeList};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Counter of edges dropped because their dynamically-formatted
/// table name doesn't appear in REL_TABLES. Producers that build
/// table names from runtime symbol labels (e.g., resolve_single_import
/// → ``Imports_File_<label>``) MUST validate against the schema or
/// risk a hard failure deep in graph_store. We surface drops via
/// eprintln! so missing labels (e.g., Method, Field, Variant) are
/// visible to the operator instead of silently degrading or aborting.
static UNKNOWN_REL_DROPS: AtomicU64 = AtomicU64::new(0);

/// Stage-3b helper: validate the dynamically-formed rel table against
/// the schema before staging. Returns true when it's safe to insert.
/// Logs the first few unknown labels at warn-equivalent level so the
/// operator can surface them as a missing-schema-entry without each
/// occurrence spamming the log.
fn check_known_rel_table(table: &str, from_id: &str, to_id: &str) -> bool {
    if is_known_rel_table(table) {
        return true;
    }
    let n = UNKNOWN_REL_DROPS.fetch_add(1, Ordering::Relaxed);
    if n < 8 {
        eprintln!(
            "resolver: dropped edge with unknown rel table '{table}' \
             ({from_id} -> {to_id}); add it to REL_TABLES in graph_store.rs"
        );
    }
    false
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Reason string recorded on an `UnresolvedRef` whose target matched a
/// language provider's `external_prefixes` (a known stdlib/framework/
/// third-party root, not a corpus symbol). Public so callers that must
/// distinguish genuine externals from other unresolved reasons (e.g. the
/// cross-repo bridge candidate filter in main.rs) key off this constant
/// instead of duplicating the literal.
pub const EXTERNAL_UNRESOLVED_REASON: &str = "external crate";

pub struct ResolutionResult {
    pub imports_resolved: u64,
    pub calls_resolved: u64,
    pub impls_resolved: u64,
    pub extends_resolved: u64,
    pub uses_resolved: u64,
    pub total_edges: u64,
    pub total_refs: u64,
    pub unresolved: Vec<UnresolvedRef>,
    pub elapsed_ms: u64,
}

/// Tracks a reference that could not be resolved.
/// Fields are read by downstream stages (3c/3d) and integration tests.
#[allow(dead_code)]
pub struct UnresolvedRef {
    pub kind: String,
    pub from_id: String,
    pub target_text: String,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Symbol index — in-memory map from name -> (id, label, qualified_name)
// source: stages/stage-3b.md §9 Q5 — HashMap index for O(1) lookups
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SymbolEntry {
    id: String,
    label: String,
    qualified_name: String,
}

impl ambiguity_policy::Candidate for SymbolEntry {
    fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
}

struct SymbolIndex {
    by_name: HashMap<String, Vec<SymbolEntry>>,
    by_qn: HashMap<String, SymbolEntry>,
    // Symbols grouped by their immediate parent module path (qualified_name
    // with the last "::segment" stripped). Populated once, O(total_symbols).
    // Lets glob-import resolution (`from x import *`) look up only the
    // symbols that live directly inside `x` instead of scanning every
    // symbol in the graph per glob import — see resolve_glob_import.
    by_parent_module: HashMap<String, Vec<SymbolEntry>>,
}

fn build_symbol_index(store: &GraphStore) -> Result<SymbolIndex, String> {
    let labels = &[
        "Function",
        "Method",
        "Struct",
        "Enum",
        "Trait",
        "Constant",
        "TypeAlias",
        "Module",
        "File",
    ];
    let mut by_name: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
    let mut by_qn: HashMap<String, SymbolEntry> = HashMap::new();
    let mut by_parent_module: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
    for label in labels {
        let qn_col = if *label == "File" {
            "path"
        } else {
            "qualified_name"
        };
        let cypher = format!("MATCH (n:{label}) RETURN n.id, n.name, n.{qn_col}");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let entry = SymbolEntry {
                id: row[0].clone(),
                label: label.to_string(),
                qualified_name: row[2].clone(),
            };
            by_name
                .entry(row[1].clone())
                .or_default()
                .push(entry.clone());
            if let Some((parent, _)) = row[2].rsplit_once("::") {
                by_parent_module
                    .entry(parent.to_string())
                    .or_default()
                    .push(entry.clone());
            }
            by_qn.insert(row[2].clone(), entry);
        }
    }
    Ok(SymbolIndex {
        by_name,
        by_qn,
        by_parent_module,
    })
}

// ---------------------------------------------------------------------------
// Entry point — runs all resolution phases in order
// source: stages/stage-3b.md §4.3
// ---------------------------------------------------------------------------

pub fn resolve_graph(store: &GraphStore) -> Result<ResolutionResult, String> {
    let start = Instant::now();
    let idx = build_symbol_index(store)?;
    let file_imports = build_file_import_map(store)?;
    let existing = load_existing_edges(store)?;
    let mut buf = EdgeBuffer::new(existing);

    let (imp_resolved, imp_total, imp_unresolved) = resolve_imports(store, &idx, &mut buf)?;
    let (call_resolved, call_total, call_unresolved) =
        resolve_calls(store, &idx, &file_imports, &mut buf)?;
    let (impl_resolved, impl_total, impl_unresolved) = resolve_implements(store, &idx, &mut buf)?;
    let (ext_resolved, ext_total, ext_unresolved) = resolve_extends(store, &idx, &mut buf)?;
    let (uses_resolved, uses_total, uses_unresolved) =
        resolve_uses(store, &idx, &file_imports, &mut buf)?;

    // 3b-v2 Layer 4/5 — macro + stdlib expansion. Lives in resolver_layers
    // so resolver.rs's function surface stays stable for Q8 ground truth.
    // source: stages/stage-3b-v2.md §5.
    //
    // source: issue #28 — macro refs used to contribute only to the
    // numerator (`macro_resolved` folded into `calls_resolved` with no
    // matching denominator), which let `resolution_rate` exceed 1.0.
    // `run_macro_expansion` now returns the same (resolved, total,
    // unresolved) shape as every other phase; its total is folded into
    // `total_refs` below.
    let idx_ref = &idx;
    let (macro_resolved, macro_total, macro_unresolved) =
        crate::resolver_layers::run_macro_expansion(store, &mut buf, &|qn: &str| {
            determine_caller_label(idx_ref, qn)
        })?;

    buf.flush(store)?;
    let call_resolved = call_resolved + macro_resolved;
    let call_total = call_total + macro_total;

    let total_edges = imp_resolved + call_resolved + impl_resolved + ext_resolved + uses_resolved;
    let total_refs = imp_total + call_total + impl_total + ext_total + uses_total;

    let mut unresolved = Vec::new();
    unresolved.extend(imp_unresolved);
    unresolved.extend(call_unresolved);
    unresolved.extend(macro_unresolved);
    unresolved.extend(impl_unresolved);
    unresolved.extend(ext_unresolved);
    unresolved.extend(uses_unresolved);

    // invariant: every reference enters `total_refs` exactly once and
    // produces exactly one resolved-or-unresolved outcome. Each phase
    // upholds this locally (see the per-phase postconditions above); this
    // asserts it holds in aggregate. source: issue #28 §"resolved +
    // unresolved == total_refs must hold exactly".
    debug_assert_eq!(
        total_edges + unresolved.len() as u64,
        total_refs,
        "resolution accounting invariant violated: total_edges ({total_edges}) + \
         unresolved ({}) != total_refs ({total_refs})",
        unresolved.len()
    );

    Ok(ResolutionResult {
        imports_resolved: imp_resolved,
        calls_resolved: call_resolved,
        impls_resolved: impl_resolved,
        extends_resolved: ext_resolved,
        uses_resolved,
        total_edges,
        total_refs,
        unresolved,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// EdgeBuffer — in-memory staging area for resolution edges.
//
// Collects all resolved edges across phases, deduplicates via a HashSet of
// (rel_table, from, to) triples, and flushes grouped by rel_table through
// GraphStore::bulk_insert_edges at the end. Eliminates per-edge MATCH+CREATE
// round-trips and the idempotency sub-query that used to run before every
// insert_resolution_edge call.
// source: Fermi audit April 2026 — resolver was bottlenecked by this loop.
// ---------------------------------------------------------------------------

/// Outcome of staging one edge. All three variants mean the *reference*
/// resolved to a real target — they differ only in whether a DB write is
/// needed. Callers that count `resolved` refs must treat all three as
/// resolved; only `Inserted` causes a new row to be written on flush.
///
/// source: issue #28 — before this type existed, `add` returned a plain
/// `bool` (true only for `Inserted`), so a ref whose edge was already
/// persisted from a prior `resolve_graph` run (`AlreadyPersisted`) or whose
/// edge collapsed with another ref resolving to the same target within this
/// run (`DuplicateInRun`, e.g. two call sites in one function calling the
/// same callee) was silently dropped from the `resolved` counter, even
/// though the reference legitimately resolved. That is what collapsed
/// `resolution_rate` toward 0 on re-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// Newly staged in this run; will be written to the store on flush.
    Inserted,
    /// This exact (rel_table, from, to) edge already exists in the store
    /// from a previous `resolve_graph` run.
    AlreadyPersisted,
    /// This exact edge was already staged earlier in this same run (a
    /// different reference resolved to the identical target).
    DuplicateInRun,
}

pub struct EdgeBuffer {
    by_table: HashMap<String, PropEdgeList>,
    /// Edges that existed in the store before this run started. Read-only
    /// after construction — never mutated by `add`.
    persisted: HashSet<(String, String, String)>,
    /// Edges staged during this run (superset check happens against
    /// `persisted` first, so this set never overlaps it).
    staged: HashSet<(String, String, String)>,
}

impl EdgeBuffer {
    fn new(existing: HashSet<(String, String, String)>) -> Self {
        Self {
            by_table: HashMap::new(),
            persisted: existing,
            staged: HashSet::new(),
        }
    }

    /// Stages an edge for the given (rel_table, from, to) key.
    ///
    /// precondition: `rel_table` is a schema-known relationship table (the
    /// caller has already checked `check_known_rel_table`).
    /// postcondition: returns the outcome (see `AddOutcome`); on
    /// `Inserted`, the edge is queued in `by_table` for `flush`. On
    /// `AlreadyPersisted` / `DuplicateInRun`, no new row is queued — the
    /// edge is either already in the store or already staged this run.
    pub fn add(
        &mut self,
        rel_table: &str,
        from_id: &str,
        to_id: &str,
        confidence: f64,
        method: &str,
    ) -> AddOutcome {
        let key = (
            rel_table.to_string(),
            from_id.to_string(),
            to_id.to_string(),
        );
        if self.persisted.contains(&key) {
            return AddOutcome::AlreadyPersisted;
        }
        if self.staged.contains(&key) {
            return AddOutcome::DuplicateInRun;
        }
        self.staged.insert(key);
        let props = vec![
            ("confidence".to_string(), confidence.to_string()),
            ("resolution_method".to_string(), format!("'{method}'")),
        ];
        self.by_table
            .entry(rel_table.to_string())
            .or_default()
            .push((from_id.to_string(), to_id.to_string(), props));
        AddOutcome::Inserted
    }

    fn flush(self, store: &GraphStore) -> Result<(), String> {
        // Tolerate unknown rel tables: the resolver builds rel-table
        // names dynamically from caller+target labels (e.g.
        // Calls_Method_Struct when a method calls a struct constructor),
        // but REL_TABLES only declares a subset of label pairs. Skipping
        // the unknown ones with a warning lets every valid table still
        // flush. Previously, one unknown table aborted the whole flush
        // via ``?`` and dropped every resolved edge — which is why
        // downstream consumers (Cortex viz) saw zero Calls/Imports rows.
        for (table, edges) in &self.by_table {
            match store.bulk_insert_edges(table, edges) {
                Ok(_) => {}
                Err(e) if e.contains("unknown relationship type") => {
                    eprintln!(
                        "resolver: skipping {} edges on unknown rel table {} ({})",
                        edges.len(),
                        table,
                        e,
                    );
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Reads all resolution edges currently in the graph so the EdgeBuffer's
/// idempotency check works on a re-run without per-edge count(r) queries.
fn load_existing_edges(store: &GraphStore) -> Result<HashSet<(String, String, String)>, String> {
    let mut seen = HashSet::new();
    use crate::graph_store::REL_TABLES;
    for &(rel, from_label, to_label) in REL_TABLES {
        if !is_resolution_edge(rel) {
            continue;
        }
        let cypher = format!("MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) RETURN a.id, b.id");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() >= 2 {
                seen.insert((rel.to_string(), row[0].clone(), row[1].clone()));
            }
        }
    }
    Ok(seen)
}

fn is_resolution_edge(rel: &str) -> bool {
    rel.starts_with("Imports_")
        || rel.starts_with("Calls_")
        || rel.starts_with("Implements_")
        || rel.starts_with("Extends_")
        || rel.starts_with("Uses_")
}

// ---------------------------------------------------------------------------
// Phase 1: Import resolution
// source: stages/stage-3b.md §5.1
// ---------------------------------------------------------------------------

pub(crate) type PhaseResult = Result<(u64, u64, Vec<UnresolvedRef>), String>;

fn resolve_imports(store: &GraphStore, idx: &SymbolIndex, buf: &mut EdgeBuffer) -> PhaseResult {
    let qr = store.execute_query("MATCH (i:Import) RETURN i.id, i.path, i.is_glob, i.language")?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    // §10.4 — Import nodes whose target was found; is_resolved is flipped to
    // true for these (all others keep the false written at index time).
    let mut resolved_ids: Vec<&str> = Vec::new();

    for row in &qr.rows {
        if row.len() < 4 {
            continue;
        }
        let provider = crate::language_provider::provider_for(&row[3]);
        let (r, t, u) = resolve_one_import(idx, buf, provider, &row[0], &row[1], &row[2]);
        resolved += r;
        total += t;
        if r > 0 {
            resolved_ids.push(&row[0]);
        }
        unresolved.extend(u);
    }
    store.mark_nodes_resolved("Import", &resolved_ids)?;
    Ok((resolved, total, unresolved))
}

/// Resolves a single Import row. Returns (resolved, total, unresolved) with
/// the invariant `resolved + unresolved.len() as u64 == total` holding for
/// every call — a non-glob import always contributes exactly 1 to `total`
/// (it is one reference); a glob import contributes one unit of `total` per
/// matched symbol (each matched symbol is itself the reference a glob
/// import introduces — there is no "unresolved member" of a glob).
fn resolve_one_import(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    provider: &dyn crate::language_provider::LanguageProvider,
    import_id: &str,
    path: &str,
    is_glob_str: &str,
) -> (u64, u64, Vec<UnresolvedRef>) {
    if provider.is_external_import(path) {
        return (
            0,
            1,
            vec![UnresolvedRef {
                kind: "Imports".to_string(),
                from_id: import_id.to_string(),
                target_text: path.to_string(),
                reason: EXTERNAL_UNRESOLVED_REASON.to_string(),
            }],
        );
    }
    let file_id = extract_file_from_import_id(import_id);
    let normalized = provider.normalize_import_path(path).to_string();
    let is_glob = is_glob_str == "true" || is_glob_str == "True";

    if is_glob {
        let count = resolve_glob_import(idx, buf, &file_id, &normalized);
        return (count, count, vec![]);
    }
    match resolve_single_import(idx, buf, provider, &file_id, &normalized) {
        Ok(count) => (count, 1, vec![]),
        Err(reason) => (
            0,
            1,
            vec![UnresolvedRef {
                kind: "Imports".to_string(),
                from_id: import_id.to_string(),
                target_text: path.to_string(),
                reason: reason.to_string(),
            }],
        ),
    }
}

/// Resolves one non-glob import to its target symbol.
/// postcondition: `Ok(1)` iff a target was found (regardless of whether the
/// edge is new, already persisted, or a within-run duplicate — see
/// `AddOutcome`); `Err(reason)` iff no edge could be produced for this ref.
fn resolve_single_import(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    provider: &dyn crate::language_provider::LanguageProvider,
    file_id: &str,
    normalized_path: &str,
) -> Result<u64, &'static str> {
    let last_segment = provider.import_last_segment(normalized_path);
    let candidates = idx
        .by_name
        .get(last_segment)
        .ok_or("no target found in graph")?;
    // The import path itself is the disambiguating evidence: it plays the
    // role of an "import in scope" whose suffix should match exactly one
    // candidate's qualified name. source: issue #30 — single ambiguity
    // policy shared with resolve_single_call's qualified path.
    let import_hint = [normalized_path.to_string()];
    let ctx = PolicyContext {
        imports_in_scope: &import_hint,
        caller_file: file_id,
        caller_package: None,
    };
    // resolve() leaves a genuinely ambiguous import (no evidence
    // distinguishes the candidates) unresolved rather than guessed — see
    // resolve_single_call's doc comment for why.
    let (entry, evidence, conf) = match ambiguity_policy::resolve(candidates, &ctx) {
        PolicyResolution::Resolved {
            target,
            evidence,
            confidence,
        } => (target, evidence, confidence),
        PolicyResolution::NotFound | PolicyResolution::Ambiguous { .. } => {
            return Err("no target found in graph")
        }
    };
    let table = format!("Imports_File_{}", entry.label);
    if !check_known_rel_table(&table, file_id, &entry.id) {
        return Err("unknown rel table for import target");
    }
    buf.add(
        &table,
        file_id,
        &entry.id,
        conf,
        ambiguity_policy::resolution_label(evidence),
    );
    Ok(1)
}

/// Resolves a glob import (`use module::*`) to every symbol the index
/// knows is a direct child of `module_path`. Each matched symbol is one
/// resolved reference; unlike a single import there is no "not found"
/// case per symbol — the module's members are enumerated from the graph
/// itself, so every candidate considered here is, by construction, real.
fn resolve_glob_import(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    file_id: &str,
    module_path: &str,
) -> u64 {
    // Non-termination fix (2026-07-04): this used to scan idx.by_qn (every
    // symbol in the whole graph) for EVERY glob import, i.e.
    // O(glob_imports * total_symbols). On a repo with a vendored dependency
    // tree (e.g. cortex-viz's 503MB .venv indexed via include_dependencies),
    // total_symbols is huge AND Python packages commonly re-export via
    // `from .submodule import *` inside __init__.py, so glob_imports also
    // scales with corpus size — the product is quadratic in corpus size.
    // Measured: 2_000 glob imports x 100_000 symbols took ~1.3s with the
    // linear-scan version (see bench_glob_import_scaling, "before" run,
    // commit message for the exact numbers). by_parent_module groups
    // symbols by parent qualified-name once (O(total_symbols) at index
    // build time), turning each glob import's cost into O(matches)
    // instead of O(total_symbols).
    let Some(candidates) = idx.by_parent_module.get(module_path) else {
        return 0;
    };
    let mut count = 0u64;
    for entry in candidates {
        let table = format!("Imports_File_{}", entry.label);
        if !check_known_rel_table(&table, file_id, &entry.id) {
            continue;
        }
        buf.add(&table, file_id, &entry.id, 0.9, "import-scope-lookup");
        count += 1;
    }
    count
}

// ---------------------------------------------------------------------------
// Phase 2: Call resolution
// source: stages/stage-3b.md §5.2
// ---------------------------------------------------------------------------

fn resolve_calls(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let qr =
        store.execute_query("MATCH (cs:CallSite) RETURN cs.id, cs.callee_name, cs.language")?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    // §10.4 — CallSite nodes whose callee was resolved to a graph target.
    let mut resolved_ids: Vec<String> = Vec::new();

    for row in &qr.rows {
        if row.len() < 3 {
            continue;
        }
        let callee = &row[1];
        // Macro invocations (`name!(...)`) are a distinct reference kind,
        // resolved exclusively by resolver_layers::run_macro_expansion.
        // Counting them here too would attempt (and fail) a plain-function
        // lookup for every macro call, double-counting the same physical
        // CallSite into both phases' total_refs — the root cause of the
        // >1.0 / undercounted-denominator half of issue #28.
        if callee.ends_with('!') {
            continue;
        }
        total += 1;
        let mut tally = CallTally {
            resolved: &mut resolved,
            unresolved: &mut unresolved,
        };
        if resolve_one_call_site(idx, file_imports, buf, &row[0], callee, &row[2], &mut tally) {
            resolved_ids.push(row[0].clone());
        }
    }
    let id_refs: Vec<&str> = resolved_ids.iter().map(|s| s.as_str()).collect();
    store.mark_nodes_resolved("CallSite", &id_refs)?;
    Ok((resolved, total, unresolved))
}

/// Resolves one CallSite row: gathers the caller-side evidence, delegates
/// ambiguity/confidence entirely to `resolve_single_call` (which in turn
/// delegates to the single `ambiguity_policy` module — issue #30), then
/// stages or records the outcome. Extracted from `resolve_calls` to keep
/// that function an orchestration loop (scan + accumulate) — resolution,
/// edge-kind reclassification, schema validation, and metric counting are
/// each owned by a single downstream helper (`resolve_single_call`,
/// `stage_call_edge`, `check_known_rel_table` inside it, and `CallTally`
/// respectively) instead of being interleaved inline. source: issue #32.
///
/// postcondition: returns `true` iff the callee resolved to a real graph
/// target (mirrors the former inline `resolved > resolved_before` check) —
/// the caller uses this to flip `CallSite.is_resolved` (§10.4). Behavior is
/// identical to the pre-extraction inline loop body.
fn resolve_one_call_site(
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
    cs_id: &str,
    callee: &str,
    language: &str,
    tally: &mut CallTally,
) -> bool {
    let provider = crate::language_provider::provider_for(language);
    let caller_qn = extract_caller_from_callsite_id(cs_id);
    let file_id = extract_file_from_qn(&caller_qn);
    let caller_label = determine_caller_label(idx, &caller_qn);

    let site = CallSite {
        cs_id,
        callee,
        caller_qn: &caller_qn,
        caller_label: &caller_label,
    };
    let resolved_before = *tally.resolved;
    match resolve_single_call(idx, provider, file_imports, callee, &file_id) {
        PolicyResolution::Resolved {
            target,
            evidence,
            confidence,
        } => {
            let matched = MatchedCall {
                target: &target,
                evidence,
                confidence,
            };
            stage_call_edge(buf, &site, &matched, tally);
        }
        // Genuinely ambiguous (no evidence tier discriminates the
        // candidates): labeled and dropped rather than guessed — see
        // resolve_single_call's doc comment for why this beats a
        // deterministic tiebreak here (issue #30).
        PolicyResolution::Ambiguous { candidates } => record_call_unresolved(
            &site,
            tally,
            format!("ambiguous ({} candidates)", candidates.len()),
        ),
        PolicyResolution::NotFound => {
            record_call_unresolved(&site, tally, "no target found".to_string())
        }
    }
    // The callee resolved to a graph target — flip the CallSite's
    // is_resolved (§10.4). Applies to both Calls and Uses edges (both mean
    // "target found").
    *tally.resolved > resolved_before
}

/// Records one unresolved `Calls` reference with the given reason.
fn record_call_unresolved(site: &CallSite, tally: &mut CallTally, reason: String) {
    tally.unresolved.push(UnresolvedRef {
        kind: "Calls".to_string(),
        from_id: site.cs_id.to_string(),
        target_text: site.callee.to_string(),
        reason,
    });
}

/// One row from the `CallSite` scan, grouped so downstream helpers take a
/// single reference instead of four loose string parameters.
struct CallSite<'a> {
    cs_id: &'a str,
    callee: &'a str,
    caller_qn: &'a str,
    caller_label: &'a str,
}

/// A resolved callee plus the evidence/confidence the policy attached to it.
struct MatchedCall<'a> {
    target: &'a SymbolEntry,
    evidence: ambiguity_policy::Evidence,
    confidence: f64,
}

/// Running counters for `resolve_calls`, grouped so helpers take one
/// reference instead of two separate mutable accumulator parameters.
struct CallTally<'a> {
    resolved: &'a mut u64,
    unresolved: &'a mut Vec<UnresolvedRef>,
}

/// Stages the Calls/Uses edge for one resolved callee, or records why it
/// couldn't be staged (unknown rel table / wrong label combination).
/// source: stages/stage-3b.md §2 — Calls is Function|Method ->
/// Function|Method only. Names resolving to Struct/Enum/Trait/TypeAlias are
/// tuple-struct ctors / enum-variant calls / type uses — preserved as
/// Uses_* edges so the full dependency chain remains queryable; dropping
/// them would hide real dependencies from change_impact/navigate.
fn stage_call_edge(
    buf: &mut EdgeBuffer,
    site: &CallSite,
    matched: &MatchedCall,
    tally: &mut CallTally,
) {
    let target = matched.target;
    let (rel_opt, kind) = match target.label.as_str() {
        "Function" | "Method" => (
            Some(format!("Calls_{}_{}", site.caller_label, target.label)),
            "Calls",
        ),
        "Struct" | "Enum" | "Trait" | "TypeAlias"
            if site.caller_label == "Function" || site.caller_label == "Method" =>
        {
            (
                Some(format!("Uses_{}_{}", site.caller_label, target.label)),
                "Uses",
            )
        }
        _ => (None, "Calls"),
    };
    match rel_opt {
        Some(rel) => {
            if !check_known_rel_table(&rel, site.caller_qn, &target.id) {
                // Schema doesn't declare this label combination — already
                // logged by check_known_rel_table. Nothing more to do.
            } else {
                // All three `AddOutcome` variants mean the reference
                // resolved to a real target (see `AddOutcome` doc comment);
                // they differ only in whether a DB write is queued.
                buf.add(
                    &rel,
                    site.caller_qn,
                    &target.id,
                    matched.confidence,
                    ambiguity_policy::resolution_label(matched.evidence),
                );
                *tally.resolved += 1;
            }
        }
        None => tally.unresolved.push(UnresolvedRef {
            kind: kind.to_string(),
            from_id: site.cs_id.to_string(),
            target_text: site.callee.to_string(),
            reason: format!(
                "no rel table for {} -> {} (callsite-as-call)",
                site.caller_label, target.label
            ),
        }),
    }
}

/// Resolves one callee reference via the shared ambiguity policy (issue
/// #30), through `call_evidence::resolve_two_pass` (issue #29). Both the
/// qualified/import-matched path and the unqualified path build the
/// evidence available at the call site and delegate — `ambiguity_policy`
/// remains the ONLY place that decides ambiguity/confidence; this function
/// (and call_evidence.rs) only gather and represent evidence for it.
///
/// Deliberately never applies a deterministic tiebreak: a genuinely
/// ambiguous reference (no evidence tier discriminates the candidates) is
/// left unresolved (`Ambiguous`) rather than guessed. This is stricter than
/// the issue's suggested "deterministic tiebreak to preserve recall" —
/// tested against `tests/graph_accuracy.rs` (the repo's ground-truth
/// gate), the deterministic tiebreak measurably regressed precision: e.g.
/// `infrastructure/pg_store.py` has a bare `_now_iso()` call inside a
/// method, name-ambiguous between the module-level function and an
/// unrelated same-named method; Python's own scoping resolves it to the
/// function, but neither candidate carries evidence our tiers model, so
/// tiebreaking picked the wrong (method) target and introduced 2 false
/// Calls edges (Calls F1 dropped 1.0 -> 0.5). Dropping the edge (labeled
/// `ambiguous (N candidates)`) matches the pre-issue-#30 unqualified
/// behavior exactly, so no real edges are lost — only the mislabeling and
/// the qualified-path's arbitrary-`candidates[0]` guess are fixed.
/// The tiebreaking variant (`resolve_deterministic`) was removed from
/// ambiguity_policy as dead code (PR #38); recover it from git history if
/// a future caller prefers recall over precision for its own ambiguity
/// class.
///
/// precondition: `callee` is the raw callee spelling as parsed (for Kotlin,
/// per issue #29, this preserves a package/object qualifier — see
/// parser/kotlin/extract/g2.rs::qualifier_or_tail — but never a value-receiver, which the parser strips
/// back to a bare name before it reaches here); `file_id` is the caller's
/// file path.
/// postcondition: the returned `Resolution` depends only on the candidate
/// set and the evidence context — never directly on whether `callee` was
/// spelled qualified or unqualified.
fn resolve_single_call(
    idx: &SymbolIndex,
    provider: &dyn crate::language_provider::LanguageProvider,
    file_imports: &HashMap<String, Vec<String>>,
    callee: &str,
    file_id: &str,
) -> PolicyResolution<SymbolEntry> {
    // Fully qualified: the callee's own spelling is the import-match
    // evidence. Unqualified: evidence is the file's own import list.
    let (last, imports_hint): (&str, Vec<String>) =
        if callee.contains("::") || callee.contains(provider.import_separator()) {
            (
                provider.import_last_segment(callee),
                vec![callee.to_string()],
            )
        } else {
            (
                callee,
                file_imports.get(file_id).cloned().unwrap_or_default(),
            )
        };
    let candidates = match idx.by_name.get(last) {
        Some(c) => c,
        None => return PolicyResolution::NotFound,
    };
    let ev = crate::call_evidence::CallEvidence {
        imports_hint: &imports_hint,
        caller_file: file_id,
    };
    crate::call_evidence::resolve_two_pass(
        candidates,
        |e: &SymbolEntry| e.qualified_name.clone(),
        |e: &SymbolEntry| extract_file_from_qn(&e.qualified_name),
        provider,
        &ev,
    )
}

// ---------------------------------------------------------------------------
// Phase 3: Implements resolution
// source: stages/stage-3b.md §5.3
// ---------------------------------------------------------------------------

/// Resolves Implements edges from DECLARED facts, not method-name guesses.
/// Two sources:
///   (A) the `implements` CSV column on Struct/Enum (`#[derive(...)]` names,
///       and, for other languages, `implements`/interface clauses), and
///   (B) `impl Trait for Type` blocks, which the parser stamps onto each
///       method as `trait_name` + the receiver's QN.
///
/// source: implements fix — replaces the prior fuzzy `trait-name-match`
/// heuristic (which guessed Struct→Trait whenever a method name coincided
/// with a trait method, producing false edges and missing every declared
/// impl). Mirrors resolve_extends and finally wires the
/// macro_expansion.emit_implements table (Debug → std::fmt::Debug, …).
fn resolve_implements(store: &GraphStore, idx: &SymbolIndex, buf: &mut EdgeBuffer) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    let mut created_stdlib: HashSet<String> = HashSet::new();

    // (A) Declared/derived trait names on Struct/Enum.
    for label in &["Struct", "Enum"] {
        let q = format!("MATCH (s:{label}) RETURN s.id, s.implements, s.language");
        let qr = match store.execute_query(&q) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let from_id = &row[0];
            let csv = &row[1];
            let provider = crate::language_provider::provider_for(&row[2]);
            if csv.is_empty() || csv == "Null(String)" {
                continue;
            }
            for raw in csv.split(',') {
                let name = raw.trim();
                if name.is_empty() {
                    continue;
                }
                total += 1;
                let ctx = ResolveContext { store, idx };
                let candidate = ImplementsCandidate {
                    provider,
                    label,
                    from_id,
                    name,
                };
                if resolve_one_implements(&ctx, buf, &candidate, &mut created_stdlib)? {
                    resolved += 1;
                } else {
                    unresolved.push(UnresolvedRef {
                        kind: "Implements".to_string(),
                        from_id: from_id.clone(),
                        target_text: name.to_string(),
                        reason: "no_target_in_corpus".to_string(),
                    });
                }
            }
        }
    }

    // (B) `impl Trait for Type` blocks.
    let (b_res, b_total, b_unresolved) = resolve_impl_trait_blocks(store, idx, buf)?;
    resolved += b_res;
    total += b_total;
    unresolved.extend(b_unresolved);

    Ok((resolved, total, unresolved))
}

/// Resolve one implemented-trait NAME for a Struct/Enum. Prefers a local Trait
/// in the corpus (`Implements_<Label>_Trait`, confidence 0.95); otherwise a
/// stdlib trait via the derive macro table (`Implements_<Label>_StdlibSymbol`,
/// 0.9).
/// postcondition: returns `Ok(true)` iff a target trait was found — staging
/// outcome (`AddOutcome`) does not gate this; a ref whose edge is already
/// persisted or duplicated within this run still resolved to a real trait.
///
/// Read-only lookup context shared by resolve_one_implements — bundles the
/// two params it only ever reads together, per coding-standards.md §4.4
/// (>4 params is a missing data type).
struct ResolveContext<'a> {
    store: &'a GraphStore,
    idx: &'a SymbolIndex,
}

/// One implemented-trait-name candidate being resolved: which Struct/Enum
/// declared it, in what language, and the raw trait name text.
struct ImplementsCandidate<'a> {
    provider: &'a dyn crate::language_provider::LanguageProvider,
    label: &'a str,
    from_id: &'a str,
    name: &'a str,
}

fn resolve_one_implements(
    ctx: &ResolveContext,
    buf: &mut EdgeBuffer,
    candidate: &ImplementsCandidate,
    created_stdlib: &mut HashSet<String>,
) -> Result<bool, String> {
    let ImplementsCandidate {
        provider,
        label,
        from_id,
        name,
    } = *candidate;
    let lookup = provider.import_last_segment(name);
    if let Some(t) = ctx
        .idx
        .by_name
        .get(lookup)
        .and_then(|c| c.iter().find(|e| e.label == "Trait"))
    {
        let table = format!("Implements_{label}_Trait");
        buf.add(&table, from_id, &t.id, 0.95, "declared-implements");
        return Ok(true);
    }
    // Derive/decorator → stdlib-trait expansion, only for languages that
    // declare such a table (Rust derives). `None` → no fabricated edges.
    let macro_key = match provider.derive_macro_key() {
        Some(k) => k,
        None => return Ok(false),
    };
    if let Some(exp) = crate::macro_expansion::lookup(macro_key, &format!("derive_{name}")) {
        let mut any = false;
        let table = format!("Implements_{label}_StdlibSymbol");
        for canonical in exp.emit_implements {
            crate::resolver_layers::ensure_stdlib_symbol(
                ctx.store,
                created_stdlib,
                canonical,
                "rust",
            )?;
            buf.add(&table, from_id, canonical, 0.9, "derive-macro");
            any = true;
        }
        return Ok(any);
    }
    Ok(false)
}

/// Resolve `impl Trait for Type` blocks. The parser stamps each method in such
/// a block with `trait_name` and the receiver's QN; we emit one
/// `Implements_<Label>_Trait` edge per block. buf.add collapses the repeated
/// methods of a block to a single edge.
fn resolve_impl_trait_blocks(
    store: &GraphStore,
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    let qr = store.execute_query(
        "MATCH (m:Method) WHERE m.trait_name <> '' AND m.receiver_type <> '' \
         RETURN m.receiver_type, m.trait_name",
    )?;
    for row in &qr.rows {
        if row.len() < 2 {
            continue;
        }
        let receiver_qn = row[0].trim();
        let trait_name = row[1].trim();
        if receiver_qn.is_empty() || trait_name.is_empty() {
            continue;
        }
        let recv = match idx.by_qn.get(receiver_qn) {
            Some(e) if e.label == "Struct" || e.label == "Enum" => e,
            _ => continue,
        };
        total += 1;
        let lookup = trait_name.rsplit("::").next().unwrap_or(trait_name);
        match idx
            .by_name
            .get(lookup)
            .and_then(|c| c.iter().find(|e| e.label == "Trait"))
        {
            Some(t) => {
                let table = format!("Implements_{}_Trait", recv.label);
                buf.add(&table, &recv.id, &t.id, 0.95, "impl-block");
                resolved += 1;
            }
            None => unresolved.push(UnresolvedRef {
                kind: "Implements".to_string(),
                from_id: recv.id.clone(),
                target_text: trait_name.to_string(),
                reason: "no_target_in_corpus".to_string(),
            }),
        }
    }
    Ok((resolved, total, unresolved))
}

// ---------------------------------------------------------------------------
// Phase 4: Extends resolution (supertrait)
// source: stages/stage-3b.md §5.4
// ---------------------------------------------------------------------------

/// Resolves the `bases` CSV property on every Struct/Enum/Trait node, looks
/// each base name up in the symbol index, and emits the matching
/// Extends_X_Y edge. Names that resolve to an Import node (cross-file base)
/// are left unresolved — multi-file indexing surfaces those naturally.
///
/// source: Spike B' BUG #9 fix — previously a no-op stub that just counted
/// pre-existing edges. The parser writes bases to the node property; this
/// function does the deferred name→QN resolution that the indexer can't
/// perform (the indexer routes by labels via label_by_qn, but base names
/// aren't QNs yet at insert time).
///
/// source: issue #28 fix — previously inserted each edge directly via
/// `store.insert_edge`, discarding the `Result` (`let _ = ...`) so a failed
/// insert still counted as resolved, and bypassing the `EdgeBuffer` dedup
/// every other edge kind goes through (so a re-run, or two identical base
/// names in one CSV, wrote duplicate physical edges). Now routes through
/// `buf`/`flush` like every other phase: the flush error propagates via
/// `?` in `resolve_graph`, and duplicates collapse the same way Imports/
/// Calls/Uses do.
fn resolve_extends(store: &GraphStore, idx: &SymbolIndex, buf: &mut EdgeBuffer) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    for &(label, table_self) in &[
        ("Struct", "Extends_Struct_Struct"),
        ("Enum", "Extends_Enum_Enum"),
        ("Trait", "Extends_Trait_Trait"),
    ] {
        let q = format!("MATCH (s:{label}) RETURN s.id, s.bases");
        let qr = match store.execute_query(&q) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 2 {
                continue;
            }
            let child_qn = &row[0];
            let bases_csv = &row[1];
            if bases_csv.is_empty() || bases_csv == "Null(String)" {
                continue;
            }
            for raw_base in bases_csv.split(',') {
                let raw_base = raw_base.trim();
                if raw_base.is_empty() {
                    continue;
                }
                total += 1;
                let (r, u) =
                    resolve_one_extends_base(idx, buf, label, table_self, child_qn, raw_base);
                resolved += r;
                unresolved.extend(u);
            }
        }
    }

    Ok((resolved, total, unresolved))
}

/// Resolves one base-class/interface name for one Struct/Enum/Trait node.
/// postcondition: `(1, vec![])` iff the base name matched a corpus symbol
/// and a schema-known rel table exists for the (label, target.label) pair
/// (regardless of `AddOutcome`); `(0, vec![one entry])` otherwise.
/// Extracted from `resolve_extends` to keep that function under the §4.2
/// size limit; behavior is identical to the pre-extraction inline loop body.
fn resolve_one_extends_base(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    label: &str,
    table_self: &str,
    child_qn: &str,
    raw_base: &str,
) -> (u64, Vec<UnresolvedRef>) {
    // Look up by last `.`-separated segment so `typing.NamedTuple` resolves
    // on `NamedTuple` if present. Cortex uses `::` in QNs but base names
    // came from source so they may carry `.`.
    let lookup = raw_base.rsplit('.').next().unwrap_or(raw_base);
    let candidates = match idx.by_name.get(lookup) {
        Some(v) => v,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Extends".to_string(),
                    from_id: child_qn.to_string(),
                    target_text: raw_base.to_string(),
                    reason: "no_target_in_corpus".to_string(),
                }],
            )
        }
    };
    // Prefer same-label matches (Struct→Struct, etc.) over cross-label
    // (Struct→Trait) for symmetry with `table_self`.
    let target = match candidates
        .iter()
        .find(|c| c.label == label)
        .or_else(|| candidates.first())
    {
        Some(t) => t,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Extends".to_string(),
                    from_id: child_qn.to_string(),
                    target_text: raw_base.to_string(),
                    reason: "no_target_in_corpus".to_string(),
                }],
            )
        }
    };
    let rel_table = if target.label == label {
        table_self.to_string()
    } else {
        format!("Extends_{label}_{}", target.label)
    };
    if !crate::graph_store::is_known_rel_table(&rel_table) {
        return (
            0,
            vec![UnresolvedRef {
                kind: "Extends".to_string(),
                from_id: child_qn.to_string(),
                target_text: raw_base.to_string(),
                reason: format!("no_rel_table_for_{label}_to_{}", target.label),
            }],
        );
    }
    // Staged through EdgeBuffer like every other resolution phase
    // (bulk_insert_edges at flush) instead of one insert_edge per base.
    // 0.95/"declared-extends" mirrors resolve_one_implements's 0.95/
    // "declared-implements" for the parallel `implements` CSV.
    // source: ADR-4253701 §Decision 2 (levier 2, resolver.rs resolve_extends).
    buf.add(&rel_table, child_qn, &target.id, 0.95, "declared-extends");
    (1, vec![])
}

// ---------------------------------------------------------------------------
// Phase 5: Uses (type-usage) resolution
// source: stages/stage-3b.md §5.5
// ---------------------------------------------------------------------------

fn resolve_uses(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    // Resolve field type annotations -> Struct/Enum/Trait
    let field_result = resolve_field_type_uses(store, idx, file_imports, buf)?;
    resolved += field_result.0;
    total += field_result.1;
    unresolved.extend(field_result.2);

    // Resolve return-type + type-construction references on Function/Method
    // (issue #92) -> Uses_<caller>_<Type>.
    let callable_result = resolve_callable_type_uses(store, idx, buf)?;
    resolved += callable_result.0;
    total += callable_result.1;
    unresolved.extend(callable_result.2);

    Ok((resolved, total, unresolved))
}

/// Resolves the `return_type` and `constructed_types` references recorded on
/// each Function/Method (issue #92) into `Uses_<caller_label>_<TargetLabel>`
/// edges. The two properties are the parser's record of the types a callable
/// names in its return-type annotation and constructs in its body; neither is a
/// call the call walker captures for Go composite literals / Rust struct
/// literals / TS `new` (Python constructs via a plain call, already covered by
/// `stage_call_edge`), so this phase is what surfaces `core.go`/`core.rs`/
/// `core.ts` as users of `OrderConfig` in the #64 eval's D4 rows.
///
/// postcondition: `resolved + unresolved.len() as u64 == total`; `total` counts
/// one unit per extracted type identifier across BOTH properties (matching
/// `resolve_field_type_uses`' per-identifier accounting, so a callable that both
/// returns and constructs the same type contributes 2 — the second `buf.add` is
/// a `DuplicateInRun` no-op edge-wise but still a resolved reference).
fn resolve_callable_type_uses(
    store: &GraphStore,
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    // Function and Method carry the same two columns (graph_store COLS_*); each
    // is its own caller label in the emitted `Uses_<label>_<Type>` table.
    for caller_label in ["Function", "Method"] {
        let query = format!(
            "MATCH (f:{caller_label}) RETURN f.id, f.return_type, f.constructed_types, f.language"
        );
        let qr = store.execute_query(&query)?;
        for row in &qr.rows {
            if row.len() < 4 {
                continue;
            }
            let callable_id = &row[0];
            let provider = crate::language_provider::provider_for(&row[3]);
            let prims = provider.primitives();
            // Return-type identifiers, then construction identifiers — both fed
            // through the same nominal-type extractor the field path uses.
            let names: Vec<String> = extract_type_identifiers(&row[1], prims)
                .into_iter()
                .chain(extract_type_identifiers(&row[2], prims))
                .collect();
            for type_name in &names {
                total += 1;
                let (r, u) =
                    resolve_one_callable_type_use(idx, buf, callable_id, caller_label, type_name);
                resolved += r;
                unresolved.extend(u);
            }
        }
    }
    Ok((resolved, total, unresolved))
}

/// Resolves one extracted type identifier for one Function/Method into a
/// `Uses_<caller_label>_<TargetLabel>` edge.
/// postcondition: `(1, vec![])` iff the identifier matched a known type-like
/// node AND the schema declares the resulting rel table; `(0, vec![one entry])`
/// otherwise (type not found, or no rel table for the label combination).
fn resolve_one_callable_type_use(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    callable_id: &str,
    caller_label: &str,
    type_name: &str,
) -> (u64, Vec<UnresolvedRef>) {
    let target = match find_type_target(idx, type_name) {
        Some(t) => t,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Uses".to_string(),
                    from_id: callable_id.to_string(),
                    target_text: type_name.to_string(),
                    reason: "type not found in graph".to_string(),
                }],
            )
        }
    };
    let table = format!("Uses_{caller_label}_{}", target.label);
    if !check_known_rel_table(&table, callable_id, &target.id) {
        return (
            0,
            vec![UnresolvedRef {
                kind: "Uses".to_string(),
                from_id: callable_id.to_string(),
                target_text: type_name.to_string(),
                reason: format!("unknown rel table {table}"),
            }],
        );
    }
    buf.add(&table, callable_id, &target.id, 0.9, "type-reference-parse");
    (1, vec![])
}

/// Resolves every nominal type identifier extracted from each Field's type
/// annotation.
/// postcondition: `resolved + unresolved.len() as u64 == total` — `total`
/// counts per extracted type identifier, not per Field row. A field typed
/// `HashMap<Foo, Bar>` extracts two identifiers (`Foo`, `Bar`; `HashMap`
/// itself is a provider-declared primitive/container and is skipped), so it
/// contributes 2 to `total`, matching the up-to-2 edges it can produce.
/// source: issue #28 — previously `total = qr.rows.len()` (1 per Field),
/// while `resolved`/`unresolved` counted per type identifier, so a
/// multi-identifier field inflated the numerator past the denominator.
fn resolve_field_type_uses(
    store: &GraphStore,
    idx: &SymbolIndex,
    _file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let qr = store.execute_query("MATCH (f:Field) RETURN f.id, f.type_annotation, f.language")?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    for row in &qr.rows {
        if row.len() < 3 {
            continue;
        }
        let field_id = &row[0];
        let type_ann = &row[1];
        let provider = crate::language_provider::provider_for(&row[2]);
        let type_names = extract_type_identifiers(type_ann, provider.primitives());

        for type_name in &type_names {
            total += 1;
            let (r, u) = resolve_one_field_type_use(idx, buf, field_id, type_name);
            resolved += r;
            unresolved.extend(u);
        }
    }
    Ok((resolved, total, unresolved))
}

/// Resolves one extracted type identifier for one Field.
/// postcondition: `(1, vec![])` iff the identifier matched a known type and
/// a schema-known rel table exists for it; `(0, vec![one entry])`
/// otherwise.
fn resolve_one_field_type_use(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    field_id: &str,
    type_name: &str,
) -> (u64, Vec<UnresolvedRef>) {
    let target = match find_type_target(idx, type_name) {
        Some(t) => t,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Uses".to_string(),
                    from_id: field_id.to_string(),
                    target_text: type_name.to_string(),
                    reason: "type not found in graph".to_string(),
                }],
            )
        }
    };
    let table = format!("Uses_Field_{}", target.label);
    if !check_known_rel_table(&table, field_id, &target.id) {
        return (
            0,
            vec![UnresolvedRef {
                kind: "Uses".to_string(),
                from_id: field_id.to_string(),
                target_text: type_name.to_string(),
                reason: format!("unknown rel table {table}"),
            }],
        );
    }
    buf.add(&table, field_id, &target.id, 0.9, "type-annotation-parse");
    (1, vec![])
}

/// Extract type identifiers from a type annotation string.
/// Strips generics, references, lifetimes, and finds nominal types. The
/// `primitives` skip-set is language-specific (provided by LanguageProvider);
/// lowercase builtins are additionally skipped by the uppercase-identifier
/// convention below.
fn extract_type_identifiers(type_ann: &str, primitives: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    let cleaned = type_ann.replace(['&', '*', '<', '>', ',', '(', ')', '[', ']'], " ");
    for token in cleaned.split_whitespace() {
        // Skip lifetimes, keywords, primitives
        if token.starts_with('\'') || token == "mut" || token == "dyn" || token == "impl" {
            continue;
        }
        if primitives.contains(&token) {
            continue;
        }
        // Must start with uppercase to be a type name (convention)
        if token.chars().next().is_some_and(|c| c.is_uppercase()) {
            result.push(token.to_string());
        }
    }
    result
}

fn find_type_target<'a>(idx: &'a SymbolIndex, type_name: &str) -> Option<&'a SymbolEntry> {
    let candidates = idx.by_name.get(type_name)?;
    // Filter to type-like labels only
    let types: Vec<_> = candidates
        .iter()
        .filter(|e| matches!(e.label.as_str(), "Struct" | "Enum" | "Trait" | "TypeAlias"))
        .collect();
    if types.len() == 1 {
        return Some(types[0]);
    }
    if types.is_empty() {
        return None;
    }
    // Ambiguous: return first match with lower confidence (handled by caller)
    Some(types[0])
}

// ---------------------------------------------------------------------------
// File-import map: file_id -> [import paths]
// ---------------------------------------------------------------------------

fn build_file_import_map(store: &GraphStore) -> Result<HashMap<String, Vec<String>>, String> {
    let qr = store.execute_query("MATCH (i:Import) RETURN i.id, i.path")?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for row in &qr.rows {
        if row.len() < 2 {
            continue;
        }
        let file_id = extract_file_from_import_id(&row[0]);
        map.entry(file_id).or_default().push(row[1].clone());
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Path normalization helpers
// ---------------------------------------------------------------------------

fn extract_file_from_import_id(import_id: &str) -> String {
    // Import IDs have format: "file_path::import_display_name". The file path
    // is recognized by its extension across ALL supported languages (the
    // extension set is disjoint enough that no per-node language is needed).
    // source: language_provider::ALL_EXTENSIONS (= parser::Language::from_extension).
    crate::language_provider::extract_file_prefix(import_id)
        .unwrap_or_else(|| import_id.to_string())
}

fn extract_file_from_qn(qn: &str) -> String {
    crate::language_provider::extract_file_prefix(qn).unwrap_or_else(|| qn.to_string())
}

pub(crate) fn extract_caller_from_callsite_id(cs_id: &str) -> String {
    // CallSite IDs: "caller_qn::call@line:col"
    if let Some(idx) = cs_id.rfind("::call@") {
        cs_id[..idx].to_string()
    } else {
        cs_id.to_string()
    }
}

fn determine_caller_label(idx: &SymbolIndex, caller_qn: &str) -> String {
    idx.by_qn
        .get(caller_qn)
        .map(|e| e.label.clone())
        .unwrap_or_else(|| "Function".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_type_identifiers() {
        // Use the Rust primitive set (matches the prior module-level PRIMITIVES).
        let prims = crate::language_provider::provider_for("rust").primitives();
        let cases = vec![
            ("String", vec![]), // primitive
            ("GraphStore", vec!["GraphStore"]),
            ("Vec<GraphStore>", vec!["GraphStore"]),
            ("&'a MyType", vec!["MyType"]),
            ("Option<Result<Foo, Bar>>", vec!["Foo", "Bar"]),
            ("i32", vec![]),
            ("HashMap<String, Value>", vec!["Value"]),
        ];
        for (input, expected) in cases {
            let result = extract_type_identifiers(input, prims);
            assert_eq!(result, expected, "for input: {input}");
        }
    }

    #[test]
    fn test_normalize_import_path_via_provider() {
        // normalize_import_path moved to LanguageProvider (Rust strips crate::).
        let rust = crate::language_provider::provider_for("rust");
        assert_eq!(
            rust.normalize_import_path("crate::graph_store::GraphStore"),
            "graph_store::GraphStore"
        );
        assert_eq!(rust.normalize_import_path("std::io"), "std::io");
        assert_eq!(rust.normalize_import_path("self::foo"), "self::foo");
    }

    #[test]
    fn test_is_external_via_provider() {
        // is_external_crate moved to LanguageProvider::is_external_import.
        let rust = crate::language_provider::provider_for("rust");
        assert!(rust.is_external_import("std::io"));
        assert!(rust.is_external_import("serde::Serialize"));
        assert!(!rust.is_external_import("crate::graph_store"));
        assert!(!rust.is_external_import("self::foo"));
        assert!(!rust.is_external_import("super::bar"));
    }

    #[test]
    fn test_extract_file_from_import_id() {
        assert_eq!(
            extract_file_from_import_id("src/main.rs::graph_store::GraphStore"),
            "src/main.rs"
        );
    }

    #[test]
    fn test_extract_caller_from_callsite_id() {
        assert_eq!(
            extract_caller_from_callsite_id("src/main.rs::main::call@5:4"),
            "src/main.rs::main"
        );
    }

    // -----------------------------------------------------------------
    // EdgeBuffer / AddOutcome — issue #28 regression tests.
    // -----------------------------------------------------------------

    #[test]
    fn test_edge_buffer_distinguishes_persisted_duplicate_and_new() {
        let mut persisted = HashSet::new();
        persisted.insert((
            "Extends_Struct_Struct".to_string(),
            "a".to_string(),
            "b".to_string(),
        ));
        let mut buf = EdgeBuffer::new(persisted);

        // Already in the store from a prior run.
        assert_eq!(
            buf.add("Extends_Struct_Struct", "a", "b", 0.9, "declared-bases"),
            AddOutcome::AlreadyPersisted
        );
        // New in this run.
        assert_eq!(
            buf.add("Extends_Struct_Struct", "c", "d", 0.9, "declared-bases"),
            AddOutcome::Inserted
        );
        // Same (rel_table, from, to) staged again within this run.
        assert_eq!(
            buf.add("Extends_Struct_Struct", "c", "d", 0.9, "declared-bases"),
            AddOutcome::DuplicateInRun
        );
    }

    #[test]
    fn test_edge_buffer_only_flushes_newly_inserted_edges() {
        // AlreadyPersisted and DuplicateInRun must not be queued for
        // another physical write — only Inserted edges reach by_table.
        let mut persisted = HashSet::new();
        persisted.insert((
            "Calls_Function_Function".to_string(),
            "caller".to_string(),
            "callee".to_string(),
        ));
        let mut buf = EdgeBuffer::new(persisted);
        buf.add("Calls_Function_Function", "caller", "callee", 0.9, "x");
        buf.add(
            "Calls_Function_Function",
            "other_caller",
            "callee",
            0.9,
            "x",
        );
        buf.add(
            "Calls_Function_Function",
            "other_caller",
            "callee",
            0.9,
            "x",
        );

        let staged: usize = buf.by_table.values().map(|v| v.len()).sum();
        assert_eq!(
            staged, 1,
            "only the genuinely new (other_caller, callee) edge should be queued for flush"
        );
    }

    #[test]
    fn test_resolve_one_extends_base_success_stages_edge() {
        let mut by_name: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
        by_name.insert(
            "Animal".to_string(),
            vec![SymbolEntry {
                id: "animal_id".to_string(),
                label: "Struct".to_string(),
                qualified_name: "demo::Animal".to_string(),
            }],
        );
        let idx = SymbolIndex {
            by_name,
            by_qn: HashMap::new(),
            by_parent_module: HashMap::new(),
        };
        let mut buf = EdgeBuffer::new(HashSet::new());

        let (resolved, unresolved) = resolve_one_extends_base(
            &idx,
            &mut buf,
            "Struct",
            "Extends_Struct_Struct",
            "demo::Dog",
            "Animal",
        );
        assert_eq!(resolved, 1);
        assert!(unresolved.is_empty());
        let staged: usize = buf.by_table.values().map(|v| v.len()).sum();
        assert_eq!(
            staged, 1,
            "a successful resolution must stage exactly one edge"
        );
    }

    #[test]
    fn test_resolve_one_extends_base_unknown_target_not_counted_resolved() {
        // Target exists in the index but as a label with no declared
        // Extends_<label>_<target_label> rel table (only same-label
        // Extends_X_X tables are declared — see graph_store::REL_TABLES).
        let mut by_name: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
        by_name.insert(
            "Weird".to_string(),
            vec![SymbolEntry {
                id: "weird_id".to_string(),
                label: "TypeAlias".to_string(),
                qualified_name: "demo::Weird".to_string(),
            }],
        );
        let idx = SymbolIndex {
            by_name,
            by_qn: HashMap::new(),
            by_parent_module: HashMap::new(),
        };
        let mut buf = EdgeBuffer::new(HashSet::new());

        let (resolved, unresolved) = resolve_one_extends_base(
            &idx,
            &mut buf,
            "Struct",
            "Extends_Struct_Struct",
            "demo::Dog",
            "Weird",
        );
        assert_eq!(
            resolved, 0,
            "no successful insert must not increment resolved"
        );
        assert_eq!(unresolved.len(), 1);
        let staged: usize = buf.by_table.values().map(|v| v.len()).sum();
        assert_eq!(staged, 0, "an unresolved base must not stage any edge");
    }

    // -----------------------------------------------------------------
    // Non-termination reproduction (2026-07-04) — glob-import resolution.
    //
    // resolve_glob_import scanned idx.by_qn (ALL symbols in the graph)
    // once per glob import. On a repo with a vendored dependency tree
    // (e.g. a .venv), by_qn holds every vendored symbol too, and Python
    // packages commonly re-export via `from .submodule import *` inside
    // __init__.py — so glob-import count scales with the number of
    // vendored packages/files. Cost was O(glob_imports * total_symbols):
    // quadratic in corpus size, not just "large but linear". This test
    // builds a synthetic index (M modules x K symbols each = total
    // symbols) and N glob imports (one per module) and measures wall
    // time. Run manually (ignored by default — timing, not CI-stable):
    //   cargo test --release resolver::tests::bench_glob_import_scaling -- --ignored --nocapture
    // source: measured on 2026-07-04 in this environment (Apple Silicon,
    // `cargo test --release`), numbers reported in the commit message.
    #[test]
    #[ignore]
    fn bench_glob_import_scaling() {
        fn build_index(modules: usize, symbols_per_module: usize) -> SymbolIndex {
            let mut by_name: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
            let mut by_qn: HashMap<String, SymbolEntry> = HashMap::new();
            let mut by_parent_module: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
            for m in 0..modules {
                let module_path = format!("pkg{m}");
                for s in 0..symbols_per_module {
                    let qn = format!("{module_path}::sym{s}");
                    let entry = SymbolEntry {
                        id: format!("file{m}.py::sym{s}"),
                        label: "Function".to_string(),
                        qualified_name: qn.clone(),
                    };
                    by_name
                        .entry(format!("sym{s}"))
                        .or_default()
                        .push(entry.clone());
                    by_qn.insert(qn.clone(), entry.clone());
                    by_parent_module
                        .entry(module_path.clone())
                        .or_default()
                        .push(entry);
                }
            }
            SymbolIndex {
                by_name,
                by_qn,
                by_parent_module,
            }
        }

        let modules = 2_000;
        let symbols_per_module = 50; // total_symbols = 100_000
        let idx = build_index(modules, symbols_per_module);
        let existing: HashSet<(String, String, String)> = HashSet::new();
        let mut buf = EdgeBuffer::new(existing);

        let start = Instant::now();
        let mut total_edges = 0u64;
        for m in 0..modules {
            let file_id = format!("caller{m}.py");
            let module_path = format!("pkg{m}");
            total_edges += resolve_glob_import(&idx, &mut buf, &file_id, &module_path);
        }
        let elapsed = start.elapsed();
        println!(
            "glob-import scaling: modules={modules} symbols/module={symbols_per_module} \
             total_symbols={} glob_imports={modules} edges_generated={total_edges} elapsed={elapsed:?}",
            modules * symbols_per_module
        );
        assert_eq!(total_edges as usize, modules * symbols_per_module);
    }
}
