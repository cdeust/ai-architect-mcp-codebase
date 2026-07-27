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

mod calls;
mod extends;
mod implements;
mod imports;
mod uses;
#[cfg(test)] use imports::resolve_glob_import;
#[cfg(test)] use extends::resolve_one_extends_base;
#[cfg(test)] use uses::extract_type_identifiers;

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

    let (imp_resolved, imp_total, imp_unresolved) = imports::resolve_imports(store, &idx, &mut buf)?;
    let (call_resolved, call_total, call_unresolved) =
        calls::resolve_calls(store, &idx, &file_imports, &mut buf)?;
    let (impl_resolved, impl_total, impl_unresolved) = implements::resolve_implements(store, &idx, &mut buf)?;
    let (ext_resolved, ext_total, ext_unresolved) = extends::resolve_extends(store, &idx, &mut buf)?;
    let (uses_resolved, uses_total, uses_unresolved) =
        uses::resolve_uses(store, &idx, &file_imports, &mut buf)?;

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
