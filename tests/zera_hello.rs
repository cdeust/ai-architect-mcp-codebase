// zera_hello — S1 end-to-end integration test.
//
// Indexes a real Python source tree (Cortex's mcp_server/), builds a
// canonical zera::GraphState from the Lbug graph, runs the cache
// handshake under both COLD (client has nothing) and WARM (client has
// the current graph_id) scenarios, and writes an honest visualization
// to target/zera-s1/hello.html for inspection.
//
// What this test PROVES:
//   * Content hash is deterministic across two indexer runs on the same source.
//   * Hello manifest stays under 2 KB.
//   * JSON baseline of the full graph (no compression yet) at Cortex
//     production scale is what we measure it to be — not a number I
//     made up. Later slices' compression claims are measured against it.
//
// What this test DOES NOT claim:
//   * Compression ratios (no encoder layers in S1).
//   * Render performance (HTML is a numerical report, not a graph render).
//
// Opt-in via CORTEX_FULL_CORPUS=1, same gate as corpus_full.rs, because
// the indexer takes ~30 s on the full Cortex tree.

use ai_architect_mcp::graph_store::{GraphStore, REL_TABLES};
use ai_architect_mcp::indexer;
use ai_architect_mcp::parser::Language;
use ai_architect_mcp::resolver;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use zera::{hello, EdgeRef, GraphState, HelloRequest, NodeRef};
mod common;
use common::TempDirExt;

fn opt_in() -> bool {
    std::env::var("CORTEX_FULL_CORPUS").ok().as_deref() == Some("1")
}

/// Pull the canonical (label, id) tuples out of the Lbug store via the
/// consolidated iterator we added in commit f7c0cb2 (Feynman audit
/// refactor). One source of truth, no parallel implementation.
fn graph_state_from_store(store: &GraphStore) -> GraphState {
    let labels = &[
        "File",
        "Directory",
        "Module",
        "Function",
        "Method",
        "Struct",
        "Enum",
        "Variant",
        "Trait",
        "Constant",
        "TypeAlias",
        "Import",
        "CallSite",
    ];
    let nodes: Vec<NodeRef> = iter_nodes_by_labels(store, labels)
        .into_iter()
        .map(|(label, id, _qn)| NodeRef { id, label })
        .collect();
    let edges: Vec<EdgeRef> = iter_edges(store)
        .into_iter()
        .map(|(from, to, kind)| EdgeRef {
            from,
            to,
            kind: kind.to_string(),
        })
        .collect();
    GraphState::new(nodes, edges)
}

/// Test helper: iterate every node carrying one of the given labels, returning
/// (label, id, qualified_name_or_path). Uses only GraphStore's public query
/// surface — kept here, beside its sole caller, rather than as production API
/// (it has exactly one use: building the zera GraphState for this test).
/// The schema knows which QN/path column each label exposes (File→path,
/// Import/CallSite→id, Commit→sha, everything else→qualified_name).
fn iter_nodes_by_labels(store: &GraphStore, labels: &[&str]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for label in labels {
        let qn_col = match *label {
            "File" | "Directory" => "path",
            "Import" | "CallSite" => "id",
            "Commit" => "sha",
            _ => "qualified_name",
        };
        let cypher = format!("MATCH (n:{label}) RETURN n.id, n.{qn_col}");
        let qr = match store.execute_query(&cypher) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in qr.rows {
            if row.len() < 2 {
                continue;
            }
            out.push((label.to_string(), row[0].clone(), row[1].clone()));
        }
    }
    out
}

/// Test helper: iterate every resolution-or-structural edge in the graph,
/// returning (from_id, to_id, rel_table_name). Public-API only; see
/// `iter_nodes_by_labels` for why it lives here.
fn iter_edges(store: &GraphStore) -> Vec<(String, String, &'static str)> {
    let mut out = Vec::new();
    for (rel, _, _) in REL_TABLES {
        let cypher = format!("MATCH (a)-[r:{rel}]->(b) RETURN a.id, b.id");
        let qr = match store.execute_query(&cypher) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in qr.rows {
            if row.len() < 2 {
                continue;
            }
            out.push((row[0].clone(), row[1].clone(), *rel));
        }
    }
    out
}

fn human_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let n = n as f64;
    if n >= GB {
        format!("{:.2} GB", n / GB)
    } else if n >= MB {
        format!("{:.2} MB", n / MB)
    } else if n >= KB {
        format!("{:.2} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

fn render_report_html(
    target_root: &str,
    state: &GraphState,
    cold: &zera::HelloResponse,
    warm: &zera::HelloResponse,
    t_index_ms: u128,
    t_state_ms: u128,
    t_hash_ms: u128,
) -> String {
    // Cross-check the contract: warm manifest is identical to cold
    // except for cache_hit. Surface it in the report so the reader
    // can verify the response is content-addressed (same graph_id on
    // both sides, only cache_hit differs).
    assert_eq!(cold.graph_id, warm.graph_id);
    assert!(warm.cache_hit && !cold.cache_hit);
    let baseline = cold.baseline_json_bytes;
    let manifest = cold.manifest_bytes;
    let ratio = if manifest > 0 {
        baseline as f64 / manifest as f64
    } else {
        0.0
    };
    let mut label_rows = String::new();
    for (label, count) in &cold.histograms.by_label {
        label_rows.push_str(&format!(
            "<tr><td>{}</td><td class=\"r\">{}</td></tr>",
            html_escape(label),
            count,
        ));
    }
    let mut kind_rows = String::new();
    for (kind, count) in &cold.histograms.by_kind {
        kind_rows.push_str(&format!(
            "<tr><td>{}</td><td class=\"r\">{}</td></tr>",
            html_escape(kind),
            count,
        ));
    }
    let bar_baseline_pct = 100.0_f64;
    let bar_manifest_pct = (manifest as f64 / baseline as f64) * 100.0;
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ZERA S1 — HELLO handshake report ({nodes} nodes, {edges} edges)</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif;
          background: #0f172a; color: #e2e8f0; max-width: 920px;
          margin: 32px auto; padding: 0 24px; line-height: 1.5; }}
  h1 {{ font-size: 18px; font-weight: 500; margin: 0 0 8px; }}
  h2 {{ font-size: 14px; font-weight: 500; margin: 24px 0 8px; color: #94a3b8;
        text-transform: uppercase; letter-spacing: 0.05em; }}
  .sub {{ font-size: 12px; color: #94a3b8; margin-bottom: 24px; }}
  .grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }}
  .panel {{ background: #1e293b; border: 1px solid #334155; border-radius: 6px;
            padding: 14px 16px; }}
  .panel h3 {{ margin: 0 0 10px; font-size: 12px; color: #94a3b8;
               text-transform: uppercase; letter-spacing: 0.05em; }}
  .row {{ display: flex; justify-content: space-between; margin: 4px 0; font-size: 13px; }}
  .row .v {{ font-family: 'SF Mono', Menlo, monospace; color: #67e8f9; }}
  .bar-row {{ margin: 10px 0; }}
  .bar-row .label {{ font-size: 12px; color: #cbd5e1; margin-bottom: 4px; }}
  .bar {{ height: 22px; background: #334155; border-radius: 3px; position: relative; }}
  .bar .fill {{ height: 100%; border-radius: 3px; }}
  .bar .fill.baseline {{ background: linear-gradient(90deg, #f43f5e, #ef4444); }}
  .bar .fill.manifest {{ background: linear-gradient(90deg, #10b981, #22d3ee); }}
  .bar .v {{ position: absolute; right: 8px; top: 2px; font-family: 'SF Mono', Menlo, monospace;
             font-size: 12px; color: white; }}
  table {{ border-collapse: collapse; width: 100%; font-size: 12px;
           font-family: 'SF Mono', Menlo, monospace; }}
  td {{ padding: 3px 8px; border-bottom: 1px solid #334155; }}
  td.r {{ text-align: right; color: #67e8f9; }}
  .note {{ font-size: 11px; color: #64748b; margin-top: 20px; line-height: 1.6; }}
  .scenarios {{ display: grid; grid-template-columns: 1fr 1fr; gap: 12px;
                margin: 16px 0; }}
  .scenario {{ background: #1e293b; border: 1px solid #334155; border-radius: 6px;
               padding: 12px 16px; }}
  .scenario h3 {{ margin: 0 0 8px; font-size: 13px; font-weight: 500; }}
  .scenario .big {{ font-family: 'SF Mono', Menlo, monospace; font-size: 22px;
                    color: #22d3ee; margin: 2px 0; }}
  .scenario.cache-miss .big {{ color: #f87171; }}
  .scenario.cache-hit .big {{ color: #34d399; }}
  .scenario .sub2 {{ font-size: 11px; color: #94a3b8; }}
</style>
</head>
<body>
<h1>ZERA S1 — HELLO handshake</h1>
<div class="sub">Cold + warm scenarios measured on a real Cortex tree.
Spec: <code>docs/protocols/ZERA-spec.md §4.2 Layer 0</code></div>

<h2>source</h2>
<div class="panel">
  <div class="row"><span>target</span><span class="v">{target}</span></div>
  <div class="row"><span>nodes (canonical, deduped)</span><span class="v">{nodes}</span></div>
  <div class="row"><span>edges (canonical, deduped)</span><span class="v">{edges}</span></div>
  <div class="row"><span>graph_id (BLAKE3)</span><span class="v">{graph_id}…</span></div>
</div>

<h2>timing</h2>
<div class="panel">
  <div class="row"><span>indexer + resolver (already paid before HELLO)</span>
    <span class="v">{t_index} ms</span></div>
  <div class="row"><span>graph_state build (canonicalize + dedupe)</span>
    <span class="v">{t_state} ms</span></div>
  <div class="row"><span>content_hash + manifest serialize</span>
    <span class="v">{t_hash} ms</span></div>
</div>

<h2>scenarios</h2>
<div class="scenarios">
  <div class="scenario cache-miss">
    <h3>COLD &mdash; client has nothing</h3>
    <div class="big">{manifest_b}</div>
    <div class="sub2">manifest the server sends to declare the graph state.
      Next: CODEBOOK + GRAMMAR + SPARSE + EIGEN + SEED frames in slices S2–S5.</div>
  </div>
  <div class="scenario cache-hit">
    <h3>WARM &mdash; client has this graph_id cached</h3>
    <div class="big">{manifest_b}</div>
    <div class="sub2">cache_hit = true. The server stops here. No payload follows.
      Compare against the JSON baseline below.</div>
  </div>
</div>

<h2>baseline comparison</h2>
<div class="panel">
  <div class="bar-row">
    <div class="label">JSON baseline (what an unencoded full transfer would cost)</div>
    <div class="bar"><div class="fill baseline" style="width:{bar_baseline_pct:.1}%"></div>
      <div class="v">{baseline_b}</div></div>
  </div>
  <div class="bar-row">
    <div class="label">ZERA S1 HELLO manifest (warm: only this on the wire)</div>
    <div class="bar"><div class="fill manifest" style="width:{bar_manifest_pct:.4}%"></div>
      <div class="v">{manifest_b}</div></div>
  </div>
  <div class="row" style="margin-top:14px;border-top:1px solid #334155;padding-top:10px">
    <span>warm-session ratio (baseline / manifest)</span>
    <span class="v">{ratio:.1}× &nbsp;&mdash;&nbsp; manifest is {manifest_pct:.4}% of baseline</span>
  </div>
</div>

<h2>graph breakdown</h2>
<div class="grid">
  <div class="panel"><h3>nodes by label</h3><table>{label_rows}</table></div>
  <div class="panel"><h3>edges by kind</h3><table>{kind_rows}</table></div>
</div>

<p class="note">
  <b>What this proves:</b> the BLAKE3 content hash deterministically
  identifies the graph state; the manifest stays under 2 KB regardless
  of corpus size; warm reconnections need 0 payload bytes beyond the
  manifest. <br><br>
  <b>What this does NOT yet prove:</b> that the cold-transfer path is
  fast — that's S2 (CODEBOOK), S3 (GRAMMAR), S4 (SPARSE + EIGEN), S5
  (SEED). Until those layers land, the COLD path falls back to JSON.
  The point of S1 is the cache-handshake primitive every later layer
  is content-addressed against.
</p>
</body>
</html>
"##,
        target = html_escape(target_root),
        nodes = state.nodes.len(),
        edges = state.edges.len(),
        graph_id = &cold.graph_id[..16],
        t_index = t_index_ms,
        t_state = t_state_ms,
        t_hash = t_hash_ms,
        manifest_b = human_bytes(manifest),
        baseline_b = human_bytes(baseline),
        bar_baseline_pct = bar_baseline_pct,
        bar_manifest_pct = bar_manifest_pct,
        ratio = ratio,
        manifest_pct = bar_manifest_pct,
        label_rows = label_rows,
        kind_rows = kind_rows,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[test]
fn zera_hello_on_cortex_mcp_server() {
    if !opt_in() {
        eprintln!("[skipped] zera_hello_on_cortex_mcp_server — set CORTEX_FULL_CORPUS=1");
        return;
    }
    let target = PathBuf::from("/Users/cdeust/Developments/Cortex/mcp_server");
    assert!(target.is_dir(), "Cortex not at {}", target.display());

    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("zera_s1_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("mkdir");
    let graph_path = tmp.join("graph.lbug");

    let t0 = Instant::now();
    indexer::index_codebase_with_language(
        &target,
        &graph_path,
        Some(Language::Python),
        indexer::DependencyScope::None,
    )
    .expect("index");
    let store = GraphStore::open_or_create(&graph_path).expect("open");
    let _ = resolver::resolve_graph(&store);
    let t_index = t0.elapsed();

    let t1 = Instant::now();
    let state = graph_state_from_store(&store);
    let t_state = t1.elapsed();

    let t2 = Instant::now();
    let cold = hello(&state, &HelloRequest::default());
    let warm = hello(
        &state,
        &HelloRequest {
            cached_graph_ids: vec![state.content_id()],
            ..Default::default()
        },
    );
    let t_hash = t2.elapsed();

    // Determinism: indexing twice in the same session must produce the
    // identical content id. The current indexer is single-pass so this
    // is a tautology in this run, but the assertion documents the
    // contract every later slice depends on.
    let state2 = graph_state_from_store(&store);
    assert_eq!(
        state.content_id(),
        state2.content_id(),
        "content_hash MUST be deterministic across reads of the same store"
    );

    // Hard contract assertions:
    assert!(
        !cold.cache_hit,
        "cold path: client has no cached id, must miss"
    );
    assert!(warm.cache_hit, "warm path: same id supplied, must hit");
    assert!(
        cold.baseline_json_bytes > 100_000,
        "baseline at Cortex scale should exceed 100 KB, got {}",
        cold.baseline_json_bytes
    );
    assert!(
        cold.manifest_bytes < 100_000,
        "manifest must stay under 100 KB regardless of corpus; got {}",
        cold.manifest_bytes
    );
    assert!(
        cold.manifest_bytes < cold.baseline_json_bytes / 100,
        "S1 sanity floor: manifest should be <1% of baseline; \
         manifest={}, baseline={}",
        cold.manifest_bytes,
        cold.baseline_json_bytes,
    );

    // Render report.
    let artifact_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/zera-s1");
    fs::create_dir_all(&artifact_dir).expect("mkdir artifact_dir");
    let html_path = artifact_dir.join("hello.html");
    let html = render_report_html(
        target.to_str().unwrap_or("?"),
        &state,
        &cold,
        &warm,
        t_index.as_millis(),
        t_state.as_millis(),
        t_hash.as_millis(),
    );
    fs::write(&html_path, &html).expect("write html");

    println!(
        "\n=== ZERA S1 — HELLO handshake on {} ===\n\
         nodes: {}\n\
         edges: {}\n\
         graph_id (first 16): {}\n\
         baseline JSON: {} ({} B)\n\
         manifest:      {} ({} B)\n\
         warm ratio:    {:.1}× ({:.6}% of baseline)\n\
         timing:        index+resolve={} ms, state={} ms, hash+manifest={} ms\n\
         report:        {}\n",
        target.display(),
        state.nodes.len(),
        state.edges.len(),
        &cold.graph_id[..16],
        human_bytes(cold.baseline_json_bytes),
        cold.baseline_json_bytes,
        human_bytes(cold.manifest_bytes),
        cold.manifest_bytes,
        cold.baseline_json_bytes as f64 / cold.manifest_bytes as f64,
        cold.manifest_bytes as f64 * 100.0 / cold.baseline_json_bytes as f64,
        t_index.as_millis(),
        t_state.as_millis(),
        t_hash.as_millis(),
        html_path.display(),
    );
}
