//! Issue #350 over the real stdio wire: a receiver name bound more than once in
//! a function was left unresolved by the static pass even when every binding
//! has the same type. The call now names the binding that is live at the call.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against the official 0.13.0 release, where the
//! shadowed sites must stay unresolved and these tests must FAIL.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "pub struct Set {
    n: u32,
}

pub struct Other {
    n: u32,
}

impl Set {
    pub fn new() -> Set {
        Set { n: 0 }
    }

    pub fn answer(&self, i: u32) -> u32 {
        self.n + i
    }
}

impl Other {
    pub fn new() -> Other {
        Other { n: 1 }
    }

    pub fn answer(&self, i: u32) -> u32 {
        self.n + i
    }
}

pub fn control_single_binding() -> u32 {
    let s = Set::new();
    s.answer(1) // control
}

pub fn shadowed_twice() -> u32 {
    let s = Set::new();
    let first = s.answer(2); // first
    let s = Set::new();
    first + s.answer(3) // second
}

pub fn shadow_changes_type() -> u32 {
    let s = Set::new();
    let a = s.answer(4); // as-set
    let s = Other::new();
    a + s.answer(5) // as-other
}

pub fn closure_parameter_of_the_same_name(v: Vec<Other>) -> u32 {
    let s = Set::new();
    v.iter().map(|s| s.answer(6)).sum::<u32>() // in-closure
        + s.answer(7) // after-closure
}
";

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

struct Analyzed {
    _tmp: tempfile::TempDir,
    graph: std::path::PathBuf,
    server: Server,
}

/// The crate of the issue, analyzed by `analyze_codebase` with the static pass only.
fn analyzed() -> Analyzed {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"shadow350\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), LIB).unwrap();
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    Analyzed {
        _tmp: tmp,
        graph: out.join("graph"),
        server,
    }
}

fn line_of(marker: &str) -> usize {
    LIB.lines()
        .position(|l| l.contains(&format!("// {marker}")))
        .unwrap_or_else(|| panic!("no line holds // {marker}"))
        + 1
}

/// `(target id, resolution_method, confidence)` of the per-site rows of the
/// `answer` call on the line marked `marker`.
fn rows_of(store: &GraphStore, marker: &str) -> Vec<(String, String, String)> {
    let line = line_of(marker);
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.line = {line} \
             AND cs.callee_name ENDS WITH '.answer' \
             RETURN t.id, r.resolution_method, r.confidence"
        ))
        .unwrap_or_else(|e| panic!("query {marker}: {e}"))
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .collect()
}

fn local_binding(target: &str) -> Vec<(String, String, String)> {
    vec![(
        format!("src/lib.rs::{target}::answer"),
        "receiver-local-binding".to_string(),
        "0.87".to_string(),
    )]
}

#[test]
fn a_name_shadowed_by_the_same_constructor_resolves_at_both_sites() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(rows_of(&store, "control"), local_binding("Set"));
    assert_eq!(rows_of(&store, "first"), local_binding("Set"));
    assert_eq!(rows_of(&store, "second"), local_binding("Set"));
}

#[test]
fn a_shadow_that_changes_the_type_resolves_each_site_to_its_own_type() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(rows_of(&store, "as-set"), local_binding("Set"));
    assert_eq!(rows_of(&store, "as-other"), local_binding("Other"));
}

#[test]
fn a_closure_parameter_of_the_same_name_is_declined_inside_and_resolved_after() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(rows_of(&store, "in-closure"), vec![]);
    assert_eq!(rows_of(&store, "after-closure"), local_binding("Set"));
}

#[test]
fn get_impact_lists_the_function_that_shadows_its_receiver() {
    let mut a = analyzed();
    let impact = a.server.call_tool(
        "get_impact",
        json!({"graph_path": a.graph, "qualified_name": "src/lib.rs::Set::answer"}),
    );
    let callers: Vec<String> = impact["callers"]
        .as_array()
        .unwrap_or_else(|| panic!("no callers: {impact}"))
        .iter()
        .map(|c| c["qualified_name"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "src/lib.rs::control_single_binding",
        "src/lib.rs::shadowed_twice",
        "src/lib.rs::shadow_changes_type",
    ] {
        assert!(
            callers.iter().any(|c| c == expected),
            "{expected} in {callers:?}"
        );
    }
}
