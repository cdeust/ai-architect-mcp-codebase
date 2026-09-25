//! Issue #356, migration through the language-server pass: `lsp_resolve` over a
//! graph written before `Calls_CallSite_Struct` existed. A constructor call the
//! static resolver leaves open (two structs named `Tier` in one file, the call
//! sits after a `use` inside the function) is resolved by rust-analyzer to the
//! struct, and its per-site row needs the table. Without the table created first
//! the row is dropped and the site falls back to unresolved.
//!
//! Needs rust-analyzer, like the other real-LSP tests: `--ignored`.

use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

const LIB: &str = "pub mod a {
    pub struct Tier(pub u8);
}
pub mod b {
    pub struct Tier(pub u8);
}
pub fn build() -> a::Tier {
    use a::Tier;
    Tier(1)
}
";

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(BIN)
            .args(["--profile", "full"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                             "params": {"name": name, "arguments": arguments}});
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{request}").expect("write");
        stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read");
        let response: Value = serde_json::from_str(&line).expect("json-rpc line");
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text: {response}"));
        serde_json::from_str(text).expect("tool json")
    }

    fn shutdown(mut self) {
        drop(self.stdin.take());
        let status = self.child.wait().expect("wait");
        assert!(status.success(), "server exit: {status}");
    }
}

fn rust_analyzer_available() -> bool {
    Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn count(graph: &Path, cypher: &str) -> i64 {
    let store = GraphStore::open_or_create(graph).expect("open graph");
    store.execute_query(cypher).expect("query").rows[0][0]
        .parse()
        .expect("a count")
}

/// A crate holding `LIB`, analyzed with the static pass only in its own server
/// process, which is closed before the graph is touched. Returns the graph path.
fn analyzed_and_closed(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("mkdir");
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"lsp356\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(repo.join("src/lib.rs"), LIB).expect("source");
    let out = tmp.join("out");
    let mut first = Server::spawn();
    let analyzed = first.call(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analyzed["status"], "ok", "{analyzed}");
    first.shutdown();
    (repo, out.join("graph"))
}

const OPEN_CTOR: &str = "MATCH (c:CallSite) WHERE c.callee_name = 'Tier' \
                         AND c.is_resolved = false RETURN count(c)";

#[test]
#[ignore = "requires rust-analyzer on PATH; run with --ignored"]
fn lsp_resolve_over_a_graph_without_the_struct_table_writes_the_constructor_row() {
    if !rust_analyzer_available() {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tmp");
    let (repo, graph) = analyzed_and_closed(tmp.path());
    assert_eq!(
        count(&graph, OPEN_CTOR),
        1,
        "control: the static pass leaves the constructor call open"
    );
    // The graph as an earlier build wrote it: no table for a Struct target.
    GraphStore::open_or_create(&graph)
        .expect("open")
        .execute_query("DROP TABLE Calls_CallSite_Struct")
        .expect("drop the table");

    let mut second = Server::spawn();
    let lsp = second.call(
        "lsp_resolve",
        json!({"graph_path": graph, "codebase_path": repo, "language": "rust"}),
    );
    assert_eq!(lsp["status"], "ok", "{lsp}");
    second.shutdown();

    assert_eq!(
        count(
            &graph,
            "MATCH (c:CallSite)-[r:Calls_CallSite_Struct]->(s:Struct) \
             WHERE r.resolution_method = 'lsp-definition' RETURN count(r)"
        ),
        1,
        "the LSP-resolved constructor row is written"
    );
    assert_eq!(count(&graph, OPEN_CTOR), 0, "the site is resolved");
}
