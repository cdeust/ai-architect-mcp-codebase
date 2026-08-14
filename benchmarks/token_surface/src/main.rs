// token_surface_bench — falsifiable token-efficiency benchmark (issue #56).
//
// Runs a FIXED 5-query structural workload against a deterministic fixture repo,
// driving the REAL library (indexer → resolver → clustering → search) and the
// REAL `token_surface` renderer the MCP tools use — so the shapes measured here
// are the shapes the tools emit, not a re-implementation. For each query it
// serializes the tool response three ways and counts characters (the token
// proxy; ~4 chars ≈ 1 token, reported alongside):
//   * current JSON  — array of objects (today's default),
//   * tabular       — columns declared once + rows as arrays,
//   * ids           — bare identifiers.
//
// Two comparisons are reported:
//   (a) ids/tabular vs current JSON on the SAME queries — the issue's ≥30%
//       reduction criterion.
//   (b) the compact graph-tool total vs a SCRIPTED grep/read transcript for the
//       same questions — the CBM-style "graph vs files" marketing ratio. (b) is a
//       simulation of what a file-exploring agent would consume, NOT a live agent.
//
// Writes benchmarks/token_surface/results.json (reproducible: the fixture is
// generated in-code) and prints a summary.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, IndexOptions};
use ai_architect_mcp::token_surface::{render_list, Detail, Format};
use ai_architect_mcp::{clustering, resolver, search};
use serde_json::{json, Map, Value};
use std::path::Path;

/// One query's three measured encodings.
struct Measured {
    label: String,
    question: String,
    json_chars: usize,
    tabular_chars: usize,
    ids_chars: usize,
    row_count: usize,
    /// Fixture files a grep/read agent would open to answer this question.
    grep_files: Vec<String>,
    grep_keyword: String,
}

fn main() {
    let tmp = tempfile::Builder::new()
        .prefix("token_surface_bench_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    let (store, graph) = index_fixture(tmp.path(), &repo);
    let measured = run_workload(&store, &graph);
    let totals = Totals::of(&measured);
    // ---- (b) graph tools vs scripted grep/read transcript -----------------
    let files_chars = simulate_grep_read(&repo, &measured);
    let results = build_results(&measured, &totals, files_chars);
    write_and_print(&results, &totals, files_chars);
}

/// Indexes, resolves and clusters the fixture repo; returns the opened store
/// and the graph path.
fn index_fixture(root: &Path, repo: &Path) -> (GraphStore, std::path::PathBuf) {
    let out = root.join("out");
    std::fs::create_dir_all(&out).expect("mk out");
    let graph = out.join("graph");
    indexer::index_codebase_with_language(repo, &graph, &IndexOptions::default()).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    // cluster_graph runs community detection AND process tracing (it calls
    // trace_processes internally), so get_processes has data afterward.
    clustering::cluster_graph(&store, 1.0).expect("cluster");
    (store, graph)
}

/// The fixed 5-query structural workload.
fn run_workload(store: &GraphStore, graph: &Path) -> Vec<Measured> {
    vec![
        measure_search(
            store,
            graph,
            "process",
            "Q1: find functions matching 'process'",
        ),
        measure_query_functions(store, "Q2: list every function's qualified name"),
        measure_processes(store, "Q3: list all execution processes"),
        measure_impact(
            store,
            "core::core_process",
            "Q4: who depends on core_process?",
        ),
        measure_search(store, graph, "config", "Q5: find symbols matching 'config'"),
    ]
}

/// Comparison (a) aggregates: ids/tabular vs current JSON. Tabular is the
/// apples-to-apples "same information" comparison; ids is the cheapest sweep.
struct Totals {
    json: usize,
    tabular: usize,
    ids: usize,
    tabular_reduction: f64,
    ids_reduction: f64,
}

impl Totals {
    fn of(measured: &[Measured]) -> Totals {
        let json: usize = measured.iter().map(|m| m.json_chars).sum();
        let tabular: usize = measured.iter().map(|m| m.tabular_chars).sum();
        let ids: usize = measured.iter().map(|m| m.ids_chars).sum();
        Totals {
            json,
            tabular,
            ids,
            tabular_reduction: pct_reduction(json, tabular),
            ids_reduction: pct_reduction(json, ids),
        }
    }
}

/// Assembles the results.json document from the per-query measurements and
/// the comparison aggregates.
fn build_results(measured: &[Measured], t: &Totals, files_chars: usize) -> Value {
    let per_query: Vec<Value> = measured
        .iter()
        .map(|m| {
            json!({
                "label": m.label,
                "question": m.question,
                "rows": m.row_count,
                "json_chars": m.json_chars,
                "tabular_chars": m.tabular_chars,
                "ids_chars": m.ids_chars,
                "tabular_reduction_pct": pct_reduction(m.json_chars, m.tabular_chars),
                "ids_reduction_pct": pct_reduction(m.json_chars, m.ids_chars),
            })
        })
        .collect();
    let files_ratio = files_chars as f64 / t.tabular.max(1) as f64;
    json!({
        "workload": "fixed 5-query structural workload on a generated fixture repo",
        "token_proxy": "characters of compact JSON serialization (~4 chars per token)",
        "comparison_a_ids_tabular_vs_json": {
            "json_total_chars": t.json,
            "tabular_total_chars": t.tabular,
            "ids_total_chars": t.ids,
            "tabular_reduction_pct": t.tabular_reduction,
            "ids_reduction_pct": t.ids_reduction,
            "meets_30pct_criterion": t.tabular_reduction >= 30.0 || t.ids_reduction >= 30.0,
        },
        "comparison_b_graph_vs_files": {
            "graph_tabular_total_chars": t.tabular,
            "grep_read_transcript_chars": files_chars,
            "files_to_graph_ratio": round2(files_ratio),
            "note": "comparison (b) is a SCRIPTED simulation of a grep/read transcript, NOT a live agent run",
        },
        "per_query": per_query,
    })
}

/// Writes results.json next to this crate's Cargo.toml and prints the
/// human-readable summary.
fn write_and_print(results: &Value, t: &Totals, files_chars: usize) {
    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("results.json");
    let pretty = serde_json::to_string_pretty(results).expect("encode results");
    std::fs::write(&out_path, &pretty).expect("write results.json");

    let (json_total, tabular_total, ids_total) = (t.json, t.tabular, t.ids);
    let (tabular_reduction, ids_reduction) = (t.tabular_reduction, t.ids_reduction);
    let files_ratio = files_chars as f64 / t.tabular.max(1) as f64;
    println!("== token_surface_bench ==");
    println!(
        "(a) ids/tabular vs current JSON over 5 queries:\n    json={json_total}  tabular={tabular_total} ({tabular_reduction:.1}% less)  ids={ids_total} ({ids_reduction:.1}% less)"
    );
    println!(
        "(b) graph (tabular) vs scripted grep/read transcript:\n    graph={}  files={files_chars}  files/graph = {:.1}x",
        tabular_total, files_ratio
    );
    println!("wrote {}", out_path.display());
    println!(
        "meets >=30% criterion (a): {}",
        tabular_reduction >= 30.0 || ids_reduction >= 30.0
    );
}

/// Percentage reduction of `compact` relative to `base` (positive = smaller).
fn pct_reduction(base: usize, compact: usize) -> f64 {
    if base == 0 {
        return 0.0;
    }
    round2((base as f64 - compact as f64) / base as f64 * 100.0)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Inputs to `measure` — bundled into a struct to keep the argument count sane.
struct MeasureInput<'a> {
    /// Tool name (also drives the response envelope).
    tool: &'a str,
    /// The full human question (its `Qn` prefix becomes the label).
    question: &'a str,
    /// Response key the list lives under (e.g. "results"/"rows"/"processes").
    key: &'a str,
    items: &'a [Value],
    columns: &'a [&'a str],
    id_field: &'a str,
    /// query_graph's redundant text table (present only in the JSON default).
    human_result: Option<String>,
    /// The keyword a grep-based agent would search for this question.
    grep_keyword: &'a str,
    /// Fixture files a grep/read agent would open to answer this question.
    grep_files: Vec<String>,
}

/// Serializes the response three ways (json / tabular / ids) with an identical
/// envelope and counts characters for each.
fn measure(input: MeasureInput) -> Measured {
    let mut envelope: Map<String, Value> = Map::new();
    envelope.insert("stage".into(), json!(3));
    envelope.insert("status".into(), json!("ok"));
    envelope.insert("tool".into(), json!(input.tool));
    envelope.insert("total_count".into(), json!(input.items.len()));
    envelope.insert("offset".into(), json!(0));
    envelope.insert("truncated".into(), json!(false));

    let build = |list: Value, cols: Option<Value>, result: Option<&String>| -> usize {
        let mut obj = envelope.clone();
        obj.insert(input.key.to_string(), list);
        if let Some(c) = cols {
            obj.insert("columns".to_string(), c);
        }
        if let Some(r) = result {
            obj.insert("result".to_string(), Value::String(r.clone()));
        }
        serde_json::to_string(&Value::Object(obj))
            .map(|s| s.len())
            .unwrap_or(0)
    };

    let json_v = render_list(
        input.items,
        input.columns,
        input.id_field,
        &Detail::Full,
        &Format::Json,
    );
    let tab_v = render_list(
        input.items,
        input.columns,
        input.id_field,
        &Detail::Full,
        &Format::Tabular,
    );
    let ids_v = render_list(
        input.items,
        input.columns,
        input.id_field,
        &Detail::Ids,
        &Format::Json,
    );

    Measured {
        label: input.question.split(':').next().unwrap_or("Q").to_string(),
        question: input.question.to_string(),
        json_chars: build(json_v.value, None, input.human_result.as_ref()),
        tabular_chars: build(tab_v.value, tab_v.columns, None),
        ids_chars: build(ids_v.value, None, None),
        row_count: input.items.len(),
        grep_files: input.grep_files,
        grep_keyword: input.grep_keyword.to_string(),
    }
}

fn measure_search(store: &GraphStore, graph: &Path, query: &str, question: &str) -> Measured {
    let index_dir = search::resolve_search_index_dir(graph);
    let opts = search::SearchOptions {
        limit: 50,
        label_filter: None,
        min_score: 0.01,
    };
    let hits = search::search_graph(store, query, &opts, index_dir.as_deref()).unwrap_or_default();
    let items: Vec<Value> = hits
        .iter()
        .map(|r| {
            json!({
                "qualified_name": r.qualified_name,
                "name": r.name,
                "kind": r.label,
                "file_path": r.file_path,
                "score": format!("{:.4}", r.score),
                "start_line": r.start_line,
                "end_line": r.end_line,
                "community_id": r.community_id,
            })
        })
        .collect();
    let columns = [
        "qualified_name",
        "name",
        "kind",
        "file_path",
        "score",
        "start_line",
        "end_line",
        "community_id",
    ];
    let files: Vec<String> = hits
        .iter()
        .map(|r| r.file_path.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    measure(MeasureInput {
        tool: "search_codebase",
        question,
        key: "results",
        items: &items,
        columns: &columns,
        id_field: "qualified_name",
        human_result: None,
        grep_keyword: query,
        grep_files: files,
    })
}

fn measure_query_functions(store: &GraphStore, question: &str) -> Measured {
    // Mirrors query_graph: RETURN rows as arrays + a human-readable `result`.
    let qr = store
        .execute_query("MATCH (f:Function) RETURN f.qualified_name, f.name")
        .expect("query");
    let items: Vec<Value> = qr
        .rows
        .iter()
        .map(|row| json!({ "qualified_name": row[0], "name": row.get(1).cloned().unwrap_or_default() }))
        .collect();
    let columns = ["qualified_name", "name"];
    // The redundant human table query_graph emits by default.
    let mut human = String::from("qualified_name | name\n");
    for row in &qr.rows {
        human.push_str(&format!(
            "{} | {}\n",
            row[0],
            row.get(1).cloned().unwrap_or_default()
        ));
    }
    measure(MeasureInput {
        tool: "query_graph",
        question,
        key: "rows",
        items: &items,
        columns: &columns,
        id_field: "qualified_name",
        human_result: Some(human),
        grep_keyword: "def ",
        grep_files: vec![],
    })
}

fn measure_processes(store: &GraphStore, question: &str) -> Measured {
    let procs = clustering::get_processes(store).unwrap_or_default();
    let items: Vec<Value> = procs
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
    let columns = ["name", "entry_point", "entry_kind", "depth", "node_count"];
    measure(MeasureInput {
        tool: "get_processes",
        question,
        key: "processes",
        items: &items,
        columns: &columns,
        id_field: "name",
        human_result: None,
        grep_keyword: "def main",
        grep_files: vec![],
    })
}

fn measure_impact(store: &GraphStore, qn: &str, question: &str) -> Measured {
    let impact = clustering::get_impact(store, qn).unwrap_or_else(|_| panic!("impact {qn}"));
    let items: Vec<Value> = impact
        .callers
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "qualified_name": n.qualified_name,
                "label": n.label,
                "confidence": format!("{:.2}", n.confidence),
            })
        })
        .collect();
    let columns = ["qualified_name", "label", "confidence", "id"];
    let files: Vec<String> = impact
        .callers
        .iter()
        .filter_map(|n| n.qualified_name.split("::").next().map(String::from))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    measure(MeasureInput {
        tool: "get_impact",
        question,
        key: "callers",
        items: &items,
        columns: &columns,
        id_field: "qualified_name",
        human_result: None,
        grep_keyword: "core_process",
        grep_files: files,
    })
}

/// Scripted grep/read transcript char count (comparison b). For each question a
/// file-exploring agent would: run grep for the keyword (transcript = matching
/// `path:lineno:line` lines) AND open the files that contain the answer (full
/// contents). We sum both across the 5 questions. This is a SIMULATION of the
/// consumed context, not a live agent — stated honestly in the MANIFEST.
fn simulate_grep_read(repo: &Path, measured: &[Measured]) -> usize {
    let mut total = 0usize;
    let mut read_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in measured {
        // grep output: every matching line across the fixture, as an agent's
        // `rg keyword` would print `path:lineno:line`.
        total += grep_output_chars(repo, &m.grep_keyword);
        // read: the files the answer lives in (deduped across questions — an
        // agent would not re-read a file it already has in context).
        for f in &m.grep_files {
            read_files.insert(f.clone());
        }
    }
    for rel in &read_files {
        if let Ok(bytes) = std::fs::read_to_string(repo.join(rel)) {
            total += bytes.len();
        }
    }
    total
}

/// Chars of a `rg <keyword>`-style transcript over the fixture.
fn grep_output_chars(repo: &Path, keyword: &str) -> usize {
    let mut chars = 0usize;
    visit_files(repo, &mut |rel, content| {
        for (i, line) in content.lines().enumerate() {
            if line.contains(keyword) {
                chars += format!("{rel}:{}:{line}\n", i + 1).len();
            }
        }
    });
    chars
}

fn visit_files(root: &Path, f: &mut impl FnMut(&str, &str)) {
    fn walk(base: &Path, dir: &Path, f: &mut impl FnMut(&str, &str)) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, f);
                } else if let Ok(content) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(base).unwrap_or(&path).to_string_lossy();
                    f(&rel, &content);
                }
            }
        }
    }
    walk(root, root, f);
}

/// Generates a deterministic fixture: `core.py` defines `core_process` and
/// `Config`; a dozen module files call `core_process` and reference `Config`, so
/// resolution yields caller edges and search has 'process'/'config' hits. Kept
/// small so the whole benchmark runs in a couple of seconds.
fn write_fixture(repo: &Path) {
    std::fs::create_dir_all(repo).expect("mk repo");
    std::fs::write(
        repo.join("core.py"),
        "def core_process(x):\n    return x + 1\n\n\
         class Config:\n    def __init__(self):\n        self.retries = 3\n\n\
         def load_config():\n    return Config()\n",
    )
    .expect("core");
    std::fs::write(
        repo.join("main.py"),
        "from core import core_process, load_config\n\n\
         def main():\n    cfg = load_config()\n    return core_process(cfg.retries)\n",
    )
    .expect("main");
    for i in 0..40 {
        std::fs::write(
            repo.join(format!("mod{i:02}.py")),
            format!(
                "from core import core_process\n\n\
                 def process_item{i}(v):\n    return core_process(v) + {i}\n\n\
                 def config_helper{i}():\n    return {i}\n\n\
                 class Widget{i}:\n    def run(self):\n        return process_item{i}({i})\n"
            ),
        )
        .expect("mod");
    }
}
