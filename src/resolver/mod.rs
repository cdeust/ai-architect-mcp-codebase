// resolver — Stage 3b resolution pass.
//
// Reads existing nodes from the graph and adds cross-file semantic edges:
// Imports, Calls, Implements, Extends, Uses.
// Runs AFTER index_codebase (3a). Modifies the graph in-place.
//
// source: stages/stage-3b.md §4, §5

use crate::ambiguity_policy::{self, Context as PolicyContext, Resolution as PolicyResolution};
use crate::graph_store::{is_known_rel_table, GraphStore, PropEdgeList};
use crate::language_provider::extract_file_prefix_or_self;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

mod calls;
mod extends;
mod implements;
mod imports;
mod phases;
mod uses;
#[cfg(test)]
use extends::{resolve_one_extends_base, ExtendsCandidate, ExtendsContext};
#[cfg(test)]
use imports::resolve_glob_import;
#[cfg(test)]
use uses::extract_type_identifiers;

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

/// Classifies a base-class/interface name that failed to resolve to a corpus
/// symbol: `EXTERNAL_UNRESOLVED_REASON` iff it matches the last segment of a
/// KNOWN EXTERNAL import in the same file, `"no_target_in_corpus"` otherwise.
///
/// source: issue #216 — fixing the `bases`/`implements` CSV-property bug for
/// Kotlin/Swift/ObjC/C++ activated `resolve_extends`/`resolve_implements` on
/// heritage clauses that previously never resolved at all (the property was
/// never set, so the resolver never even looked). That surfaced a NEW
/// blast-radius gap for Kotlin: `class MainViewModel : ViewModel()` where
/// `ViewModel` comes from `import androidx.lifecycle.ViewModel` (itself
/// correctly classified external by `resolve_imports`) started producing an
/// `UnresolvedRef` tagged `"no_target_in_corpus"` — a reason
/// `cross_repo_bridge.rs`'s cross-repo-candidate filter does NOT recognize as
/// external, so a genuinely external superclass leaked into the candidate
/// pool as noise (`kotlin_external_unresolveds_produce_empty_cross_repo_
/// candidates`, issue #31 defense 2). This gives base-name resolution the
/// same external classification import resolution already has.
///
/// precondition: `lookup` is the base name's last `.`-separated segment (the
/// same text a matching import's `import_last_segment` would produce).
/// postcondition: `EXTERNAL_UNRESOLVED_REASON` iff some import in `file_id`
/// is both `provider.is_external_import` and its `import_last_segment`
/// equals `lookup`; `"no_target_in_corpus"` otherwise.
fn unresolved_base_reason(
    provider: &dyn crate::language_provider::LanguageProvider,
    file_imports: &HashMap<String, Vec<String>>,
    child_qn: &str,
    lookup: &str,
) -> String {
    let file_id = extract_file_prefix_or_self(child_qn);
    let is_external = file_imports
        .get(&file_id)
        .into_iter()
        .flatten()
        .any(|path| {
            provider.is_external_import(path) && provider.import_last_segment(path) == lookup
        });
    if is_external {
        EXTERNAL_UNRESOLVED_REASON.to_string()
    } else {
        "no_target_in_corpus".to_string()
    }
}

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

    let tallies = phases::run_phases(store, &idx, &file_imports, &mut buf)?;
    buf.flush(store)?;
    Ok(tallies.into_result(start))
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
        let file_id = extract_file_prefix_or_self(&row[0]);
        map.entry(file_id).or_default().push(row[1].clone());
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Path normalization helpers
// ---------------------------------------------------------------------------

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
mod tests;
