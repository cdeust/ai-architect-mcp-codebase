//! Issue #318 over the real stdio wire, with a real rust-analyzer: a crate
//! whose method has a resolved caller in the library AND an unresolved call
//! site in `kani/h.rs`, a file no Cargo target compiles (the dy-wcet
//! `kani/response_bounds.rs` shape). `get_impact` must point at those sites in
//! `next_steps` although `callers` is not empty, and the query it hands out
//! must return them when it is run through `query_graph`.
//!
//! `#[ignore]`d because it needs rust-analyzer and cargo on PATH (the language
//! server is what attributes a site outside the compiled targets); run with
//! `cargo test --test impact_outside_targets_stdio_318 -- --ignored`.
use ai_architect_mcp::lsp_client::is_command_available;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "pub struct Widget;\n\
impl Widget {\n    pub fn value(&self) -> i32 { 1 }\n    pub fn twice(&self) -> i32 { self.value() * 2 }\n}\n";
const HARNESS: &str = "fn harness() {\n    let w = Widget;\n    w.value();\n}\n";

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

/// The text between `query_graph(query="` and the next `")` of a step.
fn handed_out_query(step: &str) -> String {
    let start = step
        .find("query_graph(query=\"")
        .map(|i| i + "query_graph(query=\"".len())
        .unwrap_or_else(|| panic!("no query in the step: {step}"));
    let end = step[start..].find("\")").expect("closing of the query") + start;
    step[start..end].to_string()
}

/// A crate whose `src/lib.rs` calls `Widget::value` from a method, plus
/// `kani/h.rs`, a file no Cargo target compiles, that calls it on a receiver.
fn write_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("kani")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fx318\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), LIB).unwrap();
    std::fs::write(repo.join("kani/h.rs"), HARNESS).unwrap();
    (tmp, repo, out)
}

/// The `next_steps` entry that points at the outside-target sites.
fn outside_step(impact: &Value) -> String {
    impact["next_steps"]
        .as_array()
        .expect("next_steps")
        .iter()
        .filter_map(Value::as_str)
        .find(|s| s.contains("outside every compiled Cargo target"))
        .unwrap_or_else(|| panic!("no outside-targets step: {impact}"))
        .to_string()
}

/// Runs the query the step hands out and asserts it lists the harness site.
fn assert_the_query_lists_the_harness_site(server: &mut Server, graph: &Path, step: &str) {
    let listed = server.call_tool(
        "query_graph",
        json!({"graph_path": graph, "query": handed_out_query(step), "format": "tabular"}),
    );
    let rows = listed["rows"].as_array().expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "the handed-out query lists the one site: {listed}"
    );
    let id = rows[0][0].as_str().expect("an id");
    assert!(id.starts_with("kani/h.rs::harness::call@"), "{id}");
}

#[test]
#[ignore = "requires rust-analyzer and cargo on PATH; run with `cargo test --test \
            impact_outside_targets_stdio_318 -- --ignored`"]
fn get_impact_points_at_outside_target_sites_even_with_resolved_callers() {
    if !is_command_available("rust-analyzer") || !is_command_available("cargo") {
        eprintln!("skipping: rust-analyzer or cargo not on PATH");
        return;
    }
    let (_tmp, repo, out) = write_fixture();
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "dependency_scope": "none",
               "language": "rust", "lsp": true}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    let graph = out.join("graph");

    let impact = server.call_tool(
        "get_impact",
        json!({"graph_path": graph, "qualified_name": "src/lib.rs::Widget::value"}),
    );
    assert_eq!(
        impact["unresolved_callsites_outside_targets"], 1,
        "{impact}"
    );
    let callers = impact["callers"].as_array().expect("callers");
    assert!(
        !callers.is_empty(),
        "the library caller must resolve: {impact}"
    );

    let step = outside_step(&impact);
    assert!(step.contains("(kani/h.rs)"), "{step}");
    assert_the_query_lists_the_harness_site(&mut server, &graph, &step);

    let missed = server.call_tool(
        "query_graph",
        json!({"graph_path": graph, "graph": "missed"}),
    );
    let files = missed["coverage"]["outside_build_targets"]["files"].to_string();
    assert!(files.contains("kani/h.rs"), "{missed}");
}
