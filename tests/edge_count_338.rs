// edge_count_338 — issue #338: after #335 filled the per-site
// `Calls_CallSite_*` tables, `GraphStore::edge_count` summed them, so
// `index_status.edge_count` grew by about 3 percent with no new call while
// `analyze_codebase` reported an `index.edge_count` frozen before resolve ran.
// One graph answered "how many edges" with different numbers depending on the
// tool. These tests pin one definition: a graph edge is a relationship row
// that states a fact of its own; per-site rows restate a resolution the
// symbol-level `Calls_*` edge records and are reported as their own figure.

use ai_architect_mcp::graph_store::{GraphStore, REL_TABLES};
use ai_architect_mcp::{indexer, resolver};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
mod common;
use common::TempDirExt;

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

const FIXTURE: &str = "pub struct Counter {
    n: u32,
}

impl Counter {
    pub fn bump(&mut self) {
        self.n += 1;
    }

    pub fn bump_twice(&mut self) {
        self.bump();
        self.bump();
    }
}

pub fn helper() -> u32 {
    7
}

pub fn entry() -> u32 {
    let x = helper();
    println!(\"x = {}\", x);
    x
}
";

const PER_SITE_TABLES: [&str; 3] = [
    "Calls_CallSite_Function",
    "Calls_CallSite_Method",
    "Calls_CallSite_StdlibSymbol",
];

/// Rows in one relationship table, read with a plain count query.
fn rows_in(store: &GraphStore, table: &str) -> u64 {
    let qr = store
        .execute_query(&format!("MATCH ()-[r:{table}]->() RETURN count(r)"))
        .unwrap_or_else(|e| panic!("count {table}: {e}"));
    qr.rows[0][0].parse().expect("a row count")
}

fn index_and_resolve() -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("edge_count_338_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture/src");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::write(src.join("lib.rs"), FIXTURE).expect("write fixture");
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&tmp.path().join("fixture"), &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// The defect: `edge_count` was the raw sum over every relationship table, so
/// the per-site rows entered it. It must equal the raw sum minus exactly the
/// per-site rows, and the two figures must add back to the raw sum.
#[test]
fn edge_count_counted_the_per_site_rows_as_edges() {
    let (store, _tmp) = index_and_resolve();
    let raw_total: u64 = REL_TABLES
        .iter()
        .map(|&(rel, _, _)| rows_in(&store, rel))
        .sum();
    let per_site: u64 = PER_SITE_TABLES.iter().map(|t| rows_in(&store, t)).sum();
    assert!(
        per_site > 0,
        "the fixture resolves calls, so per-site rows exist"
    );

    let counts = store.graph_counts().expect("graph counts");
    assert_eq!(counts.call_site_targets, per_site);
    assert_eq!(
        counts.edges,
        raw_total - per_site,
        "edge_count must leave the per-site rows out"
    );
    assert_eq!(counts.edges + counts.call_site_targets, raw_total);
    assert_eq!(store.edge_count().expect("edge count"), counts.edges);
}

/// Resolving twice writes no new fact, so no figure moves.
#[test]
fn a_second_resolve_leaves_both_figures_unchanged() {
    let (store, _tmp) = index_and_resolve();
    let first = store.graph_counts().expect("first counts");
    resolver::resolve_graph(&store).expect("second resolve");
    assert_eq!(store.graph_counts().expect("second counts"), first);
}

/// A live MCP server subprocess driven over its stdio wire; killed on drop.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(BIN)
            .args(["--profile", "full"])
            .env_remove("AP_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ai-architect-mcp-codebase");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn call_tool(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                         "params": {"name": name, "arguments": arguments}});
        writeln!(self.stdin, "{req}").expect("write request line");
        self.stdin.flush().expect("flush stdin");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(&line).expect("a JSON-RPC response line");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no content[0].text: {resp:?}"));
        serde_json::from_str(text).expect("tool response is JSON")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_repo(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("mkdir src");
    fs::write(dir.join("src/lib.rs"), FIXTURE).expect("write fixture");
}

/// `analyze_codebase` reported the edge count the index phase took before
/// resolve ran, so it disagreed with `index_status` on the same finished
/// graph. Its `graph` block is read after the last phase and must match
/// `index_status` field for field.
#[test]
fn analyze_and_index_status_report_the_same_edge_count() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    write_repo(&repo);
    let mut server = Server::spawn();

    let analyzed = server.call_tool(
        1,
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "dependency_scope": "none"}),
    );
    assert_eq!(analyzed["status"], "ok", "{analyzed}");
    let status = server.call_tool(2, "index_status", json!({"graph_path": out.join("graph")}));
    assert_eq!(status["status"], "ok", "{status}");

    for field in ["node_count", "edge_count", "call_site_target_count"] {
        assert_eq!(
            analyzed["graph"][field], status[field],
            "{field}: {analyzed}"
        );
    }
    assert!(
        status["call_site_target_count"].as_u64().unwrap_or(0) > 0,
        "the fixture resolves calls: {status}"
    );
    assert!(
        analyzed["graph"]["edge_count"].as_u64() >= analyzed["index"]["edge_count"].as_u64(),
        "resolve only adds edges to the index-phase snapshot: {analyzed}"
    );
}
