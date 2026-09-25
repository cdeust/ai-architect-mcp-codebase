//! Issue #353 over the real stdio wire: the reproduction of the issue, a crate
//! whose `pick` exists under `feature = "fast"` and under its negation, analyzed
//! through the `analyze_codebase` tool.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against the official 0.13.0 release, where it must
//! FAIL (one `pick` node, one unique-match 0.95 edge, no `cfg_twins` reason).
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "#[cfg(feature = \"fast\")]\npub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(feature = \"fast\"))]\npub fn pick() -> u32 {\n    2\n}\n\n\
pub fn caller() -> u32 {\n    pick()\n}\n";

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let bin = std::env::var("AP_TEST_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_ai-architect-mcp-codebase").to_string());
        let mut child = Command::new(bin)
            .args(["--profile", "full"])
            .env_remove("AP_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let envelope: Value = serde_json::from_str(&line).expect("a JSON-RPC response");
        let text = envelope["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no content text: {envelope}"));
        serde_json::from_str(text).expect("a JSON tool result")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rows(store: &GraphStore, cypher: &str) -> Vec<Vec<String>> {
    store.execute_query(cypher).expect(cypher).rows
}

fn analyzed() -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"twins\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nfast = []\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), LIB).unwrap();
    {
        let mut server = Server::spawn();
        let analysis = server.call_tool(
            "analyze_codebase",
            json!({"path": repo, "output_dir": out, "dependency_scope": "none"}),
        );
        assert_eq!(analysis["status"], "ok", "{analysis}");
    }
    let store = GraphStore::open_or_create(&out.join("graph")).unwrap();
    (tmp, store)
}

#[test]
fn analyze_codebase_gives_one_node_per_twin() {
    let (_tmp, store) = analyzed();
    let picks = rows(
        &store,
        "MATCH (f:Function) WHERE f.name = 'pick' RETURN f.id",
    );
    assert_eq!(picks.len(), 2, "one node stands for two items: {picks:?}");
}

#[test]
fn analyze_codebase_gives_no_edge_to_either_twin() {
    let (_tmp, store) = analyzed();
    let edges = rows(
        &store,
        "MATCH ()-[r]->(f:Function) WHERE f.name = 'pick' RETURN label(r), r.confidence, r.resolution_method",
    );
    let calls: Vec<_> = edges
        .iter()
        .filter(|r| !r[0].starts_with("Defines_"))
        .collect();
    assert!(
        calls.is_empty(),
        "an edge reaches a twin, whichever one the build compiles: {calls:?}"
    );
}

#[test]
fn analyze_codebase_leaves_the_call_site_open_with_the_reason_cfg_twins() {
    let (_tmp, store) = analyzed();
    let site = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(site.len(), 1, "{site:?}");
    assert_eq!(site[0][0].to_lowercase(), "false", "{site:?}");
    assert_eq!(site[0][1], "cfg_twins", "{site:?}");
}
