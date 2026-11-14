//! Issue #196 over the real stdio wire: a Python repository analyzed through
//! `analyze_codebase`, then exported through `export_wiki_skeleton`, each a
//! JSON-RPC `tools/call` frame on the server's stdin. The framed response
//! must name the artifact the server wrote, by size and SHA-256, and the
//! artifact must be a skeleton of the embedded schema with every page kind
//! and a coverage block on every page.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const MAIN_PY: &str = "from work import hub, step_one\n\n\
def a():\n    hub()\n\n\
def b():\n    hub()\n\n\
def c():\n    hub()\n\n\
def d():\n    hub()\n\n\
def main():\n    step_one()\n    vanished_helper()\n";

const WORK_PY: &str = "def hub():\n    return 1\n\n\
def step_one():\n    return hub()\n";

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
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
            next_id: 1,
        }
    }

    /// One `tools/call` frame; returns the JSON-RPC envelope and the tool
    /// result decoded from its text content.
    fn call_tool(&mut self, name: &str, arguments: Value) -> (Value, Value) {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let envelope: Value = serde_json::from_str(&line).expect("a JSON-RPC response");
        assert_eq!(envelope["jsonrpc"], "2.0", "{envelope}");
        assert_eq!(envelope["id"], id, "{envelope}");
        let text = envelope["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no content text: {envelope}"));
        let result = serde_json::from_str(text).expect("a JSON tool result");
        (envelope, result)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn export_wiki_skeleton_answers_over_stdio_and_writes_the_artifact_it_names() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("main.py"), MAIN_PY).unwrap();
    std::fs::write(repo.join("work.py"), WORK_PY).unwrap();

    let mut server = Server::spawn();
    let (_, analyzed) = server.call_tool(
        "analyze_codebase",
        json!({"path": repo.to_string_lossy(), "output_dir": out.to_string_lossy(),
            "language": "python", "lsp": false, "dependency_scope": "none"}),
    );
    assert_eq!(analyzed["status"], "ok", "{analyzed}");

    let graph = out.join("graph");
    let (envelope, result) = server.call_tool(
        "export_wiki_skeleton",
        json!({"graph_path": graph.to_string_lossy()}),
    );
    assert_ne!(envelope["result"]["isError"], true, "{envelope}");
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["tool"], "export_wiki_skeleton");

    let artifact = std::path::PathBuf::from(result["artifact_path"].as_str().unwrap());
    assert_eq!(
        artifact,
        std::path::absolute(out.join("wiki_skeleton.json")).unwrap()
    );
    let bytes = std::fs::read(&artifact).expect("the artifact the response names");
    assert_eq!(result["artifact_bytes"], bytes.len());
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(result["artifact_sha256"], sha);

    let skeleton: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(skeleton["schema_version"], result["schema_version"]);
    let pages = skeleton["pages"].as_array().unwrap();
    assert_eq!(result["page_count"], pages.len());
    for kind in ["community", "process", "god_node"] {
        let n = pages.iter().filter(|p| p["kind"] == kind).count();
        assert!(n > 0, "no {kind} page: {skeleton}");
        assert_eq!(result["pages_by_kind"][kind], n, "{result}");
    }
    assert!(pages.iter().all(|p| p["coverage"].is_array()), "{skeleton}");

    // The same graph, exported again over the wire, gives the same bytes.
    let (_, again) = server.call_tool(
        "export_wiki_skeleton",
        json!({"graph_path": graph.to_string_lossy()}),
    );
    assert_eq!(again["artifact_sha256"], sha);
}
