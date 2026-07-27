// resolver tests — moved verbatim from resolver.rs (Fowler "Move") to keep
// the module under the §4.1 cap. No behavior change.

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
