//! Issue #366, part B, over the real stdio wire: the single-sided
//! `#[cfg_attr(feature = "x", path = "x.rs")] mod imp;`. When `x` holds the
//! module is `x.rs`; otherwise it is the default-name file `imp.rs`, which
//! therefore exists only under `not(feature = "x")`. The two files hold twins
//! of `imp::pick`, and the call resolves to the one the default build compiles.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "#[cfg_attr(feature = \"x\", path = \"x.rs\")]
mod imp;

pub fn caller() -> u32 {
    imp::pick()
}
";
const IMP: &str = "pub fn pick() -> u32 {\n    1\n}\n";
const X: &str = "pub fn pick() -> u32 {\n    2\n}\n";

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

/// Analyzes the fixture with `features` through `analyze_codebase` and returns
/// `(target, method)` of every per-site row of the call to `pick`.
fn pick_targets(features: &str) -> Vec<(String, String)> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[package]\nname = \"fallback\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [features]\n{features}\n"
        ),
    )
    .unwrap();
    for (name, text) in [("src/lib.rs", LIB), ("src/imp.rs", IMP), ("src/x.rs", X)] {
        std::fs::write(repo.join(name), text).unwrap();
    }
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "dependency_scope": "none"}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    drop(server);
    let store = GraphStore::open_or_create(&out.join("graph")).unwrap();
    let mut rows: Vec<(String, String)> = store
        .execute_query(
            "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(t:Function) \
             WHERE c.callee_name ENDS WITH 'pick' RETURN t.id, r.resolution_method",
        )
        .expect("rows")
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect();
    rows.sort();
    rows
}

fn row(target: &str) -> Vec<(String, String)> {
    vec![(target.to_string(), "cfg-selected".to_string())]
}

#[test]
fn feature_off_resolves_to_the_default_name_file() {
    assert_eq!(pick_targets("x = []"), row("src/imp.rs::pick"));
}

#[test]
fn feature_on_resolves_to_the_cfg_attr_path() {
    assert_eq!(
        pick_targets("default = [\"x\"]\nx = []"),
        row("src/x.rs::pick")
    );
}
