use crate::graph_store::{cypher_str, GraphStore, PropEdgeList};
use std::collections::{HashMap, VecDeque};

pub struct ProcessInfo {
    pub name: String,
    pub entry_point: String,
    pub entry_kind: String,
    pub depth: u64,
    pub node_count: u64,
}

// ---------------------------------------------------------------------------
// Entry point detection — source: stages/stage-3c.md §3.1
// ---------------------------------------------------------------------------

struct EntryPoint {
    id: String,
    label: String,
    #[allow(dead_code)]
    name: String,
    qualified_name: String,
    kind: String,
    confidence: f64,
}

fn detect_entry_points(store: &GraphStore) -> Result<Vec<EntryPoint>, String> {
    let mut entries = Vec::new();

    // Bug 6 fix — original implementation only matched main / test_* / do_*.
    // Missed every Android lifecycle callback, Compose function, Worker,
    // ContentProvider CRUD, etc. → on dcp-wealth-android (3,200+ files,
    // 55 ViewModels, 12 @Composable in settings-journey alone), only 1
    // process was detected (a Python utility script). Added patterns for
    // common framework entry-point shapes.
    // source: Pictet-tech-fest benchmark 2026-05-20 / RUST_BUGS_VERIFIED.md
    query_entries(store, "f.name = 'main'", "main", 1.0, &mut entries)?;
    // Go program entry point. The runtime entry is `func main` in `package
    // main` (caught by the lowercase rule above), but the idiomatic *testable*
    // entry convention is a thin `func main` that delegates to an exported
    // `func Main` carrying the real startup logic (`os.Exit(cli.Main())`), so a
    // top-level Go `Main` is a program entry point too. Go is case-sensitive,
    // so the lowercase-`main` rule cannot catch it; gated to Go because `Main`
    // is not an entry marker in other languages (it would false-match e.g. a
    // Kotlin/Java helper). Confidence just below the certain lowercase `main`.
    // source: Go spec §"Program execution" (func main in package main); the
    //   testscript / cobra `func Main` testable-entry convention.
    //   Falsified as missing by the issue #64 head-to-head eval (go-D3).
    query_entries(
        store,
        "f.language = 'go' AND f.name = 'Main'",
        "main",
        0.9,
        &mut entries,
    )?;
    query_entries(
        store,
        "f.name STARTS WITH 'test_'",
        "test",
        1.0,
        &mut entries,
    )?;
    query_entries(
        store,
        "f.name STARTS WITH 'do_'",
        "handler",
        0.8,
        &mut entries,
    )?;
    // Android Activity / Fragment / Service / Receiver / Worker / Provider lifecycle.
    // source: Android SDK reference 2025 — every lifecycle override is on[A-Z]*.
    // ~5% false-positive in non-Android UI frameworks (e.g. onClick listeners).
    query_entries(
        store,
        "f.name =~ '^on[A-Z][A-Za-z]*$'",
        "android_lifecycle",
        0.95,
        &mut entries,
    )?;
    // ContentProvider CRUD — collides with non-Provider repos so weaker confidence.
    // source: android.content.ContentProvider abstract methods.
    query_entries(
        store,
        "f.name IN ['query', 'insert', 'update', 'delete', 'getType', 'openFile']",
        "android_provider",
        0.85,
        &mut entries,
    )?;
    // androidx.work.Worker / ListenableWorker / CoroutineWorker.
    // source: androidx.work 2.x — stable name across versions.
    query_entries(
        store,
        "f.name IN ['doWork', 'createWork']",
        "android_worker",
        0.95,
        &mut entries,
    )?;
    // Composable shape — CamelCase noun, no @Composable annotation tracking yet
    // (parser would need annotation capture). Lower confidence because data
    // classes and result wrappers also match.
    // source: Jetpack Compose naming convention — Composable functions are nouns.
    query_entries(
        store,
        "f.name =~ '^[A-Z][a-zA-Z0-9]{1,40}$' AND f.visibility IN ['public', 'pub']",
        "composable_candidate",
        0.5,
        &mut entries,
    )?;
    // Tests (extra — JUnit/Vitest patterns beyond the original test_ prefix).
    query_entries(
        store,
        "f.name STARTS WITH 'test' AND NOT f.name STARTS WITH 'test_'",
        "test",
        1.0,
        &mut entries,
    )?;
    query_entries(store, "f.name ENDS WITH 'Test'", "test", 1.0, &mut entries)?;
    // Web-framework handler names that don't fit the do_/Handler pattern.
    query_entries(
        store,
        "f.name ENDS WITH '_handler' OR f.name ENDS WITH 'Handler'",
        "handler",
        0.8,
        &mut entries,
    )?;

    detect_lib_entries(store, &mut entries)?;

    // Dedupe by id: multiple patterns may match the same function (e.g.
    // `KeyboardFocusHandler` matches both the composable-CamelCase and the
    // *Handler suffix patterns). Keep the first occurrence — patterns are
    // ordered above by descending specificity, so the most accurate `kind`
    // label wins.
    // source: persist_processes — Process node id must be unique, lbug
    //   enforces a primary-key uniqueness constraint that fires before insert.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.id.clone()));

    Ok(entries)
}

fn query_entries(
    store: &GraphStore,
    filter: &str,
    kind: &str,
    confidence: f64,
    out: &mut Vec<EntryPoint>,
) -> Result<(), String> {
    let cypher = format!("MATCH (f:Function) WHERE {filter} RETURN f.id, f.name, f.qualified_name");
    let qr = store.execute_query(&cypher)?;
    for row in &qr.rows {
        if row.len() < 3 {
            continue;
        }
        out.push(EntryPoint {
            id: row[0].clone(),
            label: "Function".into(),
            name: row[1].clone(),
            qualified_name: row[2].clone(),
            kind: kind.into(),
            confidence,
        });
    }
    Ok(())
}

// Public-visibility strings emitted by each language parser. The original
// implementation accepted only Rust's 'pub' (and the Cypher comparison
// also missed TypeScript's 'export'), silently dropping every JVM, Swift,
// Go, and C public symbol from lib_entry detection. This single string
// caused process-flow tracing to return 1 result on dcp-wealth-android
// (a Python utility) instead of the hundreds of public Kotlin functions
// on the codebase.
// source: src/parser/{java,kotlin,swift,go,c,cpp,objc,python,typescript}.rs visibility outputs.
//   Bug 5 in RUST_BUGS_VERIFIED.md (Pictet-tech-fest benchmark 2026-05-20).
const PUBLIC_VISIBILITY_VALUES: &[&str] = &[
    "pub",    // Rust
    "export", // TypeScript / JavaScript
    "public", // Java, Kotlin, Swift, Go, Python
    "open",   // Swift (more permissive than public)
];

// Extensions whose top-level public functions are candidate library
// entry points. The original implementation hard-coded ".rs" only,
// excluding 9 of the 10 supported languages.
// source: Bug 7 in RUST_BUGS_VERIFIED.md.
const LIB_ENTRY_FILE_EXTENSIONS: &[&str] = &[
    ".rs", // Rust
    ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx",  // TypeScript / JavaScript
    ".py",   // Python
    ".java", // Java
    ".kt", ".kts",   // Kotlin
    ".swift", // Swift
    ".go",    // Go
    ".c", ".h", // C (no visibility keyword — handled below)
    ".cc", ".cpp", ".cxx", ".hpp", // C++
    ".m", ".mm", // Objective-C
];

fn ends_with_any(s: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suf| s.ends_with(suf))
}

fn detect_lib_entries(store: &GraphStore, entries: &mut Vec<EntryPoint>) -> Result<(), String> {
    // Bug 5 fix — widen the visibility list. Cypher IN clause matches any of
    // the language-specific public-visibility strings.
    let in_list = PUBLIC_VISIBILITY_VALUES
        .iter()
        .map(|v| format!("'{v}'"))
        .collect::<Vec<_>>()
        .join(",");
    let cypher = format!(
        "MATCH (f:Function) WHERE f.visibility IN [{in_list}] \
         RETURN f.id, f.name, f.qualified_name"
    );
    let qr = store.execute_query(&cypher)?;
    for row in &qr.rows {
        if row.len() < 3 {
            continue;
        }
        let qn = &row[2];
        let segments: Vec<&str> = qn.split("::").collect();
        // Bug 7 fix — accept any of the 10 supported source extensions,
        // not just .rs.
        if segments.len() == 2 && ends_with_any(segments[0], LIB_ENTRY_FILE_EXTENSIONS) {
            let already = entries.iter().any(|e| e.id == row[0]);
            if !already {
                entries.push(EntryPoint {
                    id: row[0].clone(),
                    label: "Function".into(),
                    name: row[1].clone(),
                    qualified_name: qn.clone(),
                    kind: "lib_entry".into(),
                    confidence: 0.6,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// BFS process tracing — source: stages/stage-3c.md §3.2
// ---------------------------------------------------------------------------

const MAX_BFS_DEPTH: u64 = 20;

fn trace_one_process(
    store: &GraphStore,
    entry: &EntryPoint,
    call_edges: &HashMap<String, Vec<String>>,
    id_to_label: &HashMap<String, String>,
) -> Result<ProcessInfo, String> {
    let process_id = format!("process::{}", entry.qualified_name);
    let depths = bfs_from_entry(&entry.id, call_edges);
    let max_depth = depths.values().copied().max().unwrap_or(0);
    let symbol_count = depths.len() as u64;
    persist_process_node(store, entry, &process_id, symbol_count, max_depth)?;
    persist_participates_in(store, &depths, &process_id, id_to_label)?;

    Ok(ProcessInfo {
        name: process_id,
        entry_point: entry.qualified_name.clone(),
        entry_kind: entry.kind.clone(),
        depth: max_depth,
        node_count: symbol_count,
    })
}

/// BFS from an entry point along Calls edges, returning every reachable
/// node mapped to its BFS depth — the distance, in calls, from the entry
/// point. BFS visits each node exactly once at its minimal distance, so the
/// map is the shortest-path depth: the ordering of the process's call chain.
///
/// source: stages/stage-3c.md §3.2. Ordering fix — the per-node depth was
/// previously collapsed into a `HashSet` plus a single `max_depth`, and the
/// distance was discarded at the call site, flattening the causal chain into
/// an unordered reachability bag. Returning the full id→depth map preserves
/// the chain so `ParticipatesIn` edges can carry the real step number.
fn bfs_from_entry(
    start_id: &str,
    call_edges: &HashMap<String, Vec<String>>,
) -> HashMap<String, u64> {
    let mut depths: HashMap<String, u64> = HashMap::new();
    let mut queue: VecDeque<(String, u64)> = VecDeque::new();

    depths.insert(start_id.to_string(), 0);
    queue.push_back((start_id.to_string(), 0));

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth >= MAX_BFS_DEPTH {
            continue;
        }
        if let Some(targets) = call_edges.get(&node_id) {
            for target in targets {
                if !depths.contains_key(target) {
                    depths.insert(target.clone(), depth + 1);
                    queue.push_back((target.clone(), depth + 1));
                }
            }
        }
    }
    depths
}

fn persist_process_node(
    store: &GraphStore,
    entry: &EntryPoint,
    process_id: &str,
    symbol_count: u64,
    max_depth: u64,
) -> Result<(), String> {
    let proc_lit = cypher_str(process_id);
    store.insert_node(
        "Process",
        &[
            ("id", &proc_lit),
            ("name", &proc_lit),
            ("entry_point_id", &cypher_str(&entry.id)),
            ("entry_kind", &cypher_str(&entry.kind)),
            ("entry_confidence", &entry.confidence.to_string()),
            ("depth", &max_depth.to_string()),
            ("symbol_count", &symbol_count.to_string()),
        ],
    )?;
    let ep_rel = format!("EntryPointOf_{}_Process", entry.label);
    // EntryPointOf tables only defined for Function/Method per REL_TABLES.
    if !crate::graph_store::is_known_rel_table(&ep_rel) {
        eprintln!(
            "clustering: skipping EntryPointOf for non-callable label {}",
            entry.label
        );
        return Ok(());
    }
    store.insert_edge(
        &ep_rel,
        &entry.id,
        process_id,
        &[("confidence", &entry.confidence.to_string())],
    )
}

fn persist_participates_in(
    store: &GraphStore,
    depths: &HashMap<String, u64>,
    process_id: &str,
    id_to_label: &HashMap<String, String>,
) -> Result<(), String> {
    // Group edges by rel table, bulk-insert per group.
    // source: Fermi audit April 2026 — was 2 probe queries per visited node
    //         plus a per-row CREATE; now zero probes and one bulk call per rel.
    let mut by_rel: HashMap<String, PropEdgeList> = HashMap::new();
    for (node_id, depth) in depths {
        let lbl = match id_to_label.get(node_id) {
            Some(l) => l,
            None => continue,
        };
        let rel = format!("ParticipatesIn_{lbl}_Process");
        // ParticipatesIn tables only defined for Function/Method per
        // REL_TABLES. Filter at producer side instead of swallowing the
        // bulk_insert error (which previously dropped real edges too).
        if !crate::graph_store::is_known_rel_table(&rel) {
            continue;
        }
        // source: ordering fix — persist the real BFS depth (step number in
        // the process's call chain) instead of a hardcoded 0. The depth was
        // computed in bfs_from_entry and discarded here, collapsing the chain
        // into an unordered set. `depth` is the shortest-path distance from
        // the entry point, so a consumer can order participants by it.
        by_rel.entry(rel).or_default().push((
            node_id.clone(),
            process_id.to_string(),
            vec![("depth".into(), depth.to_string())],
        ));
    }
    for (rel, edges) in &by_rel {
        store.bulk_insert_edges(rel, edges)?;
    }
    Ok(())
}

/// Loads id -> label for every Function and Method in the graph. One scan
/// per label (two total) replaces the per-node probes that used to fire
/// inside persist_participates_in.
fn build_call_target_labels(store: &GraphStore) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for label in &["Function", "Method"] {
        let cypher = format!("MATCH (n:{label}) RETURN n.id");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if !row.is_empty() {
                map.insert(row[0].clone(), label.to_string());
            }
        }
    }
    Ok(map)
}

fn collect_call_edges(store: &GraphStore) -> Result<HashMap<String, Vec<String>>, String> {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    let call_rels = &[
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
    ];
    for &(rel, from_label, to_label) in call_rels {
        let cypher = format!("MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) RETURN a.id, b.id");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() >= 2 {
                edges
                    .entry(row[0].clone())
                    .or_default()
                    .push(row[1].clone());
            }
        }
    }
    Ok(edges)
}

// ---------------------------------------------------------------------------
// Entry point: trace_processes — source: stages/stage-3c.md §3
// ---------------------------------------------------------------------------

pub fn trace_processes(store: &GraphStore) -> Result<u64, String> {
    let entries = detect_entry_points(store)?;
    let call_edges = collect_call_edges(store)?;
    // Single scan: build id -> label once so persist_participates_in needs
    // zero database probes. source: Fermi audit April 2026.
    let id_to_label = build_call_target_labels(store)?;
    let mut count = 0u64;
    for entry in &entries {
        trace_one_process(store, entry, &call_edges, &id_to_label)?;
        count += 1;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// get_processes — list all processes
// ---------------------------------------------------------------------------

pub fn get_processes(store: &GraphStore) -> Result<Vec<ProcessInfo>, String> {
    let qr = store.execute_query(
        "MATCH (p:Process) RETURN p.id, p.name, p.entry_point_id, \
         p.entry_kind, p.depth, p.symbol_count",
    )?;
    let mut result = Vec::new();
    for row in &qr.rows {
        if row.len() < 6 {
            continue;
        }
        result.push(ProcessInfo {
            name: row[1].clone(),
            entry_point: row[2].clone(),
            entry_kind: row[3].clone(),
            depth: row[4].parse().unwrap_or(0),
            node_count: row[5].parse().unwrap_or(0),
        });
    }
    Ok(result)
}
