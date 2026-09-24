// Issue #352: `lsp_resolve` returned `completed` while the rows it wrote were
// gone after the next open, whenever a `query_graph` had run before it in the
// same server process. The read cache kept a handle open across the write and
// undid it when it closed.
//
// Each sequence below runs in one server process, closes it cleanly, starts a
// second one and counts the `lsp-definition` rows. Every order of calls must
// leave the same rows. Needs rust-analyzer, like the other real-LSP tests.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
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
            next_id: 1,
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
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

    /// Closes stdin, as a client ending its session does, and waits for exit.
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

fn write_crate(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"persist_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub struct TaskSet;\nimpl TaskSet {\n    pub fn response_of(&self) -> u64 { 1 }\n}\n\
         pub fn total(sets: &[TaskSet]) -> u64 { sets[0].response_of() }\n",
    )
    .expect("source");
}

const LSP_ROWS: &str = "MATCH (c:CallSite)-[r:Calls_CallSite_Method]->(m:Method) \
                        WHERE r.resolution_method = 'lsp-definition' RETURN count(r)";
const ANY_ROW: &str = "MATCH (c:CallSite) RETURN count(c)";

/// Runs `steps` in one server after the analysis, then reopens in a second
/// server and returns the durable `lsp-definition` row count.
fn durable_lsp_rows(steps: &[&str]) -> u64 {
    let tmp = tempfile::tempdir().expect("tmp");
    let crate_dir = tmp.path().join("crate");
    write_crate(&crate_dir);
    let out = tmp.path().join("out");
    let graph = out.join("graph");

    let mut first = Server::spawn();
    let analyzed = first.call(
        "analyze_codebase",
        json!({"path": crate_dir, "output_dir": out, "language": "rust", "lsp": false}),
    );
    assert_eq!(analyzed["status"], "ok", "{analyzed}");
    for step in steps {
        match *step {
            "query" => {
                let q = first.call(
                    "query_graph",
                    json!({"graph_path": graph, "query": ANY_ROW, "format": "tabular"}),
                );
                assert_eq!(q["status"], "ok", "{q}");
            }
            "lsp" => {
                let r = first.call(
                    "lsp_resolve",
                    json!({"graph_path": graph, "codebase_path": crate_dir, "language": "rust"}),
                );
                assert_eq!(r["status"], "ok", "{r}");
                assert!(r["resolved_count"].as_u64().unwrap_or(0) > 0, "{r}");
                // Every resolved site leaves a per-site row and a function-level
                // edge, so the durable rows are at least the resolved sites.
                assert!(
                    r["persisted"]["lsp_rows"].as_u64().unwrap_or(0)
                        >= r["resolved_count"].as_u64().unwrap_or(u64::MAX),
                    "the response must report the durable rows: {r}"
                );
            }
            other => panic!("unknown step {other}"),
        }
    }
    first.shutdown();

    let mut second = Server::spawn();
    let q = second.call(
        "query_graph",
        json!({"graph_path": graph, "query": LSP_ROWS, "format": "tabular"}),
    );
    let rows = q["rows"][0][0]
        .as_u64()
        .or_else(|| q["rows"][0][0].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or_else(|| panic!("count row: {q}"));
    second.shutdown();
    rows
}

#[test]
#[ignore = "requires rust-analyzer on PATH; run with --ignored"]
fn a_query_before_lsp_resolve_does_not_cost_the_lsp_rows() {
    if !rust_analyzer_available() {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }
    let baseline = durable_lsp_rows(&["lsp"]);
    assert!(baseline > 0, "the fixture must yield LSP rows: {baseline}");
    assert_eq!(durable_lsp_rows(&["query", "lsp"]), baseline);
}

#[test]
#[ignore = "requires rust-analyzer on PATH; run with --ignored"]
fn queries_on_both_sides_of_lsp_resolve_keep_the_lsp_rows() {
    if !rust_analyzer_available() {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }
    let baseline = durable_lsp_rows(&["lsp"]);
    assert_eq!(
        durable_lsp_rows(&["query", "query", "lsp", "query"]),
        baseline
    );
    assert_eq!(durable_lsp_rows(&["lsp", "query", "query"]), baseline);
}
