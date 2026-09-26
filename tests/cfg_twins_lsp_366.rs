// Issue #366, part A, against a real rust-analyzer: after `analyze_codebase`
// with the language-server pass, no call site holds a row to a `#[cfg]` twin
// the default build compiles out, and no site holds rows to two twins of one
// item. rust-analyzer answers under the default features and does not analyze
// compiled-out code, so it is not expected to point at the `fast` twin by
// itself; the scripted-server test in src/lsp_resolver/cfg_twin_pass_tests.rs
// covers the refusal. Needs rust-analyzer, like the other real-LSP tests.

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
        "[package]\nname = \"twin366\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [features]\nfast = []\n",
    )
    .expect("manifest");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub struct S;\n\
         impl S {\n\
         \x20   #[cfg(feature = \"fast\")]\n\
         \x20   pub fn m(&self) -> u32 { 1 }\n\
         \x20   #[cfg(not(feature = \"fast\"))]\n\
         \x20   pub fn m(&self) -> u32 { 2 }\n\
         }\n\
         pub fn caller() -> u32 {\n\
         \x20   let v = vec![S];\n\
         \x20   v[0].m()\n\
         }\n",
    )
    .expect("source");
}

fn rows(server: &mut Server, graph: &Path, query: &str) -> Vec<Vec<String>> {
    let out = server.call(
        "query_graph",
        json!({"graph_path": graph, "query": query, "format": "tabular"}),
    );
    serde_json::from_value(out["rows"].clone()).unwrap_or_else(|_| panic!("rows: {out}"))
}

#[test]
#[ignore = "requires rust-analyzer on PATH; run with --ignored"]
fn no_site_holds_a_row_to_a_compiled_out_twin_after_the_lsp_pass() {
    if !rust_analyzer_available() {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("crate");
    write_crate(&root);
    let out = tmp.path().join("out");
    let mut server = Server::spawn();
    let analysis = server.call(
        "analyze_codebase",
        json!({"path": root, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": true}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    let graph = out.join("graph");
    let to_inactive = rows(
        &mut server,
        &graph,
        "MATCH (cs:CallSite)-[r]->(t) WHERE t.cfg_active = 'inactive' RETURN cs.id, t.id",
    );
    assert!(
        to_inactive.is_empty(),
        "rows to a compiled-out twin: {to_inactive:?}"
    );
    let m_rows = rows(
        &mut server,
        &graph,
        "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.callee_name ENDS WITH '.m' \
         RETURN t.id, r.resolution_method",
    );
    assert_eq!(m_rows.len(), 1, "one target for the one site: {m_rows:?}");
    assert!(m_rows[0][0].contains("not(feature=fast)"), "{m_rows:?}");
    server.shutdown();
}
