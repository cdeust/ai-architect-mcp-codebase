//! Issue #353 over the real stdio wire: the reproduction of the issue, a crate
//! whose `pick` exists under `feature = "fast"` and under its negation, analyzed
//! through the `analyze_codebase` tool.
//!
//! Part B is included: `fast` is off by default in the fixture, so the build
//! compiles the negated twin, the caller's edge goes there as `cfg-selected` at
//! 0.85, and `get_impact`, `get_symbol` and `index_status` show the twins.
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

const FAST_OFF: &str = "[features]\nfast = []\n";

/// One analyzed crate: the temp dir, the graph directory, the server that made
/// it (kept for the tool calls that follow).
struct Analyzed {
    _tmp: tempfile::TempDir,
    graph: std::path::PathBuf,
    server: Server,
}

impl Analyzed {
    fn new(features: &str, lib: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            format!("[package]\nname = \"twins\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{features}"),
        )
        .unwrap();
        std::fs::write(repo.join("src/lib.rs"), lib).unwrap();
        let mut server = Server::spawn();
        let analysis = server.call_tool(
            "analyze_codebase",
            json!({"path": repo, "output_dir": out, "dependency_scope": "none"}),
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
}

fn analyzed() -> (tempfile::TempDir, GraphStore) {
    let a = Analyzed::new(FAST_OFF, LIB);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    (a._tmp, store)
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

/// The twin the build compiles (`fast` is off) gets the edge, with the label
/// that says how it was chosen; the twin it compiles out gets none.
#[test]
fn analyze_codebase_gives_the_edge_to_the_compiled_twin_only() {
    let (_tmp, store) = analyzed();
    let edges = rows(
        &store,
        "MATCH ()-[r]->(f:Function) WHERE f.name = 'pick' \
         RETURN label(r), f.id, r.confidence, r.resolution_method",
    );
    let calls: Vec<_> = edges
        .iter()
        .filter(|r| !r[0].starts_with("Defines_"))
        .collect();
    assert!(!calls.is_empty(), "no call edge reaches a twin: {edges:?}");
    for edge in &calls {
        assert!(edge[1].ends_with("#cfg(not(feature=fast))"), "{calls:?}");
        assert_eq!(edge[2], "0.85", "{calls:?}");
        assert_eq!(edge[3], "cfg-selected", "{calls:?}");
    }
}

#[test]
fn analyze_codebase_resolves_the_call_site_and_leaves_no_reason() {
    let (_tmp, store) = analyzed();
    let site = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(site.len(), 1, "{site:?}");
    assert_eq!(site[0][0].to_lowercase(), "true", "{site:?}");
    assert_eq!(site[0][1], "", "{site:?}");
}

/// The issue's own query: `get_impact` on the bare name names BOTH twins, says
/// which one the default build compiles, and is never exact.
#[test]
fn get_impact_on_the_bare_name_shows_both_twins() {
    let mut a = Analyzed::new(FAST_OFF, LIB);
    let out = a.call("get_impact", json!({"qualified_name": "src/lib.rs::pick"}));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(
        out["qualified_name"],
        "src/lib.rs::pick#cfg(not(feature=fast))"
    );
    assert_eq!(out["cfg_active"], "active");
    assert_eq!(out["cfg_gate"], "not(feature=fast)");
    let twins = out["cfg_twins"].as_array().expect("cfg_twins").clone();
    assert_eq!(twins.len(), 2, "{out}");
    let state = |suffix: &str| {
        twins
            .iter()
            .find(|t| t["id"].as_str().unwrap().ends_with(suffix))
            .map(|t| t["cfg_active"].as_str().unwrap().to_string())
    };
    assert_eq!(state("#cfg(feature=fast)").as_deref(), Some("inactive"));
    assert_eq!(state("#cfg(not(feature=fast))").as_deref(), Some("active"));
    let callers = out["callers"].as_array().expect("callers");
    assert_eq!(callers.len(), 1, "{out}");
    assert_eq!(out["epistemic"], "lower-bound", "{out}");
    let reasons = out["epistemic_reasons"].to_string();
    assert!(reasons.contains("twins of one item"), "{reasons}");
}

/// The twin the default build compiles out has no callers, and says it is
/// compiled out.
#[test]
fn get_impact_on_the_dropped_twin_says_it_is_inactive() {
    let mut a = Analyzed::new(FAST_OFF, LIB);
    let out = a.call(
        "get_impact",
        json!({"qualified_name": "src/lib.rs::pick#cfg(feature=fast)"}),
    );
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["cfg_active"], "inactive");
    assert_eq!(out["callers"].as_array().unwrap().len(), 0, "{out}");
    assert_eq!(
        out["epistemic"], "lower-bound",
        "never exact with a twin: {out}"
    );
}

/// When the default build does not decide, the bare name names no single item:
/// the answer is the list of twins, and the twin's own answer counts the call
/// sites that were left open.
#[test]
fn undecided_twins_are_listed_and_their_open_call_sites_counted() {
    let lib = "#[cfg(kani)]\npub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(kani))]\npub fn pick() -> u32 {\n    2\n}\n\n\
pub fn caller() -> u32 {\n    pick()\n}\n";
    let mut a = Analyzed::new("", lib);
    let bare = a.call("get_impact", json!({"qualified_name": "src/lib.rs::pick"}));
    assert_eq!(bare["status"], "error", "{bare}");
    assert_eq!(bare["reason"], "symbol_not_found");
    assert_eq!(bare["did_you_mean"].as_array().unwrap().len(), 2, "{bare}");
    let twin = a.call(
        "get_impact",
        json!({"qualified_name": "src/lib.rs::pick#cfg(kani)"}),
    );
    assert_eq!(twin["cfg_active"], "unknown", "{twin}");
    assert_eq!(twin["unresolved_callsites_cfg_twins"], 1, "{twin}");
    assert!(
        twin["epistemic_reasons"]
            .to_string()
            .contains("left unresolved"),
        "{twin}"
    );
}

#[test]
fn get_symbol_on_the_bare_name_returns_the_compiled_twin_and_lists_the_others() {
    let mut a = Analyzed::new(FAST_OFF, LIB);
    let out = a.call("get_symbol", json!({"qualified_name": "src/lib.rs::pick"}));
    assert_eq!(out["status"], "ok", "{out}");
    let twins = out["cfg_twins"].as_array().expect("cfg_twins");
    assert_eq!(twins.len(), 2, "{out}");
    assert!(
        out["node"]["data"]
            .to_string()
            .contains("not(feature=fast)"),
        "{out}"
    );
}

/// `index_status` counts the twins and reports the ones the default build
/// compiles out under `feature_gated.items`, next to the gated modules.
#[test]
fn index_status_counts_the_twins_and_the_compiled_out_items() {
    let mut a = Analyzed::new(FAST_OFF, LIB);
    let out = a.call("index_status", json!({}));
    assert_eq!(out["status"], "ok", "{out}");
    let twins = &out["cfg_twins"];
    assert_eq!(twins["sets"], 1, "{out}");
    assert_eq!(twins["members"], 2, "{out}");
    assert_eq!(twins["active"], 1, "{out}");
    assert_eq!(twins["inactive"], 1, "{out}");
    assert_eq!(twins["unknown"], 0, "{out}");
    assert_eq!(twins["unresolved_sites"], 0, "{out}");
    let gated = &out["coverage"]["feature_gated"];
    assert_eq!(gated["count"], 0, "no module is gated: {out}");
    assert_eq!(gated["items"]["count"], 1, "{out}");
    assert!(
        gated["items"]["examples"][0]["id"]
            .as_str()
            .unwrap()
            .ends_with("#cfg(feature=fast)"),
        "{out}"
    );
}
