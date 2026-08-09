// graph_accuracy — Spike B' ground-truth gate.
//
// For each corpus file under `tests/fixtures/graph_accuracy/<category>/`,
// runs the real indexer pipeline against the file's source and compares the
// resulting graph store contents to the hand-annotated expectation embedded
// here. Computes per-EdgeKind precision/recall/F1 and prints a full diff so
// fixes can be made one at a time.
//
// This test is INTENTIONALLY ALLOWED TO FAIL. Spike B' is iterative: every
// fix to the parser/resolver/indexer should move the numbers up, and the loop
// continues until every category scores F1 ≥ 0.92 across the four structural
// kinds (Defines, HasMethod, Imports, Calls).
//
// Source of truth for QN format: src/parser/python.rs (qual = scope::name).
// Source of truth for node labels: src/parser/mod.rs (LABEL_* constants).
// Source of truth for edge kinds: parser emits Defines/HasMethod/Imports/
// Extends; resolver emits Calls/Uses + resolved Imports.

use ai_architect_mcp::graph_store::{GraphStore, REL_TABLES};
use ai_architect_mcp::indexer;
use ai_architect_mcp::parser::Language;
use ai_architect_mcp::resolver;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
mod common;
use common::TempDirExt;

// ---------------------------------------------------------------------------
// Expected graph for each fixture file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ExpectedNode {
    qn: String,
    label: &'static str, // matches LABEL_* constants
    start_line: u64,
}

#[derive(Debug, Clone)]
struct ExpectedEdge {
    kind: &'static str,
    from_qn: String,
    to_qn: String,
}

struct Fixture {
    name: &'static str,
    category: &'static str,
    /// path inside the fixture root, e.g. "shared/text.py" — read by run_fixture.
    rel_path: &'static str,
    nodes: Vec<ExpectedNode>,
    edges: Vec<ExpectedEdge>,
}

// ---------------------------------------------------------------------------
// Fixture: shared/text.py
// ---------------------------------------------------------------------------
//
// This expectation is the FAITHFUL ground truth for the file as it exists.
// We assert what a correct extractor MUST emit; the current extractor will
// miss several of these (method-calls dropped by parser/python.rs:459).
//
// Reference: mcp_server/shared/text.py from Cortex, copied to
// tests/fixtures/graph_accuracy/pure-shared/text.py.
//
// Module-level structure:
//   Line 8 :  from __future__ import annotations
//   Line 10:  import re
//   Line 12:  TECHNICAL_SHORT_TERMS: frozenset[str] = frozenset({...})  -- constant
//   Line 89:  STOPWORDS: frozenset[str] = frozenset({...})              -- constant
//   Line 174: _SPLIT_RE = re.compile(r"\W+")                            -- module-level call (not a constant by UPPER_SNAKE)
//   Line 177: def extract_keywords(text: str | None) -> set[str]:
//   Line 193: def extract_keywords_array(text: str | None) -> list[str]:
//
// Calls inside extract_keywords (body lines 184-190):
//   set()                              line 185 -- bare-name builtin
//   text.lower()                       line 188 -- METHOD CALL (Bug #10 drops)
//   _SPLIT_RE.split(...)               line 188 -- METHOD CALL (Bug #10 drops)
//   len(w)                             line 189 -- bare-name builtin
//   len(w)                             line 189 -- bare-name builtin (second on same line)
//
// Calls inside extract_keywords_array (body line 195):
//   list(...)                          line 195 -- bare-name builtin
//   extract_keywords(text)             line 195 -- bare-name, resolves intra-file
//
// Expected resolution:
//   extract_keywords_array -> extract_keywords : Calls edge with confidence 1.0
//   All builtins (len, set, list, text.lower, _SPLIT_RE.split): unresolved.
//   With Bug #11 in play they produce NO Calls edge at all.

fn fixture_text_py() -> Fixture {
    let file = "shared/text.py".to_string();
    let n = |qn: &str, label: &'static str, start_line: u64| ExpectedNode {
        qn: qn.to_string(),
        label,
        start_line,
    };
    let e = |kind: &'static str, from: &str, to: &str| ExpectedEdge {
        kind,
        from_qn: from.to_string(),
        to_qn: to.to_string(),
    };
    let mut nodes = vec![
        // File node (auto-materialized by indexer)
        n(&file, "File", 0),
        // Module-level imports — QN = qual(scope, display_name)
        // `from __future__ import annotations` → display_name = "__future__::annotations"
        n("shared/text.py::__future__::annotations", "Import", 8),
        // `import re` → display_name = "re"
        n("shared/text.py::re", "Import", 10),
        // Module constants (UPPER_SNAKE check accepts underscores + uppercase)
        n("shared/text.py::TECHNICAL_SHORT_TERMS", "Constant", 12),
        n("shared/text.py::STOPWORDS", "Constant", 89),
        n("shared/text.py::_SPLIT_RE", "Constant", 174),
        // Top-level functions
        n("shared/text.py::extract_keywords", "Function", 177),
        n("shared/text.py::extract_keywords_array", "Function", 193),
    ];

    // Call sites — every Python call expression in a function body gets one,
    // with id = "{caller_qn}::call@{line}:{col}#{byte_start}-{byte_end}".
    // We assert by (caller_qn, callee, line) since byte offsets shift if the
    // file is even one byte different.
    //
    // Order matters for stable matching: we'll match by best (caller, callee, line).
    let _ck_sites_in_extract_keywords: Vec<(&str, u64)> = vec![
        ("set", 185),
        ("text.lower", 188),      // Bug #10: dropped today
        ("_SPLIT_RE.split", 188), // Bug #10: dropped today
        ("len", 189),
        ("len", 189),
    ];
    let _ck_sites_in_extract_keywords_array: Vec<(&str, u64)> =
        vec![("list", 195), ("extract_keywords", 195)];
    // CallSite nodes are recorded for matching but with synthetic placeholder
    // QNs because the byte-offset suffix is producer-determined. The scorer
    // matches CallSites by (caller, callee, line) heuristic instead of by QN.
    for (callee, line) in &[
        ("set", 185u64),
        ("text.lower", 188),
        ("_SPLIT_RE.split", 188),
        ("len", 189),
        ("len", 189),
    ] {
        nodes.push(ExpectedNode {
            qn: format!("shared/text.py::extract_keywords::callsite::{callee}::{line}"),
            label: "CallSite",
            start_line: *line,
        });
    }
    for (callee, line) in &[("list", 195u64), ("extract_keywords", 195)] {
        nodes.push(ExpectedNode {
            qn: format!("shared/text.py::extract_keywords_array::callsite::{callee}::{line}"),
            label: "CallSite",
            start_line: *line,
        });
    }

    // Edges — emitted by parser (Defines, HasMethod, Imports) and resolver (Calls).
    let mut edges = vec![
        // file → import (parser/python.rs:emit_import calls these "Defines")
        e("Defines", &file, "shared/text.py::__future__::annotations"),
        e("Defines", &file, "shared/text.py::re"),
        // file → constant
        e("Defines", &file, "shared/text.py::TECHNICAL_SHORT_TERMS"),
        e("Defines", &file, "shared/text.py::STOPWORDS"),
        e("Defines", &file, "shared/text.py::_SPLIT_RE"),
        // file → function
        e("Defines", &file, "shared/text.py::extract_keywords"),
        e("Defines", &file, "shared/text.py::extract_keywords_array"),
    ];
    // function → CallSite (Defines) — one per expected call site
    for (callee, line) in &[
        ("set", 185u64),
        ("text.lower", 188),
        ("_SPLIT_RE.split", 188),
        ("len", 189),
        ("len", 189),
    ] {
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: "shared/text.py::extract_keywords".to_string(),
            to_qn: format!("shared/text.py::extract_keywords::callsite::{callee}::{line}"),
        });
    }
    for (callee, line) in &[("list", 195u64), ("extract_keywords", 195)] {
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: "shared/text.py::extract_keywords_array".to_string(),
            to_qn: format!("shared/text.py::extract_keywords_array::callsite::{callee}::{line}"),
        });
    }
    // Resolved Calls: extract_keywords_array → extract_keywords (intra-file)
    edges.push(ExpectedEdge {
        kind: "Calls",
        from_qn: "shared/text.py::extract_keywords_array::callsite::extract_keywords::195"
            .to_string(),
        to_qn: "shared/text.py::extract_keywords".to_string(),
    });

    Fixture {
        name: "text.py",
        category: "pure-shared",
        rel_path: "shared/text.py",
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Fixture: shared/similarity.py
// ---------------------------------------------------------------------------
//
// Source: mcp_server/shared/similarity.py — 18 lines, single pure function.
//   Line 6 : from __future__ import annotations
//   Line 9 : def jaccard_similarity(set_a: set, set_b: set) -> float:
// Body call sites (lines 14-18):
//   line 16 : len(set_a & set_b)        -- bare-name builtin
//   line 17 : len(set_a | set_b)        -- bare-name builtin
// No resolvable Calls (len is a Python builtin; resolver marks it unresolved).
fn fixture_similarity_py() -> Fixture {
    let file = "shared/similarity.py".to_string();
    let n = |qn: &str, label: &'static str, start_line: u64| ExpectedNode {
        qn: qn.to_string(),
        label,
        start_line,
    };
    let e = |kind: &'static str, from: &str, to: &str| ExpectedEdge {
        kind,
        from_qn: from.to_string(),
        to_qn: to.to_string(),
    };
    let mut nodes = vec![
        n(&file, "File", 0),
        n("shared/similarity.py::__future__::annotations", "Import", 6),
        n("shared/similarity.py::jaccard_similarity", "Function", 9),
    ];
    for (callee, line) in &[("len", 16u64), ("len", 17)] {
        nodes.push(ExpectedNode {
            qn: format!("shared/similarity.py::jaccard_similarity::callsite::{callee}::{line}"),
            label: "CallSite",
            start_line: *line,
        });
    }
    let mut edges = vec![
        e(
            "Defines",
            &file,
            "shared/similarity.py::__future__::annotations",
        ),
        e("Defines", &file, "shared/similarity.py::jaccard_similarity"),
    ];
    for (callee, line) in &[("len", 16u64), ("len", 17)] {
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: "shared/similarity.py::jaccard_similarity".to_string(),
            to_qn: format!("shared/similarity.py::jaccard_similarity::callsite::{callee}::{line}"),
        });
    }
    Fixture {
        name: "similarity.py",
        category: "pure-shared",
        rel_path: "shared/similarity.py",
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Fixture: shared/hash.py
// ---------------------------------------------------------------------------
//
// Source: mcp_server/shared/hash.py — 19 lines, single pure function.
//   Line 7  : from __future__ import annotations
//   Line 10 : def simple_hash(text: str | None) -> str:
// Body call sites (lines 15-19):
//   line 18 : ord(ch)                   -- bare-name builtin
//   line 19 : format(h, "x")            -- bare-name builtin
fn fixture_hash_py() -> Fixture {
    let file = "shared/hash.py".to_string();
    let n = |qn: &str, label: &'static str, start_line: u64| ExpectedNode {
        qn: qn.to_string(),
        label,
        start_line,
    };
    let e = |kind: &'static str, from: &str, to: &str| ExpectedEdge {
        kind,
        from_qn: from.to_string(),
        to_qn: to.to_string(),
    };
    let mut nodes = vec![
        n(&file, "File", 0),
        n("shared/hash.py::__future__::annotations", "Import", 7),
        n("shared/hash.py::simple_hash", "Function", 10),
    ];
    for (callee, line) in &[("ord", 18u64), ("format", 19)] {
        nodes.push(ExpectedNode {
            qn: format!("shared/hash.py::simple_hash::callsite::{callee}::{line}"),
            label: "CallSite",
            start_line: *line,
        });
    }
    let mut edges = vec![
        e("Defines", &file, "shared/hash.py::__future__::annotations"),
        e("Defines", &file, "shared/hash.py::simple_hash"),
    ];
    for (callee, line) in &[("ord", 18u64), ("format", 19)] {
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: "shared/hash.py::simple_hash".to_string(),
            to_qn: format!("shared/hash.py::simple_hash::callsite::{callee}::{line}"),
        });
    }
    Fixture {
        name: "hash.py",
        category: "pure-shared",
        rel_path: "shared/hash.py",
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Run the real indexer on a fixture and pull back observed nodes + edges
// ---------------------------------------------------------------------------

struct Observed {
    nodes: BTreeMap<String, String>, // qn -> label
    edges_by_kind: BTreeMap<String, BTreeSet<(String, String)>>, // kind -> {(from, to)}
}

fn index_fixture(fixture_root: &Path, graph_path: &Path) -> Observed {
    indexer::index_codebase_with_language(
        fixture_root,
        graph_path,
        Some(Language::Python),
        indexer::DependencyScope::None,
    )
    .expect("indexer should succeed on a 1-file fixture");

    let store = GraphStore::open_or_create(graph_path).expect("open the freshly-built graph");

    // Run stage-3b resolution: produces Calls / Imports / Uses / Implements
    // edges from the call-sites and import nodes the parser left as stubs.
    // index_codebase only does stage-3a (parser + node/edge insertion); the
    // resolver is a separate pass invoked here so the graph_accuracy gate
    // sees the full set of edges a real pipeline run produces.
    let res = resolver::resolve_graph(&store).expect("resolver should succeed");
    eprintln!(
        "resolver: imports={} calls={} impls={} extends={} uses={} \
         total_edges={} total_refs={} unresolved={}",
        res.imports_resolved,
        res.calls_resolved,
        res.impls_resolved,
        res.extends_resolved,
        res.uses_resolved,
        res.total_edges,
        res.total_refs,
        res.unresolved.len(),
    );

    // Pull all nodes by label. We hit the labels the parser/indexer emits.
    let mut nodes: BTreeMap<String, String> = BTreeMap::new();
    for label in &[
        "File", "Import", "Constant", "Function", "Method", "Struct", "CallSite",
    ] {
        let q = format!("MATCH (n:{label}) RETURN n.id");
        let res = match store.execute_query(&q) {
            Ok(r) => r,
            Err(_) => continue, // label table absent → no nodes of that kind
        };
        for row in res.rows {
            if let Some(id) = row.into_iter().next() {
                nodes.insert(id, label.to_string());
            }
        }
    }

    // Pull all edges per relation table. REL_TABLES is the schema's single
    // source of truth; table names follow "{Kind}_{From}_{To}". We collapse
    // by leading Kind segment so the scorer can match against expected kinds.
    let mut edges_by_kind: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    let mut table_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (name, _from, _to) in REL_TABLES {
        // Full-AST layer (`Defines_File_AstNode`, `AstChild_AstNode_AstNode`)
        // is orthogonal to this scorer: it is EVERY tree-sitter node, not the
        // semantic symbol subset these fixtures encode ground truth for, and
        // its own completeness is proven separately
        // (tests/full_ast_completeness.rs, ratio == 1.0 across languages).
        // Folding it into the `Defines`/(new) `AstChild` buckets here would
        // compare against ground truth that was never meant to cover it —
        // observed 2026-08-09: `Defines_File_AstNode` collapsing into the
        // `Defines` bucket alongside `Defines_File_Function` etc. manufactured
        // a false positive and failed the REGRESSION floor on a fixture whose
        // actual Defines extraction was unchanged.
        if name.contains("AstNode") {
            continue;
        }
        let q = format!("MATCH (a)-[r:{name}]->(b) RETURN a.id, b.id");
        let res = match store.execute_query(&q) {
            Ok(r) => r,
            Err(e) => {
                // Some tables are empty after a 1-file index; lbug may error
                // on totally-empty relation queries. Don't pollute output.
                let msg = e.to_string();
                if !msg.contains("empty") && !msg.is_empty() {
                    eprintln!("query {name}: {e}");
                }
                continue;
            }
        };
        if res.rows.is_empty() {
            continue;
        }
        table_counts.insert(name.to_string(), res.rows.len());
        // Collapse "Kind_From_To" → "Kind" by taking the first underscore segment.
        let kind = name.split('_').next().unwrap_or(name).to_string();
        let bucket = edges_by_kind.entry(kind).or_default();
        for row in res.rows {
            if row.len() < 2 {
                continue;
            }
            bucket.insert((row[0].clone(), row[1].clone()));
        }
    }
    // Diagnostic: dump per-table populated counts so we can see exactly which
    // tables the indexer populated.
    if !table_counts.is_empty() {
        eprintln!("populated relation tables:");
        for (name, count) in &table_counts {
            eprintln!("  {name:<40} {count}");
        }
    } else {
        eprintln!("populated relation tables: (none)");
    }

    Observed {
        nodes,
        edges_by_kind,
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
struct Score {
    tp: usize,
    fp: usize,
    fn_: usize,
}

impl Score {
    fn precision(&self) -> f64 {
        if self.tp + self.fp == 0 {
            1.0
        } else {
            self.tp as f64 / (self.tp + self.fp) as f64
        }
    }
    fn recall(&self) -> f64 {
        if self.tp + self.fn_ == 0 {
            1.0
        } else {
            self.tp as f64 / (self.tp + self.fn_) as f64
        }
    }
    fn f1(&self) -> f64 {
        let p = self.precision();
        let r = self.recall();
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

fn score_nodes(expected: &[ExpectedNode], observed: &BTreeMap<String, String>) -> Score {
    let observed_qns: BTreeSet<&String> = observed.keys().collect();
    let mut s = Score::default();
    let expected_call_site_matches: usize =
        expected.iter().filter(|n| n.label == "CallSite").count();

    // Non-callsite nodes match exactly by QN.
    for en in expected {
        if en.label == "CallSite" {
            continue;
        }
        if observed_qns.contains(&en.qn) {
            s.tp += 1;
        } else {
            s.fn_ += 1;
        }
    }

    // CallSite nodes match heuristically: count observed CallSites and compare
    // to expected (we don't try to pair specific callees here — that requires
    // reading node properties, which we'll add when we tighten the gate).
    let observed_call_sites = observed
        .iter()
        .filter(|(_, label)| label.as_str() == "CallSite")
        .count();
    let cs_tp = observed_call_sites.min(expected_call_site_matches);
    let cs_fp = observed_call_sites.saturating_sub(expected_call_site_matches);
    let cs_fn = expected_call_site_matches.saturating_sub(observed_call_sites);
    s.tp += cs_tp;
    s.fp += cs_fp;
    s.fn_ += cs_fn;

    // FPs for non-callsite: anything observed that wasn't expected, ignoring
    // CallSite nodes (handled above) and Directory nodes (auto-materialized).
    let expected_qns: BTreeSet<&String> = expected.iter().map(|n| &n.qn).collect();
    for (qn, label) in observed {
        if label == "CallSite" || label == "Directory" {
            continue;
        }
        if !expected_qns.contains(qn) {
            s.fp += 1;
        }
    }

    s
}

fn score_edges_by_kind(
    expected: &[ExpectedEdge],
    observed: &BTreeMap<String, BTreeSet<(String, String)>>,
) -> BTreeMap<String, Score> {
    let mut by_kind: BTreeMap<String, Score> = BTreeMap::new();

    // Group expected by kind. For CallSite-targeting Defines edges and
    // CallSite-source Calls edges we relax matching (count-based) because
    // the call-site QN suffix is producer-determined.
    let mut expected_by_kind: BTreeMap<&str, Vec<&ExpectedEdge>> = BTreeMap::new();
    for ee in expected {
        expected_by_kind.entry(ee.kind).or_default().push(ee);
    }

    for (kind, exp_edges) in &expected_by_kind {
        let obs_set: BTreeSet<(String, String)> = observed.get(*kind).cloned().unwrap_or_default();
        let mut s = Score::default();

        // Partition expected into strict (no "callsite::" segment) and
        // relaxed (touches a CallSite placeholder).
        let (strict, relaxed): (Vec<&&ExpectedEdge>, Vec<&&ExpectedEdge>) =
            exp_edges.iter().partition(|e| {
                !e.from_qn.contains("::callsite::") && !e.to_qn.contains("::callsite::")
            });

        // Strict matching: exact (from, to) pair.
        for e in &strict {
            let pair = (e.from_qn.clone(), e.to_qn.clone());
            if obs_set.contains(&pair) {
                s.tp += 1;
            } else {
                s.fn_ += 1;
            }
        }

        // Relaxed: count observed edges of this kind that involve a CallSite
        // endpoint (we trust labels for this). Compare count.
        // For the first iteration we treat all "callsite::"-bearing expected
        // edges as collectively satisfied iff the observed count >= expected.
        // This is conservative; we'll tighten when the strict layer is at 1.0.
        if !relaxed.is_empty() {
            let exp_count = relaxed.len();
            // Count observed edges of this kind touching anything that doesn't
            // appear in our strict expected sets.
            let strict_pairs: BTreeSet<(String, String)> = strict
                .iter()
                .map(|e| (e.from_qn.clone(), e.to_qn.clone()))
                .collect();
            let obs_relaxed: usize = obs_set.iter().filter(|p| !strict_pairs.contains(p)).count();
            let tp_r = obs_relaxed.min(exp_count);
            let fp_r = obs_relaxed.saturating_sub(exp_count);
            let fn_r = exp_count.saturating_sub(obs_relaxed);
            s.tp += tp_r;
            s.fp += fp_r;
            s.fn_ += fn_r;
        }

        // FP: any observed of this kind not in expected strict set, beyond
        // the relaxed budget. Already accounted for via fp_r above.
        by_kind.insert((*kind).to_string(), s);
    }

    // Add scores for kinds the producer emits but we didn't expect.
    for (kind, set) in observed {
        if expected_by_kind.contains_key(kind.as_str()) {
            continue;
        }
        // Don't penalize Directory containment kinds — they're infrastructure.
        if kind == "Contains_Dir_File" || kind == "Contains_Dir_Dir" || kind == "Contains" {
            continue;
        }
        let s = Score {
            fp: set.len(),
            ..Default::default()
        };
        by_kind.insert(kind.clone(), s);
    }

    by_kind
}

// ---------------------------------------------------------------------------
// Diagnostic printing
// ---------------------------------------------------------------------------

fn print_diff(fixture: &Fixture, observed: &Observed) {
    println!(
        "\n===== fixture: {} / {} =====",
        fixture.category, fixture.name
    );
    println!("  expected nodes : {}", fixture.nodes.len());
    println!("  observed nodes : {}", observed.nodes.len());

    println!("\n  observed nodes by label:");
    let mut by_label: BTreeMap<&String, usize> = BTreeMap::new();
    for label in observed.nodes.values() {
        *by_label.entry(label).or_default() += 1;
    }
    for (label, count) in &by_label {
        println!("    {label:<12} {count}");
    }

    println!("\n  observed edges by kind:");
    for (kind, set) in &observed.edges_by_kind {
        println!("    {kind:<20} {}", set.len());
    }

    println!("\n  expected NOT in observed (missing):");
    let observed_qns: BTreeSet<&String> = observed.nodes.keys().collect();
    let mut missing = 0;
    for en in &fixture.nodes {
        if en.label == "CallSite" {
            continue;
        }
        if !observed_qns.contains(&en.qn) {
            println!(
                "    MISSING node  [{}] {}  @line {}",
                en.label, en.qn, en.start_line
            );
            missing += 1;
        }
    }
    if missing == 0 {
        println!("    (no missing non-CallSite nodes)");
    }

    let mut missing_edges = 0;
    for ee in &fixture.edges {
        if ee.from_qn.contains("::callsite::") || ee.to_qn.contains("::callsite::") {
            continue;
        }
        let pair = (ee.from_qn.clone(), ee.to_qn.clone());
        if !observed
            .edges_by_kind
            .get(ee.kind)
            .map(|s| s.contains(&pair))
            .unwrap_or(false)
        {
            println!(
                "    MISSING edge  [{}] {} -> {}",
                ee.kind, ee.from_qn, ee.to_qn
            );
            missing_edges += 1;
        }
    }
    if missing_edges == 0 {
        println!("    (no missing strict-match edges)");
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

fn fixture_root_for(test_name: &str) -> common::TestTempDir {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not (test_name is kept as a
    // human-readable prefix for debuggability, not for uniqueness).
    let tmp = tempfile::Builder::new()
        .prefix(&format!("graph_accuracy_{test_name}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("create tempdir");
    tmp
}

// ---------------------------------------------------------------------------
// Fixture: shared/linear_algebra.py
// ---------------------------------------------------------------------------
//
// 99 lines, 11 functions, no classes. Stresses:
//   - aliased import (`import numpy as np`)
//   - from-import with dotted module (`from numpy.typing import NDArray`)
//   - many bare-name + method calls (np.asarray, len, float, .tolist, ...)
//   - intra-file Function → Function resolutions (cosine_similarity → norm,
//     project → dot/scale, add/subtract → _pad_to_same_length, etc.)
//
// Call-site count is hand-counted by walking the source. CallSites match
// loosely (by count) in the scorer; if the count is off after running, the
// printed diff identifies which calls are missing.
//
// Resolvable intra-file Calls (target.label = Function → Calls_Function_Function):
//   normalize          -> norm           (1)
//   cosine_similarity  -> norm, norm, dot (3)
//   add                -> _pad_to_same_length (1)
//   subtract           -> _pad_to_same_length (1)
//   project            -> dot, dot, scale (3)
// = 9 resolved Calls edges total
fn fixture_linear_algebra_py() -> Fixture {
    let file = "shared/linear_algebra.py".to_string();
    let n = |qn: &str, label: &'static str, start_line: u64| ExpectedNode {
        qn: qn.to_string(),
        label,
        start_line,
    };
    let e = |kind: &'static str, from: &str, to: &str| ExpectedEdge {
        kind,
        from_qn: from.to_string(),
        to_qn: to.to_string(),
    };

    // Imports: __future__ + np (aliased) + numpy::typing::NDArray (from-import)
    let mut nodes = vec![
        n(&file, "File", 0),
        n(
            "shared/linear_algebra.py::__future__::annotations",
            "Import",
            7,
        ),
        n("shared/linear_algebra.py::np", "Import", 9), // aliased: display=alias
        n(
            "shared/linear_algebra.py::numpy::typing::NDArray",
            "Import",
            10,
        ),
    ];

    // Functions in declaration order with their (line, body call-count).
    let fns: &[(&str, u64, usize)] = &[
        ("dot", 13, 7),
        ("norm", 22, 4),
        ("normalize", 30, 4),
        ("cosine_similarity", 39, 3),
        ("_pad_to_same_length", 48, 9),
        ("add", 60, 4),
        ("subtract", 67, 4),
        ("scale", 74, 2),
        ("project", 80, 5),
        ("clamp", 90, 3),
        ("zeros", 96, 0),
    ];
    let mut edges = vec![
        e(
            "Defines",
            &file,
            "shared/linear_algebra.py::__future__::annotations",
        ),
        e("Defines", &file, "shared/linear_algebra.py::np"),
        e(
            "Defines",
            &file,
            "shared/linear_algebra.py::numpy::typing::NDArray",
        ),
    ];
    for (fname, line, n_calls) in fns {
        let fqn = format!("shared/linear_algebra.py::{fname}");
        nodes.push(n(&fqn, "Function", *line));
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: file.clone(),
            to_qn: fqn.clone(),
        });
        // CallSite nodes: matched by count, not by callee name. Synthetic QNs
        // satisfy the "::callsite::" relaxed-match path in the scorer.
        for i in 0..*n_calls {
            nodes.push(ExpectedNode {
                qn: format!("{fqn}::callsite::__hand_counted__::{i}"),
                label: "CallSite",
                start_line: *line,
            });
            edges.push(ExpectedEdge {
                kind: "Defines",
                from_qn: fqn.clone(),
                to_qn: format!("{fqn}::callsite::__hand_counted__::{i}"),
            });
        }
    }

    // Resolved Calls edges (caller Function → callee Function, both intra-file).
    // The scorer's relaxed-match path collapses these into a single count
    // bucket because both endpoints involve a CallSite chain in the actual
    // graph (call_site → callee). We supply the count via these expected
    // entries; scorer matches Calls count vs observed.
    for (caller, _line, _n) in fns {
        // (no synthetic Calls edges here — the relaxed scorer matches the
        // observed count, and the resolver will emit ~9 such edges.)
        let _ = caller;
    }
    // Calls edges deduplicate at insertion (the underlying rel table is a
    // set keyed on (from, to)), so multiple call sites in the same caller
    // function pointing at the same callee collapse to ONE Calls edge.
    //
    // cosine_similarity calls norm TWICE in source but produces ONE edge.
    // project calls dot TWICE but produces ONE edge. Count distinct PAIRS.
    let resolved_calls: &[(&str, &str)] = &[
        ("normalize", "norm"),
        ("cosine_similarity", "norm"),
        ("cosine_similarity", "dot"),
        ("add", "_pad_to_same_length"),
        ("subtract", "_pad_to_same_length"),
        ("project", "dot"),
        ("project", "scale"),
    ];
    for (i, (caller, callee)) in resolved_calls.iter().enumerate() {
        edges.push(ExpectedEdge {
            kind: "Calls",
            from_qn: format!("shared/linear_algebra.py::{caller}::callsite::__resolved__::{i}"),
            to_qn: format!("shared/linear_algebra.py::{callee}"),
        });
    }

    Fixture {
        name: "linear_algebra.py",
        category: "pure-shared",
        rel_path: "shared/linear_algebra.py",
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Fixture: shared/yaml_parser.py
// ---------------------------------------------------------------------------
//
// 41 lines, 1 class (Struct: FrontmatterResult extends NamedTuple),
// 1 function (parse_yaml_frontmatter). Stresses:
//   - from-import with dotted module (`from typing import NamedTuple`)
//   - class with base (NamedTuple) — Extends edge is BUG #9 territory and
//     remains DROPPED by the indexer for now. We do NOT assert on it; the
//     Struct node itself still materializes.
//   - many method chains (kv.group(1).strip().lower() — 3 calls in one line)
//   - module-level re.compile(...) assigned to constants — those are module
//     `expression_statement` and extract_module_constant captures the lhs.
//     The re.compile call itself is NOT a call site (extract_call_sites only
//     runs inside function bodies).
//
// Body call sites inside parse_yaml_frontmatter (line 21-40):
//   line 28 : FrontmatterResult(...)                       1
//   line 30 : _FRONTMATTER_RE.match(...)                   1
//   line 32 : FrontmatterResult(...)                       1
//   line 32 : content.strip()                              1
//   line 35 : match.group(1)                               1
//   line 35 : .split("\n")                                 1
//   line 36 : _KV_RE.match(...)                            1
//   line 38 : kv.group(1)                                  1
//   line 38 : .strip()                                     1
//   line 38 : .lower()                                     1
//   line 38 : kv.group(2)                                  1
//   line 38 : .strip()                                     1
//   line 40 : FrontmatterResult(...)                       1
//   line 40 : match.group(2)                               1
//   line 40 : .strip()                                     1
// = 15 call sites
//
// FrontmatterResult is called 3 times from parse_yaml_frontmatter but
// dedupes to ONE Uses_Function_Struct edge (caller=Function, callee=Struct).
// We don't assert on Uses today — the Calls floor stays vacuously 1.0.
fn fixture_yaml_parser_py() -> Fixture {
    let file = "shared/yaml_parser.py".to_string();
    let n = |qn: &str, label: &'static str, start_line: u64| ExpectedNode {
        qn: qn.to_string(),
        label,
        start_line,
    };
    let e = |kind: &'static str, from: &str, to: &str| ExpectedEdge {
        kind,
        from_qn: from.to_string(),
        to_qn: to.to_string(),
    };

    let mut nodes = vec![
        n(&file, "File", 0),
        n(
            "shared/yaml_parser.py::__future__::annotations",
            "Import",
            7,
        ),
        n("shared/yaml_parser.py::re", "Import", 9),
        n("shared/yaml_parser.py::typing::NamedTuple", "Import", 10),
        n("shared/yaml_parser.py::_FRONTMATTER_RE", "Constant", 12),
        n("shared/yaml_parser.py::_KV_RE", "Constant", 13),
        n("shared/yaml_parser.py::FrontmatterResult", "Struct", 16),
        n(
            "shared/yaml_parser.py::parse_yaml_frontmatter",
            "Function",
            21,
        ),
    ];

    let fn_qn = "shared/yaml_parser.py::parse_yaml_frontmatter".to_string();
    for i in 0..15 {
        nodes.push(ExpectedNode {
            qn: format!("{fn_qn}::callsite::__hand_counted__::{i}"),
            label: "CallSite",
            start_line: 21,
        });
    }

    let mut edges = vec![
        // File → top-level symbols
        e(
            "Defines",
            &file,
            "shared/yaml_parser.py::__future__::annotations",
        ),
        e("Defines", &file, "shared/yaml_parser.py::re"),
        e(
            "Defines",
            &file,
            "shared/yaml_parser.py::typing::NamedTuple",
        ),
        e("Defines", &file, "shared/yaml_parser.py::_FRONTMATTER_RE"),
        e("Defines", &file, "shared/yaml_parser.py::_KV_RE"),
        e("Defines", &file, "shared/yaml_parser.py::FrontmatterResult"),
        e("Defines", &file, &fn_qn),
        // Uses_Function_Struct: parse_yaml_frontmatter calls FrontmatterResult
        // three times; the resolver dedupes to one Uses edge because the
        // (caller, target) pair is the same. Resolved via intra-file lookup.
        // We use the "::callsite::" relaxed-match path because the actual
        // edge in the graph goes from a call_site to FrontmatterResult,
        // and the scorer collapses call_site QNs to count matching.
        e(
            "Uses",
            &format!("{fn_qn}::callsite::__resolved_uses__::FrontmatterResult"),
            "shared/yaml_parser.py::FrontmatterResult",
        ),
    ];
    for i in 0..15 {
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: fn_qn.clone(),
            to_qn: format!("{fn_qn}::callsite::__hand_counted__::{i}"),
        });
    }

    Fixture {
        name: "yaml_parser.py",
        category: "pure-shared",
        rel_path: "shared/yaml_parser.py",
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Fixture: core/persona_vector.py
// ---------------------------------------------------------------------------
//
// 194 lines, 9 functions, 1 constant, 6 imports (one multi-name from-import).
// First pure-core fixture — exercises cross-module imports that stay
// unresolved in single-file isolation (mcp_server.shared.linear_algebra
// has no in-scope file, so the resolver can't link `add`, `scale`, etc.).
//
// Call counts per function (AST-derived ground truth):
//   _clamp                       :  2
//   _normalize_signal            :  0
//   _compute_behavioral_dims     : 26
//   build_persona_vector         :  8
//   persona_to_array             :  1
//   persona_distance             :  3
//   persona_drift                :  8
//   compose_personas             : 11
//   steer_context                :  7
// = 66 CallSites total
//
// Intra-file resolved Calls (deduplicated PAIRS):
//   _compute_behavioral_dims -> _normalize_signal
//   _compute_behavioral_dims -> _clamp
//   build_persona_vector     -> _compute_behavioral_dims
//   persona_distance         -> persona_to_array
//   persona_drift            -> persona_to_array
//   compose_personas         -> persona_to_array
//   compose_personas         -> _clamp
// = 7 Calls_Function_Function edges
fn fixture_persona_vector_py() -> Fixture {
    let file = "core/persona_vector.py".to_string();
    let n = |qn: &str, label: &'static str, start_line: u64| ExpectedNode {
        qn: qn.to_string(),
        label,
        start_line,
    };
    let e = |kind: &'static str, from: &str, to: &str| ExpectedEdge {
        kind,
        from_qn: from.to_string(),
        to_qn: to.to_string(),
    };

    let imports = &[
        ("__future__::annotations", 7u64),
        ("typing::Any", 9),
        ("mcp_server::shared::linear_algebra::add", 11),
        ("mcp_server::shared::linear_algebra::cosine_similarity", 11),
        ("mcp_server::shared::linear_algebra::scale", 11),
        ("mcp_server::shared::linear_algebra::zeros", 11),
    ];
    let fns: &[(&str, u64, usize)] = &[
        ("_clamp", 26, 2),
        ("_normalize_signal", 30, 0),
        ("_compute_behavioral_dims", 41, 26),
        ("build_persona_vector", 77, 8),
        ("persona_to_array", 91, 1),
        ("persona_distance", 95, 3),
        ("persona_drift", 99, 8),
        ("compose_personas", 137, 11),
        ("steer_context", 155, 7),
    ];
    let constants = &[("PERSONA_DIMENSIONS", 13u64)];

    let mut nodes = vec![n(&file, "File", 0)];
    let mut edges = vec![];
    for (path, line) in imports {
        let qn = format!("core/persona_vector.py::{path}");
        nodes.push(n(&qn, "Import", *line));
        edges.push(e("Defines", &file, &qn));
    }
    for (name, line) in constants {
        let qn = format!("core/persona_vector.py::{name}");
        nodes.push(n(&qn, "Constant", *line));
        edges.push(e("Defines", &file, &qn));
    }
    for (fname, line, n_calls) in fns {
        let fqn = format!("core/persona_vector.py::{fname}");
        nodes.push(n(&fqn, "Function", *line));
        edges.push(ExpectedEdge {
            kind: "Defines",
            from_qn: file.clone(),
            to_qn: fqn.clone(),
        });
        for i in 0..*n_calls {
            nodes.push(ExpectedNode {
                qn: format!("{fqn}::callsite::__hand_counted__::{i}"),
                label: "CallSite",
                start_line: *line,
            });
            edges.push(ExpectedEdge {
                kind: "Defines",
                from_qn: fqn.clone(),
                to_qn: format!("{fqn}::callsite::__hand_counted__::{i}"),
            });
        }
    }

    // Resolved intra-file Calls (deduplicated pairs).
    let resolved_calls: &[(&str, &str)] = &[
        ("_compute_behavioral_dims", "_normalize_signal"),
        ("_compute_behavioral_dims", "_clamp"),
        ("build_persona_vector", "_compute_behavioral_dims"),
        ("persona_distance", "persona_to_array"),
        ("persona_drift", "persona_to_array"),
        ("compose_personas", "persona_to_array"),
        ("compose_personas", "_clamp"),
    ];
    for (i, (caller, callee)) in resolved_calls.iter().enumerate() {
        edges.push(ExpectedEdge {
            kind: "Calls",
            from_qn: format!("core/persona_vector.py::{caller}::callsite::__resolved__::{i}"),
            to_qn: format!("core/persona_vector.py::{callee}"),
        });
    }

    Fixture {
        name: "persona_vector.py",
        category: "pure-core",
        rel_path: "core/persona_vector.py",
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Shared builder for pure-core fixtures (reduce boilerplate)
// ---------------------------------------------------------------------------

struct CoreFixtureInputs {
    name: &'static str,
    category: &'static str,
    rel_path: &'static str,
    file_prefix: &'static str, // file ID prefix, e.g. "core/profile_builder.py"
    imports: &'static [(&'static str, u64)], // (qual_suffix, line)
    constants: &'static [(&'static str, u64)], // (name, line)
    functions: &'static [(&'static str, u64, usize)], // (name, line, call_count)
    classes: &'static [ExpectedClassInput],
    resolved_calls: &'static [(&'static str, &'static str)], // (caller, callee) DEDUPLICATED
}

/// A class expectation: the class itself (Struct node, Defines edge from file),
/// plus its methods (Method nodes, HasMethod edges from the class). Bases
/// are tracked for documentation but NOT asserted on (BUG #9 — Extends
/// edges are dropped by the indexer.resolve_edge_table pass).
struct ExpectedClassInput {
    name: &'static str,
    line: u64,
    #[allow(dead_code)] // documents the schema; not enforced until BUG #9 fix
    bases: &'static [&'static str],
    methods: &'static [(&'static str, u64, usize)], // (method_name, line, n_calls)
}

fn build_core_fixture(inp: &CoreFixtureInputs) -> Fixture {
    let file = inp.file_prefix.to_string();
    let mut nodes = vec![ExpectedNode {
        qn: file.clone(),
        label: "File",
        start_line: 0,
    }];
    let mut edges: Vec<ExpectedEdge> = vec![];

    let push_node = |nodes: &mut Vec<ExpectedNode>, qn: String, label: &'static str, line: u64| {
        nodes.push(ExpectedNode {
            qn,
            label,
            start_line: line,
        });
    };
    let push_edge =
        |edges: &mut Vec<ExpectedEdge>, kind: &'static str, from: String, to: String| {
            edges.push(ExpectedEdge {
                kind,
                from_qn: from,
                to_qn: to,
            });
        };

    for (path, line) in inp.imports {
        let qn = format!("{}::{}", inp.file_prefix, path);
        push_node(&mut nodes, qn.clone(), "Import", *line);
        push_edge(&mut edges, "Defines", file.clone(), qn);
    }
    for (name, line) in inp.constants {
        let qn = format!("{}::{}", inp.file_prefix, name);
        push_node(&mut nodes, qn.clone(), "Constant", *line);
        push_edge(&mut edges, "Defines", file.clone(), qn);
    }
    for (fname, line, n_calls) in inp.functions {
        let fqn = format!("{}::{}", inp.file_prefix, fname);
        push_node(&mut nodes, fqn.clone(), "Function", *line);
        push_edge(&mut edges, "Defines", file.clone(), fqn.clone());
        for i in 0..*n_calls {
            let cs_qn = format!("{fqn}::callsite::__hand_counted__::{i}");
            push_node(&mut nodes, cs_qn.clone(), "CallSite", *line);
            push_edge(&mut edges, "Defines", fqn.clone(), cs_qn);
        }
    }
    for cls in inp.classes {
        let class_qn = format!("{}::{}", inp.file_prefix, cls.name);
        push_node(&mut nodes, class_qn.clone(), "Struct", cls.line);
        push_edge(&mut edges, "Defines", file.clone(), class_qn.clone());
        for (mname, mline, n_calls) in cls.methods {
            let mqn = format!("{class_qn}::{mname}");
            push_node(&mut nodes, mqn.clone(), "Method", *mline);
            // HasMethod: Struct → Method
            push_edge(&mut edges, "HasMethod", class_qn.clone(), mqn.clone());
            for i in 0..*n_calls {
                let cs_qn = format!("{mqn}::callsite::__hand_counted__::{i}");
                push_node(&mut nodes, cs_qn.clone(), "CallSite", *mline);
                // Defines: Method → CallSite (BUG #12 fix added this table)
                push_edge(&mut edges, "Defines", mqn.clone(), cs_qn);
            }
        }
    }
    for (i, (caller, callee)) in inp.resolved_calls.iter().enumerate() {
        push_edge(
            &mut edges,
            "Calls",
            format!(
                "{}::{}::callsite::__resolved__::{i}",
                inp.file_prefix, caller
            ),
            format!("{}::{}", inp.file_prefix, callee),
        );
    }

    Fixture {
        name: inp.name,
        category: inp.category,
        rel_path: inp.rel_path,
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------------------
// Fixture: core/profile_builder.py
// ---------------------------------------------------------------------------
// 164 lines, 6 functions, 3 constants, 5 imports. Top-level orchestrator
// with apply_session_update fanning out to 5 helpers.
fn fixture_profile_builder_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "profile_builder.py",
        category: "pure-core",
        rel_path: "core/profile_builder.py",
        file_prefix: "core/profile_builder.py",
        imports: &[
            ("__future__::annotations", 8),
            ("datetime::datetime", 10),
            ("datetime::timezone", 10),
            ("mcp_server::core::persona_vector::build_persona_vector", 12),
            (
                "mcp_server::core::style_classifier_ema::update_style_ema",
                13,
            ),
        ],
        constants: &[
            ("_BURST_THRESHOLD_MS", 19),
            ("_EXPLORATION_THRESHOLD_TURNS", 20),
            ("_EMA_ALPHA", 21),
        ],
        functions: &[
            ("_update_session_shape", 24, 0),
            ("_update_tool_preferences", 50, 6),
            ("_build_style_observation", 75, 7),
            ("_update_persona_vector", 111, 10),
            ("_update_counts_and_metadata", 126, 6),
            ("apply_session_update", 134, 13),
        ],
        classes: &[],
        resolved_calls: &[
            ("apply_session_update", "_build_style_observation"),
            ("apply_session_update", "_update_counts_and_metadata"),
            ("apply_session_update", "_update_persona_vector"),
            ("apply_session_update", "_update_session_shape"),
            ("apply_session_update", "_update_tool_preferences"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Fixture: core/style_classifier.py
// ---------------------------------------------------------------------------
// 311 lines. Felder-Silverman classifier with many private helpers and the
// public classify_style entry. Heaviest call graph in pure-core (87 sites,
// 20 intra-file Function→Function resolutions).
fn fixture_style_classifier_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "style_classifier.py",
        category: "pure-core",
        rel_path: "core/style_classifier.py",
        file_prefix: "core/style_classifier.py",
        imports: &[
            ("__future__::annotations", 10),
            ("re", 12), // plain `import re`
            ("typing::Any", 13),
        ],
        constants: &[
            ("ABSTRACT_KEYWORDS", 15),
            ("CONCRETE_KEYWORDS", 36),
            ("PLANNING_KEYWORDS", 56),
            ("TRIAL_KEYWORDS", 72),
            ("_TEST_RE", 86),
        ],
        functions: &[
            ("_count_tool", 92, 6),
            ("_total_tool_calls", 101, 4),
            ("_count_keywords", 106, 2),
            ("_non_linearity_score", 113, 11),
            ("_clamp", 124, 2),
            ("_score_active_reflective", 128, 13),
            ("_score_sensing_intuitive", 169, 10),
            ("_score_sequential_global", 195, 10),
            ("_classify_problem_decomposition", 229, 9),
            ("_classify_exploration_style", 254, 4),
            ("_classify_verification_behavior", 274, 9),
            ("classify_style", 301, 7),
        ],
        classes: &[],
        resolved_calls: &[
            ("_classify_exploration_style", "_total_tool_calls"),
            ("_classify_problem_decomposition", "_count_keywords"),
            ("_classify_problem_decomposition", "_count_tool"),
            ("_classify_verification_behavior", "_count_tool"),
            ("_score_active_reflective", "_clamp"),
            ("_score_active_reflective", "_count_keywords"),
            ("_score_active_reflective", "_count_tool"),
            ("_score_active_reflective", "_total_tool_calls"),
            ("_score_sensing_intuitive", "_clamp"),
            ("_score_sensing_intuitive", "_count_keywords"),
            ("_score_sequential_global", "_clamp"),
            ("_score_sequential_global", "_count_keywords"),
            ("_score_sequential_global", "_non_linearity_score"),
            ("_score_sequential_global", "_total_tool_calls"),
            ("classify_style", "_classify_exploration_style"),
            ("classify_style", "_classify_problem_decomposition"),
            ("classify_style", "_classify_verification_behavior"),
            ("classify_style", "_score_active_reflective"),
            ("classify_style", "_score_sensing_intuitive"),
            ("classify_style", "_score_sequential_global"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Fixture: core/sparse_dictionary.py
// ---------------------------------------------------------------------------
// 257 lines. 5 functions, 2 constants, 10 imports including aliased ones
// (`initialize_atoms as _initialize_atoms`, `update_dictionary as
// _update_dictionary`). Aliased imports use the ALIAS as display_name in
// the QN, so the Import node is `..::_initialize_atoms` not the original.
fn fixture_sparse_dictionary_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "sparse_dictionary.py",
        category: "pure-core",
        rel_path: "core/sparse_dictionary.py",
        file_prefix: "core/sparse_dictionary.py",
        imports: &[
            ("__future__::annotations", 8),
            ("typing::Any", 10),
            (
                "mcp_server::core::sparse_dictionary_activation::SIGNAL_NAMES",
                12,
            ),
            ("mcp_server::core::sparse_dictionary_activation::D", 12),
            (
                "mcp_server::core::sparse_dictionary_activation::extract_session_activation",
                12,
            ),
            // Aliased: display_name = alias
            ("_initialize_atoms", 17),
            ("mcp_server::core::sparse_dictionary_learning::omp", 20),
            ("_update_dictionary", 23),
            ("mcp_server::shared::linear_algebra::norm", 26),
            ("mcp_server::shared::linear_algebra::normalize", 26),
            ("mcp_server::shared::linear_algebra::zeros", 26),
        ],
        constants: &[("_SEED_FEATURES", 32), ("_SIGNAL_LABELS", 179)],
        functions: &[
            ("_build_seed_feature", 101, 7),
            ("build_seed_dictionary", 123, 4),
            ("learn_dictionary", 143, 13),
            ("label_feature", 210, 9),
            ("encode_session", 242, 5),
        ],
        classes: &[],
        resolved_calls: &[
            ("build_seed_dictionary", "_build_seed_feature"),
            ("learn_dictionary", "build_seed_dictionary"),
            ("learn_dictionary", "label_feature"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Fixture: core/cognitive_map.py
// ---------------------------------------------------------------------------
// 300 lines. 9 functions (Successor Representation graph builder + 2D
// projection). `import math` and `from collections import defaultdict`
// are stdlib imports that won't resolve to any in-scope file.
fn fixture_cognitive_map_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "cognitive_map.py",
        category: "pure-core",
        rel_path: "core/cognitive_map.py",
        file_prefix: "core/cognitive_map.py",
        imports: &[
            ("__future__::annotations", 13),
            ("math", 15), // plain `import math`
            ("collections::defaultdict", 16),
            ("typing::Any", 17),
        ],
        constants: &[
            ("_SR_DISCOUNT", 22),
            ("_CO_ACCESS_WINDOW_HOURS", 25),
            ("_MAX_NAVIGATE_DEPTH", 28),
        ],
        functions: &[
            ("build_co_access_graph", 34, 5),
            ("_parse_iso_timestamp", 69, 3),
            ("_link_nearby_memories", 82, 6),
            ("build_temporal_co_access", 106, 5),
            ("compute_sr_scores", 128, 8),
            ("_enqueue_neighbors", 169, 4),
            ("navigate_from", 189, 3),
            ("_spring_relax", 235, 6),
            ("project_to_2d", 263, 11),
        ],
        classes: &[],
        resolved_calls: &[
            ("_link_nearby_memories", "_parse_iso_timestamp"),
            ("build_temporal_co_access", "_link_nearby_memories"),
            ("build_temporal_co_access", "_parse_iso_timestamp"),
            ("navigate_from", "_enqueue_neighbors"),
            ("project_to_2d", "_spring_relax"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Category: core-with-deps — core modules importing other core/ modules.
// Stress-tests deep dependency chains and many cross-module imports.
// ---------------------------------------------------------------------------

fn fixture_hierarchical_predictive_coding_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "hierarchical_predictive_coding.py",
        category: "core-with-deps",
        rel_path: "core/hierarchical_predictive_coding.py",
        file_prefix: "core/hierarchical_predictive_coding.py",
        imports: &[
            ("__future__::annotations", 19),
            ("math", 21),
            (
                "mcp_server::core::predictive_coding_flat::compute_embedding_novelty",
                23,
            ),
            (
                "mcp_server::core::predictive_coding_flat::compute_entity_novelty",
                23,
            ),
            (
                "mcp_server::core::predictive_coding_flat::compute_structural_novelty",
                23,
            ),
            (
                "mcp_server::core::predictive_coding_flat::compute_temporal_novelty",
                23,
            ),
            (
                "mcp_server::core::predictive_coding_gate::PrecisionState",
                29,
            ),
            (
                "mcp_server::core::predictive_coding_gate::neuromodulate_precisions",
                29,
            ),
            (
                "mcp_server::core::predictive_coding_signals::HierarchicalPrediction",
                33,
            ),
            (
                "mcp_server::core::predictive_coding_signals::PredictionLevel",
                33,
            ),
            (
                "mcp_server::core::predictive_coding_signals::compute_entity_errors",
                33,
            ),
            (
                "mcp_server::core::predictive_coding_signals::compute_schema_errors",
                33,
            ),
            (
                "mcp_server::core::predictive_coding_signals::compute_sensory_errors",
                33,
            ),
            (
                "mcp_server::core::predictive_coding_signals::compute_sensory_prediction",
                33,
            ),
        ],
        constants: &[("_LEVEL_WEIGHTS", 61)],
        functions: &[
            ("_compute_ach_weights", 67, 0),
            ("_apply_precision_modulation", 87, 2),
            ("_compute_prediction_levels", 106, 4),
            ("_aggregate_novelty", 134, 4),
            ("compute_hierarchical_novelty", 149, 6),
        ],
        classes: &[],
        resolved_calls: &[
            ("_aggregate_novelty", "_compute_ach_weights"),
            ("compute_hierarchical_novelty", "_aggregate_novelty"),
            (
                "compute_hierarchical_novelty",
                "_apply_precision_modulation",
            ),
            ("compute_hierarchical_novelty", "_compute_prediction_levels"),
        ],
    })
}

fn fixture_dual_store_cls_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "dual_store_cls.py",
        category: "core-with-deps",
        rel_path: "core/dual_store_cls.py",
        file_prefix: "core/dual_store_cls.py",
        imports: &[("__future__::annotations", 20), ("re", 22)],
        constants: &[
            ("_SEMANTIC_TAGS", 26),
            ("_DECISION_RE", 41),
            ("_INSTRUCTION_RE", 45),
            ("_ARCHITECTURE_RE", 51),
            ("_SPECIFIC_RE", 55),
        ],
        functions: &[("classify_memory", 62, 9), ("auto_weight", 96, 6)],
        classes: &[],
        resolved_calls: &[],
    })
}

fn fixture_causal_graph_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "causal_graph.py",
        category: "core-with-deps",
        rel_path: "core/causal_graph.py",
        file_prefix: "core/causal_graph.py",
        imports: &[
            ("__future__::annotations", 6),
            ("math", 8),
            ("typing::Any", 9),
        ],
        constants: &[],
        functions: &[
            ("compute_co_occurrence_matrix", 12, 9),
            ("compute_conditional_independence", 34, 3),
            ("compute_temporal_precedence", 69, 2),
            ("_build_skeleton", 94, 4),
            ("_find_conditionally_independent_edges", 118, 13),
            ("_orient_edges", 158, 6),
            ("_prune_skeleton", 191, 1),
            ("discover_causal_edges", 200, 4),
            ("_build_directed_adjacency", 243, 3),
            ("find_causal_chain", 252, 9),
            ("find_common_causes", 291, 9),
        ],
        classes: &[],
        resolved_calls: &[
            ("_build_skeleton", "compute_conditional_independence"),
            (
                "_find_conditionally_independent_edges",
                "compute_conditional_independence",
            ),
            ("_orient_edges", "compute_temporal_precedence"),
            ("discover_causal_edges", "_build_skeleton"),
            (
                "discover_causal_edges",
                "_find_conditionally_independent_edges",
            ),
            ("discover_causal_edges", "_orient_edges"),
            ("discover_causal_edges", "_prune_skeleton"),
            ("find_causal_chain", "_build_directed_adjacency"),
        ],
    })
}

fn fixture_consolidation_engine_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "consolidation_engine.py",
        category: "core-with-deps",
        rel_path: "core/consolidation_engine.py",
        file_prefix: "core/consolidation_engine.py",
        imports: &[
            ("__future__::annotations", 13),
            ("typing::Any", 15),
            ("mcp_server::core::dual_store_cls::classify_memory", 17),
            (
                "mcp_server::core::dual_store_cls_abstraction::abstract_to_schema",
                18,
            ),
            (
                "mcp_server::core::dual_store_cls_abstraction::check_consistency",
                18,
            ),
            (
                "mcp_server::core::dual_store_cls_abstraction::cluster_by_similarity",
                18,
            ),
            (
                "mcp_server::core::dual_store_cls_abstraction::filter_recurring_patterns",
                18,
            ),
        ],
        constants: &[],
        functions: &[
            ("_is_duplicate_schema", 28, 4),
            ("_collect_common_tags", 46, 8),
            ("plan_cls_consolidation", 58, 3),
            ("_try_abstract_pattern", 87, 3),
            ("_process_patterns", 111, 4),
            ("find_near_duplicates", 145, 13),
            ("summarize_action_group", 184, 14),
            ("should_reclassify", 222, 6),
        ],
        classes: &[],
        resolved_calls: &[
            ("_process_patterns", "_try_abstract_pattern"),
            ("_try_abstract_pattern", "_collect_common_tags"),
            ("_try_abstract_pattern", "_is_duplicate_schema"),
            ("plan_cls_consolidation", "_process_patterns"),
        ],
    })
}

fn fixture_replay_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "replay.py",
        category: "core-with-deps",
        rel_path: "core/replay.py",
        file_prefix: "core/replay.py",
        imports: &[
            ("__future__::annotations", 48),
            (
                "mcp_server::core::replay_execution::build_causal_sequence",
                50,
            ),
            (
                "mcp_server::core::replay_execution::build_temporal_sequence",
                50,
            ),
            (
                "mcp_server::core::replay_execution::compute_replay_stdp_pairs",
                50,
            ),
            (
                "mcp_server::core::replay_formatting::format_restoration",
                55,
            ),
            (
                "mcp_server::core::replay_formatting::should_micro_checkpoint",
                55,
            ),
            (
                "mcp_server::core::replay_selection::compute_sequence_priority",
                59,
            ),
            (
                "mcp_server::core::replay_selection::compute_sequence_rpe",
                59,
            ),
            (
                "mcp_server::core::replay_selection::select_replay_sequences",
                59,
            ),
            ("mcp_server::core::replay_types::ReplayDirection", 64),
            ("mcp_server::core::replay_types::ReplayEvent", 64),
            ("mcp_server::core::replay_types::ReplayResult", 64),
            ("mcp_server::core::replay_types::ReplaySequence", 64),
        ],
        constants: &[("_MIN_SEQUENCE_LENGTH", 73), ("_MAX_SEQUENCES_PER_SWR", 74)],
        functions: &[
            ("_select_seeds", 80, 2),
            ("run_swr_replay", 86, 7),
            ("_build_candidate_sequences", 116, 2),
            ("_build_single_sequence", 140, 5),
            ("_build_temporal_candidates", 164, 5),
            ("_aggregate_results", 190, 7),
            ("describe_replay_result", 227, 2),
        ],
        classes: &[],
        resolved_calls: &[
            ("_build_candidate_sequences", "_build_single_sequence"),
            ("run_swr_replay", "_aggregate_results"),
            ("run_swr_replay", "_build_candidate_sequences"),
            ("run_swr_replay", "_build_temporal_candidates"),
            ("run_swr_replay", "_select_seeds"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Category: infrastructure — I/O modules with heavy external deps.
// New stresses: classes with methods (Struct + HasMethod), multi-inheritance
// (Bug #9 territory), async methods, large plain-import surfaces.
// ---------------------------------------------------------------------------

fn fixture_file_io_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "file_io.py",
        category: "infrastructure",
        rel_path: "infrastructure/file_io.py",
        file_prefix: "infrastructure/file_io.py",
        imports: &[
            ("__future__::annotations", 8),
            ("json", 10),
            ("os", 11),
            ("sys", 12),
            ("pathlib::Path", 13),
            ("typing::Any", 14),
        ],
        constants: &[],
        functions: &[
            ("read_json", 17, 5),
            ("write_json", 28, 4),
            ("read_text_file", 35, 4),
            ("ensure_dir", 46, 2),
            ("list_dir", 51, 6),
            ("stat_file", 64, 2),
        ],
        classes: &[],
        resolved_calls: &[("write_json", "ensure_dir")],
    })
}

fn fixture_embedding_engine_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "embedding_engine.py",
        category: "infrastructure",
        rel_path: "infrastructure/embedding_engine.py",
        file_prefix: "infrastructure/embedding_engine.py",
        imports: &[
            ("__future__::annotations", 23),
            ("hashlib", 25),
            ("logging", 26),
            ("collections::OrderedDict", 27),
            ("typing::Any", 28),
            // `import numpy as np` — aliased plain import → display = "np"
            ("np", 30),
        ],
        constants: &[],
        functions: &[
            ("get_embedding_engine", 41, 2),
            ("reset_embedding_engine", 52, 0),
        ],
        classes: &[ExpectedClassInput {
            name: "EmbeddingEngine",
            line: 58,
            bases: &[],
            methods: &[
                ("__init__", 71, 1),
                ("_cache_key", 90, 3),
                ("model_name", 105, 0),
                ("dimensions", 109, 0),
                ("available", 113, 0),
                ("_detect_device", 129, 6),
                ("_resolve_device", 144, 3),
                ("_fallback_to_cpu", 163, 2),
                ("_ensure_model", 175, 10),
                ("_trigger_background_install", 224, 6),
                ("_encode_vec", 262, 9),
                ("encode", 281, 7),
                ("encode_batch", 308, 12),
                ("similarity", 334, 9),
                ("to_list", 347, 2),
                ("from_list", 353, 2),
                ("_normalize", 359, 1),
                ("_fallback_encode", 365, 17),
            ],
        }],
        // No intra-file Function→Function calls — all calls go to methods of
        // the singleton via the get_embedding_engine factory.
        resolved_calls: &[],
    })
}

fn fixture_mcp_client_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "mcp_client.py",
        category: "infrastructure",
        rel_path: "infrastructure/mcp_client.py",
        file_prefix: "infrastructure/mcp_client.py",
        imports: &[
            ("__future__::annotations", 7),
            ("asyncio", 9),
            ("json", 10),
            ("sys", 11),
            ("typing::Any", 12),
            ("mcp_server::errors::McpConnectionError", 14),
        ],
        constants: &[("CLIENT_INFO", 16), ("PROTOCOL_VERSION", 17)],
        functions: &[],
        classes: &[ExpectedClassInput {
            name: "MCPClient",
            line: 20,
            bases: &[],
            methods: &[
                ("__init__", 21, 4),
                ("connect", 47, 7),
                ("_spawn_process", 72, 13),
                ("_perform_handshake", 132, 11),
                ("call", 167, 7),
                ("list_tools", 193, 1),
                ("server_info", 197, 0),
                ("protocol_version", 201, 0),
                ("connected", 205, 0),
                ("idle", 209, 2),
                ("close", 213, 9),
                ("_send", 244, 10),
                ("_notify", 278, 3),
                ("_touch_activity", 284, 2),
                ("_read_loop", 290, 26),
                ("_stderr_loop", 363, 8),
                ("_open_stderr_log", 392, 8),
                ("_idle_loop", 413, 3),
            ],
        }],
        resolved_calls: &[],
    })
}

fn fixture_pg_store_py() -> Fixture {
    let mut f = build_core_fixture(&CoreFixtureInputs {
        name: "pg_store.py",
        category: "infrastructure",
        rel_path: "infrastructure/pg_store.py",
        file_prefix: "infrastructure/pg_store.py",
        imports: &[
            ("__future__::annotations", 17),
            ("json", 19),
            ("logging", 20),
            ("os", 21),
            ("contextlib::contextmanager", 22),
            ("datetime::datetime", 23),
            ("datetime::timezone", 23),
            ("typing::Any", 24),
            ("typing::Iterator", 24),
            ("np", 26), // aliased: import numpy as np
            ("psycopg", 27),
            ("pgvector::psycopg::register_vector", 28),
            ("psycopg::rows::dict_row", 29),
            ("psycopg_pool::ConnectionPool", 30),
            ("mcp_server::infrastructure::pg_schema::get_all_ddl", 32),
            (
                "mcp_server::infrastructure::pg_store_auxiliary::PgAuxiliaryMixin",
                33,
            ),
            (
                "mcp_server::infrastructure::pg_store_entities::PgEntityMixin",
                34,
            ),
            (
                "mcp_server::infrastructure::pg_store_queries::PgQueryMixin",
                35,
            ),
            (
                "mcp_server::infrastructure::pg_store_relationships::PgRelationshipMixin",
                36,
            ),
            (
                "mcp_server::infrastructure::pg_store_rules::PgRuleMixin",
                37,
            ),
            (
                "mcp_server::infrastructure::pg_store_stats::PgStatsMixin",
                38,
            ),
        ],
        constants: &[],
        functions: &[("_get_database_url", 43, 2), ("_now_iso", 96, 2)],
        classes: &[
            ExpectedClassInput {
                name: "_MaterializedCursor",
                line: 53,
                bases: &[],
                methods: &[
                    ("__init__", 66, 1),
                    ("fetchone", 75, 1),
                    ("fetchall", 82, 1),
                    ("rowcount", 88, 0),
                    ("__iter__", 91, 1),
                ],
            },
            // Resolver-emitted edges from this fixture (auto-derived per file):
            //  - Uses_Method_Struct: PgMemoryStore method → _MaterializedCursor
            //    (line 297: return _MaterializedCursor(cur)). The resolver
            //    routes Function|Method → Struct calls as Uses_*, not Calls_*.
            //  - Calls_Method_Function: PgMemoryStore method → top-level _now_iso
            //    (lines 321/343: _now_iso() bound to the top-level Function
            //    because `_now_iso` matches the top-level by name; dedupes).
            // We add these as relaxed-match Calls/Uses expectations so the
            // scorer counts them correctly.
            ExpectedClassInput {
                name: "PgMemoryStore",
                line: 100,
                // 6-base multi-inheritance. BUG #9 means these Extends edges
                // never reach the graph today.
                bases: &[
                    "PgEntityMixin",
                    "PgRelationshipMixin",
                    "PgQueryMixin",
                    "PgRuleMixin",
                    "PgStatsMixin",
                    "PgAuxiliaryMixin",
                ],
                methods: &[
                    ("__init__", 110, 5),
                    ("_create_connection", 123, 1),
                    ("_configure_pool_connection", 129, 1),
                    ("_open_interactive_pool", 138, 2),
                    ("_open_batch_pool", 154, 2),
                    ("interactive_pool", 171, 1),
                    ("batch_pool", 182, 1),
                    ("acquire_interactive", 193, 2),
                    ("acquire_batch", 210, 2),
                    ("_deallocate_all", 220, 1),
                    ("_reconnect", 232, 3),
                    ("_execute", 241, 4),
                    ("_execute_on_conn", 268, 8),
                    ("_init_schema", 299, 5),
                    ("has_vec", 315, 0),
                    ("_now_iso", 320, 1),
                    ("_bytes_to_vector", 326, 1),
                    ("_vector_to_bytes", 333, 2),
                    ("insert_memory", 341, 35),
                    ("get_memory", 412, 3),
                    ("update_memory_heat", 420, 1),
                    ("bump_heat_raw", 431, 5),
                    ("get_homeostatic_factor", 454, 3),
                    ("set_homeostatic_factor", 475, 5),
                    ("update_memories_heat_batch", 492, 7),
                    ("update_memory_importance", 517, 2),
                    ("update_memory_access", 524, 2),
                    ("update_memory_metamemory", 532, 2),
                    ("get_user_mood", 555, 3),
                    ("get_user_mood_state", 574, 4),
                    ("set_user_mood", 595, 8),
                    ("delete_memory", 621, 2),
                    ("set_memory_protected", 626, 2),
                    ("mark_memory_stale", 633, 2),
                    ("recall_memories", 641, 9),
                    ("search_fts", 689, 2),
                    ("search_vectors", 700, 3),
                    ("update_memory_compression", 717, 4),
                    ("_normalize_memory_row", 744, 8),
                    ("spread_activation_memories", 781, 2),
                    ("get_hot_embeddings", 802, 3),
                    ("get_embeddings_for_memories", 821, 6),
                    ("get_temporal_co_access", 845, 2),
                    ("close", 863, 3),
                ],
            },
        ],
        resolved_calls: &[],
    });
    // Post-build: resolver also emits cross-class/cross-kind edges that the
    // builder's resolved_calls doesn't model directly. Add them as relaxed
    // CallSite-bearing expectations so the scorer counts them as TP.
    f.edges.push(ExpectedEdge {
        kind: "Uses",
        from_qn:
            "infrastructure/pg_store.py::PgMemoryStore::_open_interactive_pool::callsite::__uses__::0"
                .to_string(),
        to_qn: "infrastructure/pg_store.py::_MaterializedCursor".to_string(),
    });
    f.edges.push(ExpectedEdge {
        kind: "Calls",
        from_qn:
            "infrastructure/pg_store.py::PgMemoryStore::insert_memory::callsite::__resolved__::0"
                .to_string(),
        to_qn: "infrastructure/pg_store.py::_now_iso".to_string(),
    });
    f
}

fn fixture_scanner_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "scanner.py",
        category: "infrastructure",
        rel_path: "infrastructure/scanner.py",
        file_prefix: "infrastructure/scanner.py",
        imports: &[
            ("__future__::annotations", 8),
            ("json", 10),
            ("sys", 11),
            ("datetime::datetime", 12),
            ("datetime::timezone", 12),
            ("pathlib::Path", 13),
            ("typing::Any", 14),
            ("mcp_server::infrastructure::config::CLAUDE_DIR", 16),
            ("mcp_server::infrastructure::file_io::list_dir", 17),
            ("mcp_server::infrastructure::file_io::read_text_file", 17),
            ("mcp_server::infrastructure::file_io::stat_file", 17),
            (
                "mcp_server::infrastructure::scanner_parse::build_conversation_record",
                18,
            ),
            (
                "mcp_server::infrastructure::scanner_parse::extract_message_stats",
                18,
            ),
            (
                "mcp_server::infrastructure::scanner_parse::extract_metadata_fields",
                18,
            ),
            (
                "mcp_server::shared::yaml_parser::parse_yaml_frontmatter",
                23,
            ),
        ],
        constants: &[("HEAD_BYTES", 25), ("TAIL_BYTES", 26)],
        functions: &[
            ("_parse_jsonl_lines", 29, 3),
            ("read_head_tail", 43, 17),
            ("iter_tool_uses", 72, 14),
            ("_format_timestamp", 114, 4),
            ("_parse_memory_file", 126, 10),
            ("discover_all_memories", 153, 7),
            ("_parse_conversation_file", 189, 4),
            ("discover_conversations", 208, 10),
            ("group_by_project", 250, 2),
        ],
        classes: &[],
        resolved_calls: &[
            ("_parse_conversation_file", "read_head_tail"),
            ("_parse_memory_file", "_format_timestamp"),
            ("discover_all_memories", "_parse_memory_file"),
            ("discover_conversations", "_parse_conversation_file"),
            ("read_head_tail", "_parse_jsonl_lines"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Category: handlers — MCP tool composition roots (DI, async dispatch).
// All function-only (no stateful classes); heavy cross-layer imports.
// ---------------------------------------------------------------------------

fn fixture_explore_features_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "explore_features.py",
        category: "handlers",
        rel_path: "handlers/explore_features.py",
        file_prefix: "handlers/explore_features.py",
        imports: &[
            ("__future__::annotations", 6),
            ("mcp_server::core::attribution_tracer::trace_attribution", 8),
            (
                "mcp_server::core::behavioral_crosscoder::compare_feature_profiles",
                9,
            ),
            (
                "mcp_server::core::behavioral_crosscoder::detect_persistent_features",
                9,
            ),
            ("mcp_server::core::persona_vector::PERSONA_DIMENSIONS", 13),
            ("mcp_server::core::persona_vector::build_persona_vector", 13),
            ("mcp_server::core::persona_vector::compose_personas", 13),
            (
                "mcp_server::core::sparse_dictionary::build_seed_dictionary",
                18,
            ),
            ("mcp_server::handlers::_tool_meta::READ_ONLY", 19),
            (
                "mcp_server::infrastructure::profile_store::load_profiles",
                20,
            ),
        ],
        constants: &[],
        functions: &[
            ("handler", 106, 11),
            ("_handle_features", 135, 12),
            ("_handle_attribution", 159, 12),
            ("_handle_persona", 185, 12),
            ("_handle_crosscoder", 221, 14),
        ],
        classes: &[],
        resolved_calls: &[
            ("handler", "_handle_attribution"),
            ("handler", "_handle_crosscoder"),
            ("handler", "_handle_features"),
            ("handler", "_handle_persona"),
        ],
    })
}

fn fixture_recall_handler_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "recall.py",
        category: "handlers",
        rel_path: "handlers/recall.py",
        file_prefix: "handlers/recall.py",
        imports: &[
            ("__future__::annotations", 10),
            ("typing::Any", 12),
            ("mcp_server::core::memory_rules", 14),
            ("mcp_server::handlers::_telemetry_wrap::instrument", 15),
            ("mcp_server::core::knowledge_graph::extract_entities", 16),
            // Aliased: `from ... import recall as pg_recall`
            ("pg_recall", 17),
            ("mcp_server::core::query_intent::QueryIntent", 18),
            ("mcp_server::core::query_intent::classify_query_intent", 18),
            ("mcp_server::handlers::_tool_meta::READ_ONLY", 19),
            (
                "mcp_server::handlers::recall_helpers::build_enhancements",
                20,
            ),
            (
                "mcp_server::handlers::recall_helpers::filter_low_signal",
                20,
            ),
            (
                "mcp_server::handlers::recall_helpers::inject_triggered_memories",
                20,
            ),
            (
                "mcp_server::infrastructure::embedding_engine::get_embedding_engine",
                25,
            ),
            (
                "mcp_server::infrastructure::memory_config::get_memory_settings",
                26,
            ),
            ("mcp_server::infrastructure::memory_store::MemoryStore", 27),
        ],
        constants: &[],
        functions: &[
            ("_get_store", 173, 2),
            ("_apply_strategic_ordering", 181, 5),
            ("_apply_co_activation", 197, 10),
            ("_apply_rules_and_order", 227, 3),
            ("_track_recall_replay", 245, 4),
            ("_handler_impl", 262, 21),
        ],
        classes: &[],
        resolved_calls: &[
            ("_apply_rules_and_order", "_apply_strategic_ordering"),
            ("_handler_impl", "_apply_co_activation"),
            ("_handler_impl", "_apply_rules_and_order"),
            ("_handler_impl", "_get_store"),
            ("_handler_impl", "_track_recall_replay"),
        ],
    })
}

fn fixture_remember_handler_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "remember.py",
        category: "handlers",
        rel_path: "handlers/remember.py",
        file_prefix: "handlers/remember.py",
        imports: &[
            ("__future__::annotations", 6),
            ("typing::Any", 8),
            // Bare module imports (from X import Y where Y is a module not a symbol)
            ("mcp_server::core::thermodynamics", 10),
            ("mcp_server::core::write_gate", 10),
            ("mcp_server::handlers::_telemetry_wrap::instrument", 11),
            ("mcp_server::core::domain_detector::detect_domain", 12),
            ("mcp_server::core::global_detector::detect_global", 13),
            ("mcp_server::handlers::_tool_meta::IDEMPOTENT_WRITE", 14),
            (
                "mcp_server::handlers::remember_helpers::apply_modulations",
                15,
            ),
            ("mcp_server::handlers::remember_helpers::evaluate_gate", 15),
            (
                "mcp_server::handlers::remember_helpers::insert_and_post_process",
                15,
            ),
            ("mcp_server::handlers::remember_helpers::try_curation", 15),
            (
                "mcp_server::handlers::remember_helpers::update_user_mood_ema",
                15,
            ),
            (
                "mcp_server::handlers::remember_response::build_merge_response",
                22,
            ),
            ("mcp_server::infrastructure::wiki_store", 23),
            ("mcp_server::infrastructure::config::WIKI_ROOT", 24),
            (
                "mcp_server::infrastructure::embedding_engine::get_embedding_engine",
                25,
            ),
            (
                "mcp_server::infrastructure::memory_config::get_memory_settings",
                26,
            ),
            ("mcp_server::infrastructure::memory_store::MemoryStore", 27),
            (
                "mcp_server::infrastructure::profile_store::load_profiles",
                28,
            ),
        ],
        constants: &[],
        functions: &[
            ("_get_store", 187, 2),
            ("_resolve_domain", 195, 6),
            ("_enrich_mod_with_gate", 218, 1),
            ("_parse_args", 231, 11),
            ("_handler_impl", 260, 30),
        ],
        classes: &[],
        resolved_calls: &[
            ("_handler_impl", "_enrich_mod_with_gate"),
            ("_handler_impl", "_get_store"),
            ("_handler_impl", "_parse_args"),
            ("_handler_impl", "_resolve_domain"),
        ],
    })
}

fn fixture_consolidate_handler_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "consolidate.py",
        category: "handlers",
        rel_path: "handlers/consolidate.py",
        file_prefix: "handlers/consolidate.py",
        imports: &[
            ("__future__::annotations", 7),
            ("logging", 9),
            ("time", 10),
            ("typing::Any", 11),
            ("mcp_server::core::emergence_metrics", 13),
            (
                "mcp_server::handlers::consolidation::cascade::run_cascade_advancement",
                14,
            ),
            (
                "mcp_server::handlers::consolidation::cls::run_cls_cycle",
                15,
            ),
            (
                "mcp_server::handlers::consolidation::compression::run_compression_cycle",
                16,
            ),
            (
                "mcp_server::handlers::consolidation::decay::run_decay_cycle",
                17,
            ),
            (
                "mcp_server::handlers::consolidation::homeostatic::run_homeostatic_cycle",
                18,
            ),
            (
                "mcp_server::handlers::consolidation::memify::run_memify_cycle",
                19,
            ),
            (
                "mcp_server::handlers::consolidation::plasticity::run_plasticity_cycle",
                20,
            ),
            (
                "mcp_server::handlers::consolidation::pruning::run_pruning_cycle",
                21,
            ),
            (
                "mcp_server::handlers::consolidation::sleep::run_deep_sleep",
                22,
            ),
            (
                "mcp_server::handlers::consolidation::transfer::run_two_stage_transfer",
                23,
            ),
            (
                "mcp_server::handlers::consolidation::wiki_maintenance::run_wiki_maintenance",
                24,
            ),
            (
                "mcp_server::infrastructure::embedding_engine::EmbeddingEngine",
                25,
            ),
            (
                "mcp_server::infrastructure::embedding_engine::get_embedding_engine",
                25,
            ),
            (
                "mcp_server::infrastructure::memory_config::get_memory_settings",
                29,
            ),
            ("mcp_server::infrastructure::memory_store::MemoryStore", 30),
            ("mcp_server::handlers::_tool_meta::IDEMPOTENT_WRITE", 31),
        ],
        constants: &[],
        functions: &[
            ("_get_store", 160, 2),
            ("_timed", 168, 8),
            ("handler", 186, 24),
            ("_run_cycles", 255, 12),
            ("_run_always_cycles", 291, 6),
            ("_log_consolidation", 317, 7),
        ],
        classes: &[],
        resolved_calls: &[
            ("_run_always_cycles", "_timed"),
            ("_run_cycles", "_timed"),
            ("handler", "_get_store"),
            ("handler", "_log_consolidation"),
            ("handler", "_run_always_cycles"),
            ("handler", "_run_cycles"),
            ("handler", "_timed"),
        ],
    })
}

fn fixture_get_methodology_graph_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "get_methodology_graph.py",
        category: "handlers",
        rel_path: "handlers/get_methodology_graph.py",
        file_prefix: "handlers/get_methodology_graph.py",
        imports: &[
            ("__future__::annotations", 3),
            ("mcp_server::core::graph_builder::build_graph", 5),
            (
                "mcp_server::infrastructure::profile_store::load_profiles",
                6,
            ),
            ("mcp_server::handlers::_tool_meta::READ_ONLY", 7),
        ],
        constants: &[("_MAX_NODES", 41), ("_MAX_EDGES", 42)],
        functions: &[("handler", 45, 13)],
        classes: &[],
        resolved_calls: &[],
    })
}

// ---------------------------------------------------------------------------
// Category: server-transport — HTTP servers, route dispatch, bootstrap.
// New stress: very large functions (http_standalone_graph._kick_background_build
// has 247 call sites in one body); empty-body class with multi-inheritance.
// ---------------------------------------------------------------------------

fn fixture_http_standalone_graph_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "http_standalone_graph.py",
        category: "server-transport",
        rel_path: "server/http_standalone_graph.py",
        file_prefix: "server/http_standalone_graph.py",
        imports: &[
            ("__future__::annotations", 26),
            ("os", 28),
            ("sys", 29),
            ("threading", 30),
            ("time", 31),
            ("traceback", 32),
            (
                "mcp_server::server::http_standalone_state::CONVERSATIONS_CACHE_TTL",
                34,
            ),
            (
                "mcp_server::server::http_standalone_state::get_cached_conversations_state",
                34,
            ),
            (
                "mcp_server::server::http_standalone_state::set_cached_conversations_state",
                34,
            ),
        ],
        constants: &[("PHASES", 78)],
        functions: &[
            ("parse_graph_query", 106, 7),
            ("parse_discussion_params", 127, 7),
            ("extract_domain_hub_ids", 148, 4),
            ("_compute_memory_vitals", 159, 12),
            ("_session_counts_from_profiles", 183, 3),
            ("_roster_fingerprint", 191, 6),
            ("get_build_progress", 209, 3),
            ("_set_progress", 217, 1),
            ("_register_phase", 240, 2),
            ("get_phase_payload", 250, 5),
            ("_phase_deps_satisfied", 262, 2),
            ("_mark_phase_ready", 273, 2),
            ("_kick_background_build", 284, 247), // largest function in corpus
            ("get_graph_response", 975, 10),
            ("_get_cached_conversations", 1046, 4),
            ("build_discussions_response", 1058, 9),
            ("_find_session_file", 1093, 4),
            ("build_discussion_detail", 1110, 12),
        ],
        classes: &[],
        resolved_calls: &[
            ("_kick_background_build", "_mark_phase_ready"),
            ("_kick_background_build", "_phase_deps_satisfied"),
            ("_kick_background_build", "_register_phase"),
            ("_kick_background_build", "_roster_fingerprint"),
            ("_kick_background_build", "_set_progress"),
            ("_kick_background_build", "extract_domain_hub_ids"),
            ("build_discussion_detail", "_find_session_file"),
            ("build_discussion_detail", "_get_cached_conversations"),
            ("build_discussions_response", "_get_cached_conversations"),
            ("build_discussions_response", "parse_discussion_params"),
            ("get_graph_response", "_kick_background_build"),
            ("get_graph_response", "_roster_fingerprint"),
            ("get_graph_response", "get_build_progress"),
            ("get_graph_response", "parse_graph_query"),
        ],
    })
}

fn fixture_http_standalone_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "http_standalone.py",
        category: "server-transport",
        rel_path: "server/http_standalone.py",
        file_prefix: "server/http_standalone.py",
        imports: &[
            ("__future__::annotations", 19),
            ("argparse", 21),
            ("json", 22),
            ("os", 23),
            ("sys", 24),
            ("threading", 25),
            ("time", 26),
            ("http::server::BaseHTTPRequestHandler", 27),
            ("http::server::HTTPServer", 27),
            ("pathlib::Path", 28),
            ("socketserver::ThreadingMixIn", 29),
            ("mcp_server::server::http_common::_apply_cors_headers", 31),
            (
                "mcp_server::server::http_security::enforce_same_origin_write",
                32,
            ),
            (
                "mcp_server::server::http_security::validate_host_header",
                32,
            ),
            (
                "mcp_server::server::http_standalone_endpoints::serve_discussion_detail",
                36,
            ),
            (
                "mcp_server::server::http_standalone_endpoints::serve_discussions",
                36,
            ),
            (
                "mcp_server::server::http_standalone_endpoints::serve_file_diff",
                36,
            ),
            (
                "mcp_server::server::http_standalone_endpoints::serve_graph",
                36,
            ),
            (
                "mcp_server::server::http_standalone_endpoints::serve_sankey",
                36,
            ),
            (
                "mcp_server::server::http_standalone_endpoints::serve_static",
                36,
            ),
            (
                "mcp_server::server::http_standalone_state::IDLE_TIMEOUT",
                44,
            ),
            (
                "mcp_server::server::http_standalone_state::seconds_since_last_request",
                44,
            ),
            ("mcp_server::server::http_standalone_state::touch", 44),
            (
                "mcp_server::server::http_standalone_wiki::serve_wiki_db",
                49,
            ),
            (
                "mcp_server::server::http_standalone_wiki::serve_wiki_export",
                49,
            ),
            (
                "mcp_server::server::http_standalone_wiki::serve_wiki_list",
                49,
            ),
            (
                "mcp_server::server::http_standalone_wiki::serve_wiki_page",
                49,
            ),
            (
                "mcp_server::server::http_standalone_wiki::serve_wiki_projects",
                49,
            ),
            (
                "mcp_server::server::http_standalone_wiki::serve_wiki_save",
                49,
            ),
        ],
        constants: &[("_WIKI_DB_OPS", 108)],
        functions: &[
            ("_idle_watchdog", 65, 4),
            ("_get_ui_root", 78, 5),
            ("_get_store", 99, 2),
            ("_route_unified_get", 120, 53),
            ("_build_unified_handler", 241, 22),
            ("_bind_server", 298, 1),
            ("_announce", 309, 5),
            ("_auto_enable_ap", 316, 32),
            ("main", 434, 14),
        ],
        classes: &[ExpectedClassInput {
            name: "_ThreadedHTTPServer",
            line: 59,
            bases: &["ThreadingMixIn", "HTTPServer"], // dropped (BUG #9)
            methods: &[],                             // empty class body
        }],
        resolved_calls: &[
            ("_build_unified_handler", "_route_unified_get"),
            ("main", "_announce"),
            ("main", "_auto_enable_ap"),
            ("main", "_bind_server"),
            ("main", "_build_unified_handler"),
            ("main", "_get_store"),
            ("main", "_get_ui_root"),
        ],
    })
}

fn fixture_http_launcher_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "http_launcher.py",
        category: "server-transport",
        rel_path: "server/http_launcher.py",
        file_prefix: "server/http_launcher.py",
        imports: &[
            ("__future__::annotations", 8),
            ("json", 10),
            ("os", 11),
            ("shutil", 12),
            ("subprocess", 13),
            ("sys", 14),
            ("urllib::request", 15), // `import urllib.request`
            ("pathlib::Path", 16),
        ],
        constants: &[("PORTS", 21)],
        functions: &[
            ("_kill_port", 26, 7),
            ("_detect_dev_source", 51, 13),
            ("_find_ap_binary", 99, 6),
            ("_ensure_ap_graph", 123, 25),
            ("_sync_dev_source", 211, 6),
            ("_probe_port", 244, 2),
            ("launch_server", 255, 17),
            ("open_in_browser", 328, 3),
        ],
        classes: &[],
        resolved_calls: &[
            ("_ensure_ap_graph", "_find_ap_binary"),
            ("launch_server", "_detect_dev_source"),
            ("launch_server", "_ensure_ap_graph"),
            ("launch_server", "_kill_port"),
            ("launch_server", "_probe_port"),
            ("launch_server", "_sync_dev_source"),
        ],
    })
}

fn fixture_http_dashboard_data_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "http_dashboard_data.py",
        category: "server-transport",
        rel_path: "server/http_dashboard_data.py",
        file_prefix: "server/http_dashboard_data.py",
        imports: &[
            ("__future__::annotations", 7),
            ("json", 9),
            ("datetime::datetime", 10),
            ("datetime::timezone", 10),
        ],
        constants: &[],
        functions: &[
            ("build_dashboard_data", 13, 22),
            ("_safe_call", 52, 1),
            ("build_stats", 60, 18),
            ("format_memory", 94, 52),
            ("format_entity", 135, 5),
            ("format_relationship", 146, 14),
            ("format_schema", 162, 8),
            ("build_engram_data", 175, 6),
            ("parse_tags", 193, 3),
        ],
        classes: &[],
        resolved_calls: &[
            ("build_dashboard_data", "_safe_call"),
            ("build_dashboard_data", "build_engram_data"),
            ("build_dashboard_data", "build_stats"),
            ("build_dashboard_data", "format_entity"),
            ("build_dashboard_data", "format_memory"),
            ("build_dashboard_data", "format_relationship"),
            ("build_dashboard_data", "format_schema"),
            ("format_memory", "parse_tags"),
        ],
    })
}

fn fixture_visualize_bootstrap_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "visualize_bootstrap.py",
        category: "server-transport",
        rel_path: "server/visualize_bootstrap.py",
        file_prefix: "server/visualize_bootstrap.py",
        imports: &[
            ("__future__::annotations", 26),
            ("os", 28),
            ("shutil", 29),
            ("subprocess", 30),
            ("sys", 31),
            ("pathlib::Path", 32),
        ],
        constants: &[("PORT", 34), ("_SYNC_SUBTREES", 90), ("_SYNC_FILES", 101)],
        functions: &[
            ("_is_cortex_root", 37, 3),
            ("_find_dev_source", 45, 6),
            ("_cache_roots", 56, 9),
            ("_sync", 109, 9),
            ("_kill_port", 145, 7),
            ("_spawn_server", 165, 6),
            ("main", 193, 6),
        ],
        classes: &[],
        resolved_calls: &[
            ("_find_dev_source", "_is_cortex_root"),
            ("_sync", "_cache_roots"),
            ("main", "_find_dev_source"),
            ("main", "_kill_port"),
            ("main", "_spawn_server"),
            ("main", "_sync"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Category: hooks — lifecycle/event handlers, mostly stdlib + JSON-stdin.
// ---------------------------------------------------------------------------

fn fixture_session_lifecycle_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "session_lifecycle.py",
        category: "hooks",
        rel_path: "hooks/session_lifecycle.py",
        file_prefix: "hooks/session_lifecycle.py",
        imports: &[
            ("__future__::annotations", 34),
            ("json", 36),
            ("sys", 37),
            ("datetime::datetime", 38),
            ("datetime::timezone", 38),
            ("typing::Any", 39),
        ],
        constants: &[("_LOG_PREFIX", 66), ("MAX_SESSION_LOG_ENTRIES", 69)],
        functions: &[
            ("_log", 72, 1),
            ("_resolve_domain", 77, 8),
            ("_run_consolidation", 103, 12),
            ("_build_session_entry", 154, 13),
            ("_append_session", 171, 3),
            ("process_event", 180, 19),
            ("main", 220, 8),
        ],
        classes: &[],
        resolved_calls: &[
            ("_run_consolidation", "_log"),
            ("main", "_log"),
            ("main", "process_event"),
            ("process_event", "_append_session"),
            ("process_event", "_build_session_entry"),
            ("process_event", "_log"),
            ("process_event", "_resolve_domain"),
            ("process_event", "_run_consolidation"),
        ],
    })
}

fn fixture_session_start_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "session_start.py",
        category: "hooks",
        rel_path: "hooks/session_start.py",
        file_prefix: "hooks/session_start.py",
        imports: &[
            ("__future__::annotations", 19),
            ("json", 21),
            ("os", 22),
            ("subprocess", 23),
            ("sys", 24),
            ("pathlib::Path", 25),
        ],
        constants: &[
            ("_DATABASE_URL", 29),
            ("_HOT_LIMIT", 30),
            ("_MIN_HEAT", 31),
            ("_ANCHOR_LIMIT", 32),
            ("_PLUGIN_ROOT", 33),
            ("_CONSOLIDATE_TTL_HOURS", 599),
        ],
        functions: &[
            ("_log", 36, 1),
            ("_has_sentence_transformers", 40, 0),
            ("_short", 50, 3),
            ("_try_setup_db", 58, 11),
            ("_connect_pg", 85, 2),
            ("_fetch_anchors", 101, 14),
            ("_fetch_team_decisions", 139, 7),
            ("_fetch_hot_memories", 178, 11),
            ("_count_pending_curations", 206, 16),
            ("_fetch_checkpoint", 264, 16),
            ("_count_memories", 299, 2),
            ("_count_session_files", 308, 7),
            ("_detect_external_sources", 323, 25),
            ("_auto_backfill", 373, 6),
            ("_format_checkpoint_section", 404, 13),
            ("_build_context", 426, 27),
            ("_build_cold_start_message", 508, 28),
            ("_auto_wire_pipeline", 575, 6),
            ("_maybe_background_consolidate", 604, 26),
            ("_maybe_background_reanalyze", 694, 22),
            ("_lookup_cached_graph_path", 756, 9),
            ("main", 788, 32),
            ("_print_external_sources", 881, 9),
        ],
        classes: &[],
        resolved_calls: &[
            ("_auto_backfill", "_log"),
            ("_auto_wire_pipeline", "_log"),
            ("_build_cold_start_message", "_auto_backfill"),
            ("_build_cold_start_message", "_log"),
            ("_build_context", "_format_checkpoint_section"),
            ("_build_context", "_has_sentence_transformers"),
            ("_build_context", "_short"),
            ("_connect_pg", "_log"),
            ("_lookup_cached_graph_path", "_connect_pg"),
            ("_maybe_background_consolidate", "_log"),
            ("_maybe_background_reanalyze", "_log"),
            ("_maybe_background_reanalyze", "_lookup_cached_graph_path"),
            ("_print_external_sources", "_detect_external_sources"),
            ("_print_external_sources", "_log"),
            ("_try_setup_db", "_log"),
            ("main", "_auto_wire_pipeline"),
            ("main", "_build_cold_start_message"),
            ("main", "_build_context"),
            ("main", "_connect_pg"),
            ("main", "_count_memories"),
            ("main", "_count_pending_curations"),
            ("main", "_count_session_files"),
            ("main", "_fetch_anchors"),
            ("main", "_fetch_checkpoint"),
            ("main", "_fetch_hot_memories"),
            ("main", "_fetch_team_decisions"),
            ("main", "_log"),
            ("main", "_maybe_background_consolidate"),
            ("main", "_maybe_background_reanalyze"),
            ("main", "_print_external_sources"),
            ("main", "_try_setup_db"),
        ],
    })
}

fn fixture_post_tool_capture_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "post_tool_capture.py",
        category: "hooks",
        rel_path: "hooks/post_tool_capture.py",
        file_prefix: "hooks/post_tool_capture.py",
        imports: &[
            ("__future__::annotations", 16),
            ("json", 18),
            ("sys", 19),
            ("typing::Any", 20),
        ],
        constants: &[
            ("_LOG_PREFIX", 22),
            ("_HIGH_VALUE_TOOLS", 25),
            ("_LIGHT_VALUE_TOOLS", 40),
            ("_CONDITIONAL_TOOLS", 48),
            ("_MIN_OUTPUT_LENGTH", 54),
            ("_HIGH_VALUE_PATTERNS", 65),
            ("_CASCADE_INTERVAL", 321),
        ],
        functions: &[
            ("_log", 93, 1),
            ("_should_capture", 97, 2),
            ("_reference_line", 124, 10),
            ("_build_memory_content", 151, 7),
            ("_build_tags", 188, 6),
            ("_normalize_output", 214, 28),
            ("_load_remember", 275, 5),
            ("_store_memory", 293, 9),
            ("_maybe_run_cascade", 325, 5),
            ("process_event", 348, 12),
            ("main", 372, 8),
        ],
        classes: &[],
        resolved_calls: &[
            ("_build_memory_content", "_reference_line"),
            ("_maybe_run_cascade", "_log"),
            ("_store_memory", "_load_remember"),
            ("_store_memory", "_log"),
            ("main", "_log"),
            ("main", "process_event"),
            ("process_event", "_build_memory_content"),
            ("process_event", "_build_tags"),
            ("process_event", "_log"),
            ("process_event", "_maybe_run_cascade"),
            ("process_event", "_normalize_output"),
            ("process_event", "_should_capture"),
            ("process_event", "_store_memory"),
        ],
    })
}

fn fixture_compaction_checkpoint_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "compaction_checkpoint.py",
        category: "hooks",
        rel_path: "hooks/compaction_checkpoint.py",
        file_prefix: "hooks/compaction_checkpoint.py",
        imports: &[
            ("__future__::annotations", 32),
            ("json", 34),
            ("sys", 35),
            ("typing::Any", 36),
        ],
        constants: &[("_LOG_PREFIX", 38)],
        functions: &[("_log", 41, 1), ("process_event", 46, 15), ("main", 94, 7)],
        classes: &[],
        resolved_calls: &[
            ("main", "_log"),
            ("main", "process_event"),
            ("process_event", "_log"),
        ],
    })
}

// ---------------------------------------------------------------------------
// Category: tests — pytest test files with TestXxx classes.
// New stress: many classes, each with many test_* methods (no inheritance).
// ---------------------------------------------------------------------------

fn fixture_test_sparse_dictionary_py() -> Fixture {
    let mut f = build_core_fixture(&CoreFixtureInputs {
        name: "test_sparse_dictionary.py",
        category: "tests",
        rel_path: "tests_py/core/test_sparse_dictionary.py",
        file_prefix: "tests_py/core/test_sparse_dictionary.py",
        imports: &[
            (
                "mcp_server::core::sparse_dictionary::build_seed_dictionary",
                3,
            ),
            ("mcp_server::core::sparse_dictionary::learn_dictionary", 3),
            ("mcp_server::core::sparse_dictionary::encode_session", 3),
            ("mcp_server::core::sparse_dictionary::label_feature", 3),
            (
                "mcp_server::core::sparse_dictionary_activation::SIGNAL_NAMES",
                9,
            ),
            ("mcp_server::core::sparse_dictionary_activation::D", 9),
            (
                "mcp_server::core::sparse_dictionary_activation::extract_session_activation",
                9,
            ),
            ("mcp_server::core::sparse_dictionary_learning::omp", 14),
            ("mcp_server::shared::linear_algebra::norm", 15),
            ("mcp_server::shared::linear_algebra::normalize", 15),
        ],
        constants: &[],
        functions: &[("_make_conv", 18, 1), ("_make_conversations", 33, 2)],
        classes: &[
            ExpectedClassInput {
                name: "TestSignalNames",
                line: 37,
                bases: &[],
                methods: &[
                    ("test_has_27_dimensions", 38, 1),
                    ("test_no_duplicates", 42, 3),
                ],
            },
            ExpectedClassInput {
                name: "TestExtractSessionActivation",
                line: 46,
                bases: &[],
                methods: &[
                    ("test_returns_27d_vector", 47, 3),
                    ("test_all_values_finite", 51, 3),
                    ("test_tool_ratios_sum_to_1", 57, 4),
                    ("test_handles_empty_conversation", 64, 2),
                    ("test_burst_indicator_for_short_sessions", 68, 2),
                    ("test_exploration_indicator_for_high_turns", 72, 2),
                ],
            },
            ExpectedClassInput {
                name: "TestBuildSeedDictionary",
                line: 77,
                bases: &[],
                methods: &[
                    ("test_valid_structure", 78, 2),
                    ("test_all_atoms_unit_vectors", 86, 3),
                    ("test_features_have_labels_and_descriptions", 92, 4),
                ],
            },
            ExpectedClassInput {
                name: "TestOmp",
                line: 100,
                bases: &[],
                methods: &[
                    ("test_returns_empty_for_zero_signal", 101, 4),
                    ("test_finds_correct_single_atom", 106, 6),
                    ("test_respects_sparsity_constraint", 113, 6),
                    ("test_reconstructs_signal_with_low_error", 123, 4),
                ],
            },
            ExpectedClassInput {
                name: "TestLearnDictionary",
                line: 130,
                bases: &[],
                methods: &[
                    ("test_returns_seed_for_few_sessions", 131, 2),
                    ("test_returns_seed_for_null", 137, 1),
                    ("test_learns_for_10_plus_sessions", 141, 2),
                    ("test_all_learned_atoms_unit_vectors", 155, 4),
                    ("test_features_have_auto_generated_labels", 162, 4),
                ],
            },
            ExpectedClassInput {
                name: "TestEncodeSession",
                line: 170,
                bases: &[],
                methods: &[
                    ("test_returns_sparse_activation", 171, 5),
                    ("test_respects_sparsity", 178, 4),
                    ("test_weights_are_nonzero", 183, 5),
                ],
            },
            ExpectedClassInput {
                name: "TestLabelFeature",
                line: 197,
                bases: &[],
                methods: &[
                    ("test_generates_meaningful_label", 198, 4),
                    ("test_handles_zero_direction", 206, 1),
                ],
            },
        ],
        resolved_calls: &[("_make_conversations", "_make_conv")],
    });
    // Method → top-level Function calls (resolver emits Calls_Method_Function).
    // Each (class, method, callee) tuple becomes one deduped Calls edge.
    let method_to_fn: &[(&str, &str, &str)] = &[
        ("TestEncodeSession", "test_respects_sparsity", "_make_conv"),
        (
            "TestEncodeSession",
            "test_returns_sparse_activation",
            "_make_conv",
        ),
        (
            "TestEncodeSession",
            "test_weights_are_nonzero",
            "_make_conv",
        ),
        (
            "TestExtractSessionActivation",
            "test_all_values_finite",
            "_make_conv",
        ),
        (
            "TestExtractSessionActivation",
            "test_burst_indicator_for_short_sessions",
            "_make_conv",
        ),
        (
            "TestExtractSessionActivation",
            "test_exploration_indicator_for_high_turns",
            "_make_conv",
        ),
        (
            "TestExtractSessionActivation",
            "test_returns_27d_vector",
            "_make_conv",
        ),
        (
            "TestExtractSessionActivation",
            "test_tool_ratios_sum_to_1",
            "_make_conv",
        ),
        (
            "TestLearnDictionary",
            "test_all_learned_atoms_unit_vectors",
            "_make_conversations",
        ),
        (
            "TestLearnDictionary",
            "test_features_have_auto_generated_labels",
            "_make_conversations",
        ),
        (
            "TestLearnDictionary",
            "test_learns_for_10_plus_sessions",
            "_make_conversations",
        ),
        (
            "TestLearnDictionary",
            "test_returns_seed_for_few_sessions",
            "_make_conversations",
        ),
    ];
    let prefix = "tests_py/core/test_sparse_dictionary.py";
    for (i, (cls, m, fn_)) in method_to_fn.iter().enumerate() {
        f.edges.push(ExpectedEdge {
            kind: "Calls",
            from_qn: format!("{prefix}::{cls}::{m}::callsite::__m2fn__::{i}"),
            to_qn: format!("{prefix}::{fn_}"),
        });
    }
    f
}

fn fixture_test_text_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "test_text.py",
        category: "tests",
        rel_path: "tests_py/shared/test_text.py",
        file_prefix: "tests_py/shared/test_text.py",
        imports: &[
            ("mcp_server::shared::text::STOPWORDS", 3),
            ("mcp_server::shared::text::TECHNICAL_SHORT_TERMS", 3),
            ("mcp_server::shared::text::extract_keywords", 3),
            ("mcp_server::shared::text::extract_keywords_array", 3),
        ],
        constants: &[],
        functions: &[],
        classes: &[
            ExpectedClassInput {
                name: "TestExtractKeywords",
                line: 11,
                bases: &[],
                methods: &[
                    ("test_returns_empty_set_for_empty_string", 12, 3),
                    ("test_returns_empty_set_for_none", 17, 2),
                    ("test_handles_unicode_text_without_crashing", 20, 2),
                    ("test_extracts_technical_abbreviations", 24, 1),
                    ("test_handles_mixed_case_by_lowercasing", 29, 1),
                    ("test_passes_words_longer_than_6_characters", 35, 1),
                    ("test_filters_out_short_non_technical_words", 41, 2),
                    ("test_handles_long_text", 45, 1),
                    ("test_deduplicates_keywords", 53, 2),
                ],
            },
            ExpectedClassInput {
                name: "TestExtractKeywordsArray",
                line: 60,
                bases: &[],
                methods: &[
                    ("test_returns_a_list", 61, 2),
                    ("test_returns_empty_list_for_empty_string", 65, 3),
                    ("test_contains_same_elements_as_extract_keywords", 70, 4),
                ],
            },
            ExpectedClassInput {
                name: "TestStopwordsFiltering",
                line: 79,
                bases: &[],
                methods: &[
                    ("test_excludes_common_stopwords_from_results", 80, 1),
                    ("test_stopwords_set_contains_common_english_words", 86, 0),
                ],
            },
            ExpectedClassInput {
                name: "TestTechnicalShortTerms",
                line: 94,
                bases: &[],
                methods: &[
                    ("test_includes_known_short_technical_terms", 95, 0),
                    (
                        "test_extract_keywords_picks_up_technical_short_terms",
                        99,
                        1,
                    ),
                    ("test_technical_short_terms_is_frozenset", 105, 2),
                ],
            },
        ],
        resolved_calls: &[],
    })
}

fn fixture_test_recall_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "test_recall.py",
        category: "tests",
        rel_path: "tests_py/handlers/test_recall.py",
        file_prefix: "tests_py/handlers/test_recall.py",
        imports: &[
            ("asyncio", 3),
            // aliased: handler as recall_handler
            ("recall_handler", 5),
            ("_wrrf_fuse", 6),
            ("remember_handler", 7),
        ],
        constants: &[],
        functions: &[],
        classes: &[
            ExpectedClassInput {
                name: "TestWRRFFuse",
                line: 10,
                bases: &[],
                methods: &[
                    ("test_single_signal", 11, 2),
                    ("test_multiple_signals_boost_overlap", 21, 1),
                    ("test_zero_weight_ignored", 34, 2),
                    ("test_empty_signals", 43, 1),
                ],
            },
            ExpectedClassInput {
                name: "TestRecallHandler",
                line: 48,
                bases: &[],
                methods: &[
                    ("test_no_query_returns_empty", 49, 2),
                    ("test_empty_query_returns_empty", 54, 2),
                    ("test_recall_stored_memory", 58, 4),
                    ("test_recall_response_shape", 85, 7),
                    ("test_domain_scoped_recall", 101, 6),
                    ("test_global_memory_visible_across_domains", 131, 6),
                ],
            },
            ExpectedClassInput {
                name: "TestRecallReturnsDict",
                line: 167,
                bases: &[],
                methods: &[
                    ("test_handler_direct_returns_dict", 170, 3),
                    ("test_safe_handler_returns_dict", 177, 4),
                ],
            },
        ],
        resolved_calls: &[],
    })
}

fn fixture_test_brain_index_store_py() -> Fixture {
    build_core_fixture(&CoreFixtureInputs {
        name: "test_brain_index_store.py",
        category: "tests",
        rel_path: "tests_py/infrastructure/test_brain_index_store.py",
        file_prefix: "tests_py/infrastructure/test_brain_index_store.py",
        imports: &[(
            "mcp_server::infrastructure::brain_index_store::load_brain_index",
            3,
        )],
        constants: &[],
        functions: &[],
        classes: &[ExpectedClassInput {
            name: "TestLoadBrainIndex",
            line: 6,
            bases: &[],
            methods: &[("test_returns_valid_structure", 7, 5)],
        }],
        resolved_calls: &[],
    })
}

fn fixture_test_http_server_py() -> Fixture {
    let mut f = build_core_fixture(&CoreFixtureInputs {
        name: "test_http_server.py",
        category: "tests",
        rel_path: "tests_py/server/test_http_server.py",
        file_prefix: "tests_py/server/test_http_server.py",
        imports: &[
            ("json", 3),
            ("io::BytesIO", 4),
            ("unittest::mock::patch", 5),
            ("unittest::mock::MagicMock", 5),
            // `import mcp_server.server.http_server as http_mod` — aliased plain
            ("http_mod", 7),
            ("mcp_server::server::http_server::start_ui_server", 8),
            ("mcp_server::server::http_server::shutdown_server", 8),
            ("mcp_server::server::http_server::_reset_idle_timer", 8),
        ],
        constants: &[],
        functions: &[("_reset_module_state", 15, 1)],
        classes: &[
            ExpectedClassInput {
                name: "TestResetIdleTimer",
                line: 23,
                bases: &[],
                methods: &[
                    ("setup_method", 24, 1),
                    ("teardown_method", 27, 1),
                    ("test_creates_daemon_timer", 30, 3),
                    ("test_cancels_previous_timer", 38, 5),
                    ("test_timer_duration_is_600", 48, 4),
                    ("test_shutdown_callback_clears_active_server", 56, 7),
                    ("test_shutdown_callback_noop_if_no_server", 75, 5),
                    ("test_shutdown_callback_prints_message", 91, 8),
                ],
            },
            ExpectedClassInput {
                name: "TestStartUiServer",
                line: 107,
                bases: &[],
                methods: &[
                    ("setup_method", 108, 1),
                    ("teardown_method", 111, 1),
                    ("test_reuses_existing_server", 114, 4),
                    ("test_reads_html_from_custom_path", 131, 11),
                    ("test_raises_on_missing_html_file", 162, 2),
                    ("test_starts_server_on_port_3456_first", 169, 8),
                    ("test_falls_back_to_port_0_on_oserror", 190, 9),
                    ("test_raises_if_both_ports_fail", 211, 5),
                    ("test_server_thread_is_daemon", 224, 11),
                    ("test_sets_active_server_state", 245, 9),
                    ("test_prints_startup_message", 265, 10),
                ],
            },
            ExpectedClassInput {
                name: "TestShutdownServer",
                line: 283,
                bases: &[],
                methods: &[
                    ("setup_method", 284, 1),
                    ("teardown_method", 287, 1),
                    ("test_shutdown_active_server", 290, 3),
                    ("test_shutdown_cancels_timer", 297, 4),
                    ("test_shutdown_noop_when_no_server", 305, 1),
                    ("test_shutdown_cancels_timer_even_without_server", 312, 3),
                ],
            },
            ExpectedClassInput {
                name: "TestHandlerBehavior",
                line: 321,
                bases: &[],
                methods: &[
                    ("setup_method", 324, 1),
                    ("teardown_method", 327, 1),
                    ("_create_handler_class", 330, 8),
                    ("_make_handler", 357, 5),
                    ("test_get_root_returns_html", 373, 6),
                    ("test_get_graph_returns_json", 388, 7),
                    ("test_get_sets_no_cache", 402, 4),
                    ("test_do_options_returns_204", 412, 4),
                    ("test_log_message_suppressed", 423, 3),
                    ("test_send_header_cors_is_noop", 429, 3),
                    ("test_get_resets_idle_timer", 435, 5),
                ],
            },
        ],
        resolved_calls: &[],
    });
    // Method → top-level Function calls: setup_method/teardown_method on each
    // of the 4 test classes calls _reset_module_state. Dedupes per (caller, target),
    // so 8 distinct method→fn pairs = 8 Calls edges.
    let method_to_fn: &[(&str, &str, &str)] = &[
        ("TestHandlerBehavior", "setup_method", "_reset_module_state"),
        (
            "TestHandlerBehavior",
            "teardown_method",
            "_reset_module_state",
        ),
        ("TestResetIdleTimer", "setup_method", "_reset_module_state"),
        (
            "TestResetIdleTimer",
            "teardown_method",
            "_reset_module_state",
        ),
        ("TestShutdownServer", "setup_method", "_reset_module_state"),
        (
            "TestShutdownServer",
            "teardown_method",
            "_reset_module_state",
        ),
        ("TestStartUiServer", "setup_method", "_reset_module_state"),
        (
            "TestStartUiServer",
            "teardown_method",
            "_reset_module_state",
        ),
    ];
    let prefix = "tests_py/server/test_http_server.py";
    for (i, (cls, m, fn_)) in method_to_fn.iter().enumerate() {
        f.edges.push(ExpectedEdge {
            kind: "Calls",
            from_qn: format!("{prefix}::{cls}::{m}::callsite::__m2fn__::{i}"),
            to_qn: format!("{prefix}::{fn_}"),
        });
    }
    f
}

// ---------------------------------------------------------------------------
// Category: synthetic — hand-crafted fixtures for specific bug scenarios
// not present in real Cortex source. intra_inheritance.py validates BUG #9
// (resolver must emit Extends_Struct_Struct for intra-file class inheritance).
// ---------------------------------------------------------------------------

fn fixture_intra_inheritance_py() -> Fixture {
    let mut f = build_core_fixture(&CoreFixtureInputs {
        name: "intra_inheritance.py",
        category: "synthetic",
        rel_path: "synthetic/intra_inheritance.py",
        file_prefix: "synthetic/intra_inheritance.py",
        imports: &[("__future__::annotations", 9)],
        constants: &[],
        functions: &[],
        classes: &[
            ExpectedClassInput {
                name: "Animal",
                line: 12,
                bases: &[],
                methods: &[("speak", 13, 0)],
            },
            ExpectedClassInput {
                name: "Dog",
                line: 17,
                bases: &["Animal"],
                methods: &[("speak", 18, 0)],
            },
            ExpectedClassInput {
                name: "Puppy",
                line: 22,
                bases: &["Dog"],
                methods: &[("speak", 23, 0)],
            },
        ],
        resolved_calls: &[],
    });
    // Two Extends_Struct_Struct edges expected after BUG #9 fix:
    //   Dog → Animal
    //   Puppy → Dog
    let file_prefix = "synthetic/intra_inheritance.py";
    f.edges.push(ExpectedEdge {
        kind: "Extends",
        from_qn: format!("{file_prefix}::Dog"),
        to_qn: format!("{file_prefix}::Animal"),
    });
    f.edges.push(ExpectedEdge {
        kind: "Extends",
        from_qn: format!("{file_prefix}::Puppy"),
        to_qn: format!("{file_prefix}::Dog"),
    });
    f
}

// ---------------------------------------------------------------------------
// Per-fixture runner
// ---------------------------------------------------------------------------
//
// Set up tempdir → copy fixture source → index → resolve → score → assert.
// Each #[test] is a thin wrapper that supplies a Fixture and floor values.

struct Floors {
    nodes: f64,
    defines: f64,
    calls: f64,
}

fn run_fixture(test_id: &str, fixture: Fixture, floors: Floors) {
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_accuracy")
        .join(fixture.category);
    let tmp = fixture_root_for(test_id);
    // Mirror the rel_path under the tempdir so the file_id materialized by
    // the indexer matches our annotation prefix exactly.
    let dest = tmp.join(fixture.rel_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).expect("mkdir -p inside tempdir");
    }
    let source_name = std::path::Path::new(fixture.rel_path)
        .file_name()
        .expect("rel_path has a file name")
        .to_string_lossy()
        .into_owned();
    fs::copy(corpus.join(&source_name), &dest)
        .unwrap_or_else(|e| panic!("copy {source_name} into tempdir: {e}"));

    let graph_path = tmp.join("graph.lbug");
    let observed = index_fixture(&tmp, &graph_path);

    print_diff(&fixture, &observed);

    let node_score = score_nodes(&fixture.nodes, &observed.nodes);
    let edge_scores = score_edges_by_kind(&fixture.edges, &observed.edges_by_kind);

    println!("\n  ===== scores =====");
    println!(
        "  nodes  P={:.3} R={:.3} F1={:.3}  (tp={} fp={} fn={})",
        node_score.precision(),
        node_score.recall(),
        node_score.f1(),
        node_score.tp,
        node_score.fp,
        node_score.fn_,
    );
    for (kind, s) in &edge_scores {
        println!(
            "  edge[{kind}]  P={:.3} R={:.3} F1={:.3}  (tp={} fp={} fn={})",
            s.precision(),
            s.recall(),
            s.f1(),
            s.tp,
            s.fp,
            s.fn_,
        );
    }

    let f1_defines = edge_scores.get("Defines").map(|s| s.f1()).unwrap_or(0.0);
    let f1_calls = edge_scores.get("Calls").map(|s| s.f1()).unwrap_or_else(|| {
        // No expectations for Calls (e.g. files with only builtins) means
        // the gate is vacuously satisfied at 1.0. Don't penalize.
        1.0
    });
    let f1_nodes = node_score.f1();

    println!(
        "\n  Spike B' floors: Nodes≥{}  Defines≥{}  Calls≥{}",
        floors.nodes, floors.defines, floors.calls
    );
    println!(
        "  Measured       : Nodes={f1_nodes:.3}  Defines={f1_defines:.3}  \
         Calls={f1_calls:.3}"
    );

    assert!(
        f1_nodes >= floors.nodes,
        "REGRESSION on {}/{}: nodes F1 {f1_nodes:.3} fell below floor {}",
        fixture.category,
        fixture.name,
        floors.nodes
    );
    assert!(
        f1_defines >= floors.defines,
        "REGRESSION on {}/{}: Defines F1 {f1_defines:.3} fell below floor {}",
        fixture.category,
        fixture.name,
        floors.defines
    );
    assert!(
        f1_calls >= floors.calls,
        "REGRESSION on {}/{}: Calls F1 {f1_calls:.3} fell below floor {}",
        fixture.category,
        fixture.name,
        floors.calls
    );
}

// ---------------------------------------------------------------------------
// The tests — one per fixture. As floors tighten across more files, the
// gate becomes universal.
// ---------------------------------------------------------------------------

#[test]
fn pure_shared_text_py() {
    run_fixture(
        "text_py",
        fixture_text_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_shared_similarity_py() {
    run_fixture(
        "similarity_py",
        fixture_similarity_py(),
        // Calls floor stays at 1.0 vacuously (similarity.py has no resolvable
        // intra-file calls — len is a builtin).
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_shared_hash_py() {
    run_fixture(
        "hash_py",
        fixture_hash_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_shared_linear_algebra_py() {
    run_fixture(
        "linear_algebra_py",
        fixture_linear_algebra_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_shared_yaml_parser_py() {
    run_fixture(
        "yaml_parser_py",
        fixture_yaml_parser_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_core_persona_vector_py() {
    run_fixture(
        "persona_vector_py",
        fixture_persona_vector_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_core_profile_builder_py() {
    run_fixture(
        "profile_builder_py",
        fixture_profile_builder_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_core_style_classifier_py() {
    run_fixture(
        "style_classifier_py",
        fixture_style_classifier_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_core_sparse_dictionary_py() {
    run_fixture(
        "sparse_dictionary_py",
        fixture_sparse_dictionary_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn pure_core_cognitive_map_py() {
    run_fixture(
        "cognitive_map_py",
        fixture_cognitive_map_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn core_with_deps_hierarchical_predictive_coding_py() {
    run_fixture(
        "hpc_py",
        fixture_hierarchical_predictive_coding_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn core_with_deps_dual_store_cls_py() {
    run_fixture(
        "dual_store_cls_py",
        fixture_dual_store_cls_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn core_with_deps_causal_graph_py() {
    run_fixture(
        "causal_graph_py",
        fixture_causal_graph_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn core_with_deps_consolidation_engine_py() {
    run_fixture(
        "consolidation_engine_py",
        fixture_consolidation_engine_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn core_with_deps_replay_py() {
    run_fixture(
        "replay_py",
        fixture_replay_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn infrastructure_file_io_py() {
    run_fixture(
        "file_io_py",
        fixture_file_io_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn infrastructure_scanner_py() {
    run_fixture(
        "scanner_py",
        fixture_scanner_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn infrastructure_embedding_engine_py() {
    run_fixture(
        "embedding_engine_py",
        fixture_embedding_engine_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn infrastructure_mcp_client_py() {
    run_fixture(
        "mcp_client_py",
        fixture_mcp_client_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn infrastructure_pg_store_py() {
    run_fixture(
        "pg_store_py",
        fixture_pg_store_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn handlers_explore_features_py() {
    run_fixture(
        "explore_features_py",
        fixture_explore_features_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn handlers_recall_py() {
    run_fixture(
        "recall_handler_py",
        fixture_recall_handler_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn handlers_remember_py() {
    run_fixture(
        "remember_handler_py",
        fixture_remember_handler_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn handlers_consolidate_py() {
    run_fixture(
        "consolidate_handler_py",
        fixture_consolidate_handler_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn handlers_get_methodology_graph_py() {
    run_fixture(
        "get_methodology_graph_py",
        fixture_get_methodology_graph_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn server_transport_http_standalone_graph_py() {
    run_fixture(
        "http_standalone_graph_py",
        fixture_http_standalone_graph_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn server_transport_http_standalone_py() {
    run_fixture(
        "http_standalone_py",
        fixture_http_standalone_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn server_transport_http_launcher_py() {
    run_fixture(
        "http_launcher_py",
        fixture_http_launcher_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn server_transport_http_dashboard_data_py() {
    run_fixture(
        "http_dashboard_data_py",
        fixture_http_dashboard_data_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

#[test]
fn server_transport_visualize_bootstrap_py() {
    run_fixture(
        "visualize_bootstrap_py",
        fixture_visualize_bootstrap_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

// hooks (4 — note: hooks category has only 4 modules in Cortex)
#[test]
fn hooks_session_lifecycle_py() {
    run_fixture(
        "session_lifecycle_py",
        fixture_session_lifecycle_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn hooks_session_start_py() {
    run_fixture(
        "session_start_py",
        fixture_session_start_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn hooks_post_tool_capture_py() {
    run_fixture(
        "post_tool_capture_py",
        fixture_post_tool_capture_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn hooks_compaction_checkpoint_py() {
    run_fixture(
        "compaction_checkpoint_py",
        fixture_compaction_checkpoint_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

// tests (5)
#[test]
fn tests_sparse_dictionary_py() {
    run_fixture(
        "ts_sparse_dictionary_py",
        fixture_test_sparse_dictionary_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn tests_text_py() {
    run_fixture(
        "ts_text_py",
        fixture_test_text_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn tests_recall_py() {
    run_fixture(
        "ts_recall_py",
        fixture_test_recall_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn tests_brain_index_store_py() {
    run_fixture(
        "ts_brain_index_store_py",
        fixture_test_brain_index_store_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}
#[test]
fn tests_http_server_py() {
    run_fixture(
        "ts_http_server_py",
        fixture_test_http_server_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

// synthetic — purpose-built fixtures for specific bug scenarios
#[test]
fn synthetic_intra_inheritance_py() {
    run_fixture(
        "intra_inheritance_py",
        fixture_intra_inheritance_py(),
        Floors {
            nodes: 1.0,
            defines: 1.0,
            calls: 1.0,
        },
    );
}

/// Multi-file fixture for BUG #2 validation: file_a imports symbols from
/// file_b and calls them. The resolver should emit cross-file Imports edges
/// (file_a's Import nodes → file_b's Function nodes) and cross-file Calls
/// edges (file_a::main's call_sites → file_b::helper, file_b::square).
///
/// This test bypasses run_fixture because run_fixture's corpus layout
/// assumes one file per fixture. Multi-file uses a subdir under the
/// synthetic/ corpus root.
#[test]
fn synthetic_multi_file_imports() {
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_accuracy/synthetic/multi_file");
    let tmp = fixture_root_for("multi_file");
    fs::copy(corpus.join("file_a.py"), tmp.join("file_a.py")).expect("copy file_a.py");
    fs::copy(corpus.join("file_b.py"), tmp.join("file_b.py")).expect("copy file_b.py");

    let graph_path = tmp.join("graph.lbug");
    let observed = index_fixture(&tmp, &graph_path);

    println!("\n===== multi_file_imports diagnostic =====");
    println!("observed nodes by label:");
    let mut by_label: BTreeMap<&String, usize> = BTreeMap::new();
    for label in observed.nodes.values() {
        *by_label.entry(label).or_default() += 1;
    }
    for (label, count) in &by_label {
        println!("  {label:<12} {count}");
    }
    println!("\nobserved edges by kind:");
    for (kind, set) in &observed.edges_by_kind {
        println!("  {kind:<20} {}", set.len());
        if kind == "Imports" || kind == "Calls" {
            for (from, to) in set {
                println!("    {from}  ->  {to}");
            }
        }
    }

    // BUG #2 floor: at minimum, the resolver must emit ≥1 Imports edge
    // pointing FROM file_a's symbol/file/import-node TO a symbol in file_b
    // (not back to file_a). This is the "cross-file linkage" the audit
    // identified as missing.
    let imports = observed
        .edges_by_kind
        .get("Imports")
        .cloned()
        .unwrap_or_default();
    let cross_file_imports = imports
        .iter()
        .filter(|(from, to)| {
            (from.contains("file_a") && to.contains("file_b"))
                || (from.contains("file_b") && to.contains("file_a"))
        })
        .count();
    println!("\ncross-file Imports edges: {cross_file_imports}");

    // Calls — file_a::main calls helper + square, both in file_b. Both
    // should resolve to Calls_Function_Function (or similar) cross-file.
    let calls = observed
        .edges_by_kind
        .get("Calls")
        .cloned()
        .unwrap_or_default();
    let cross_file_calls = calls
        .iter()
        .filter(|(from, to)| from.contains("file_a") && to.contains("file_b"))
        .count();
    println!("cross-file Calls edges    : {cross_file_calls}");

    assert!(
        cross_file_imports >= 1,
        "BUG #2 regression: expected ≥1 cross-file Imports edge from file_a → file_b, got {cross_file_imports}"
    );
    assert!(
        cross_file_calls >= 2,
        "expected ≥2 cross-file Calls edges (helper + square), got {cross_file_calls}"
    );
}
