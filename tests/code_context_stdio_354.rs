//! Issue #354 over the real stdio wire: the reproduction of the issue, a crate
//! with one production function called from a plain test, an async test, a helper
//! in a `#[cfg(test)]` module, a helper in `tests/`, a bench and an example,
//! analyzed through `analyze_codebase` and read through `query_graph` and
//! `get_impact`.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against the official 0.13.0 release, where it must
//! FAIL (no `context`, no `callers_production_total`, `#[tokio::test]` unmarked).
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "pub fn prod() -> u32 { 1 }\n\
pub fn production_user() -> u32 { prod() }\n\
#[test]\nfn plain_test() { assert_eq!(prod(), 1); }\n\
#[tokio::test]\nasync fn async_test() { assert_eq!(prod(), 1); }\n\
#[cfg(test)]\nmod tests {\n    use super::*;\n    fn helper_in_test_module() -> u32 { prod() }\n    #[test]\n    fn nested_test() { assert_eq!(helper_in_test_module(), 1); }\n}\n";

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

impl Analyzed {
    fn new(lib: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let out = tmp.path().join("out");
        for dir in ["src", "tests", "benches", "examples"] {
            std::fs::create_dir_all(repo.join(dir)).unwrap();
        }
        let files = [
            (
                "Cargo.toml",
                "[package]\nname = \"fx2b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", lib),
            (
                "tests/common.rs",
                "pub fn shared_helper() -> u32 { fx2b::prod() }\n",
            ),
            (
                "benches/b.rs",
                "fn bench_like() -> u32 { fx2b::prod() }\nfn main() { let _ = bench_like(); }\n",
            ),
            ("examples/e.rs", "fn main() { let _ = fx2b::prod(); }\n"),
        ];
        for (name, text) in files {
            std::fs::write(repo.join(name), text).unwrap();
        }
        let mut server = Server::spawn();
        let analysis = server.call_tool(
            "analyze_codebase",
            json!({"path": repo, "output_dir": out, "dependency_scope": "none",
                   "language": "rust", "lsp": false}),
        );
        assert_eq!(analysis["status"], "ok", "{analysis}");
        Analyzed {
            _tmp: tmp,
            graph: out.join("graph"),
            server,
        }
    }

    fn call(&mut self, tool: &str, extra: Value) -> Value {
        let mut arguments = json!({ "graph_path": self.graph });
        for (key, value) in extra.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        self.server.call_tool(tool, arguments)
    }

    fn impact_of(&mut self, qn: &str) -> Value {
        self.call(
            "get_impact",
            json!({"qualified_name": qn, "detail": "full", "format": "json"}),
        )
    }
}

fn contexts(impact: &Value) -> BTreeMap<String, String> {
    impact["callers"]
        .as_array()
        .unwrap_or_else(|| panic!("no callers: {impact}"))
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["context"].as_str().unwrap_or("<none>").to_string(),
            )
        })
        .collect()
}

#[test]
fn every_test_bench_and_example_caller_is_told_apart_from_production() {
    let mut a = Analyzed::new(LIB);
    let impact = a.impact_of("src/lib.rs::prod");
    assert_eq!(impact["status"], "ok", "{impact}");
    let expected: BTreeMap<String, String> = [
        ("benches/b.rs::bench_like", "bench"),
        ("examples/e.rs::main", "example"),
        ("src/lib.rs::async_test", "test"),
        ("src/lib.rs::plain_test", "test"),
        ("src/lib.rs::production_user", "production"),
        ("src/lib.rs::tests::helper_in_test_module", "test"),
        ("tests/common.rs::shared_helper", "test"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(contexts(&impact), expected, "{impact}");
    assert_eq!(impact["callers_total"], 7);
    assert_eq!(impact["callers_production_total"], 1);
    assert_eq!(
        impact["callers_by_context"],
        json!({"bench": 1, "example": 1, "production": 1, "test": 4})
    );
    assert_eq!(impact["dependents_production_total"], 1);
    assert_eq!(impact["dependents_total"], 7);
    assert_eq!(impact["code_context_basis"], "cargo+source");
}

#[test]
fn a_function_only_test_code_can_reach_has_no_production_caller() {
    let mut a = Analyzed::new(LIB);
    let impact = a.impact_of("src/lib.rs::tests::helper_in_test_module");
    assert_eq!(contexts(&impact).len(), 1, "{impact}");
    assert_eq!(impact["callers_production_total"], 0, "{impact}");
    assert_eq!(impact["callers_by_context"], json!({"test": 1}));
}

#[test]
fn the_graph_marks_the_functions_the_issue_listed_with_an_empty_entry_kind() {
    let mut a = Analyzed::new(LIB);
    let rows = a.call(
        "query_graph",
        json!({"query": "MATCH (n:Function) RETURN n.qualified_name, n.entry_kind, \
                         n.code_context ORDER BY n.qualified_name",
               "format": "tabular"}),
    );
    let table: BTreeMap<String, (String, String)> = rows["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("no rows: {rows}"))
        .iter()
        .map(|r| {
            (
                r[0].as_str().unwrap().to_string(),
                (
                    r[1].as_str().unwrap_or("").to_string(),
                    r[2].as_str().unwrap_or("").to_string(),
                ),
            )
        })
        .collect();
    let pair = |entry: &str, ctx: &str| (entry.to_string(), ctx.to_string());
    assert_eq!(
        table["src/lib.rs::async_test"],
        pair("test", "test"),
        "{rows}"
    );
    assert_eq!(table["src/lib.rs::plain_test"], pair("test", "test"));
    assert_eq!(
        table["src/lib.rs::tests::helper_in_test_module"],
        pair("", "test")
    );
    assert_eq!(
        table["src/lib.rs::tests::nested_test"],
        pair("test", "test")
    );
    assert_eq!(table["src/lib.rs::prod"], pair("", ""));
    assert_eq!(table["tests/common.rs::shared_helper"], pair("", ""));
}
