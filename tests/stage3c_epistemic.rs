// stage3c_epistemic — end-to-end test for epistemic-boundary honesty in
// get_impact (Phase 1 "honest impact").
//
// Fixture: a trait with two implementors and a polymorphic caller. A trait is a
// dynamic-dispatch surface, so its statically-captured dependent set is a LOWER
// BOUND on true impact — get_impact must report `epistemic == LowerBound` with a
// non-empty reason. A concrete leaf function with no dependents must report
// `epistemic == Exact`.
// source: epistemic module contract; checkpoint Phase 1 #1.

use ai_architect_mcp::clustering;
use ai_architect_mcp::epistemic::Boundary;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
mod common;
use common::{TempDirExt, TestTempDir};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

/// A live MCP server subprocess, driven over its real JSON-RPC stdio wire —
/// no library-function shortcuts, so the assertions below exercise the
/// actual `get_impact` MCP contract (issue #283's `unresolved_callsites_
/// naming_target` field and its `next_steps` hint), not just the
/// `clustering::get_impact` struct the rest of this file checks. Copied
/// verbatim from `tests/analyze_coverage_stdio.rs` / `tests/exclude_dirs_
/// and_unreadable.rs` — this project's established per-file pattern for a
/// minimal stdio harness rather than a shared crate dependency.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(BIN)
            .args(["--profile", "full"])
            .env_remove("AP_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ai-architect-mcp-codebase");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").expect("write request line");
        self.stdin.flush().expect("flush stdin");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read response line");
        assert!(
            !line.is_empty(),
            "server closed stdout before responding to {method}"
        );
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response line is not valid JSON ({e}): {line:?}"))
    }

    fn call_tool(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let resp = self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool response has no content[0].text: {resp:?}"));
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("tool response text is not valid JSON ({e}): {text}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// A trait (dynamic-dispatch surface), two declared implementors, and a function
// that invokes the trait method through a trait object.
const FIXTURE: &str = r#"
pub trait Greeter {
    fn greet(&self) -> String;
}

pub struct English;
impl Greeter for English {
    fn greet(&self) -> String { "hello".to_string() }
}

pub struct French;
impl Greeter for French {
    fn greet(&self) -> String { "bonjour".to_string() }
}

pub fn run(g: &dyn Greeter) -> String {
    g.greet()
}

pub fn standalone_leaf() -> i32 {
    7
}
"#;

/// Indexes+resolves `source` under a fresh temp dir (`fixture/src/lib.rs`)
/// and returns the tempdir guard (the caller must keep this bound — dropping
/// it removes the directory tree), the open store, and the graph directory
/// path (needed by tests that also spawn a server against the same path).
/// `prefix` disambiguates concurrent runs (issue #25 audit: process::id()
/// collides across processes under PID reuse; tempfile's random suffix does
/// not). Shared by every test in this file — extracted so no test carries
/// setup plumbing past its own assertions (coding-standards §4.2/§9).
fn indexed_and_resolved(
    prefix: &str,
    source: &str,
) -> (TestTempDir, GraphStore, std::path::PathBuf) {
    let tmp_root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("lib.rs"), source).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&fixture_dir, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve_graph");
    (tmp_root, store, graph_dir)
}

/// Fetches the fixture trait's stored qualified_name rather than guessing
/// its on-disk form (the path-stripped `lib.rs::Greeter` convention).
fn trait_qualified_name(store: &GraphStore) -> String {
    let qr = store
        .execute_query("MATCH (t:Trait) RETURN t.qualified_name")
        .expect("query traits");
    assert!(!qr.rows.is_empty(), "fixture must produce a Trait node");
    qr.rows[0][0].clone()
}

#[test]
fn test_get_impact_reports_lower_bound_for_interface() {
    let (_tmp_root, store, _graph_dir) = indexed_and_resolved("stage3c_epistemic_", FIXTURE);
    let trait_qn = trait_qualified_name(&store);
    let impact = clustering::get_impact(&store, &trait_qn).expect("get_impact(trait)");

    // The two declared `impl Greeter for _` blocks must surface as implementors.
    assert_eq!(
        impact.implementors.len(),
        2,
        "expected 2 implementors (English, French), got {}: {:?}",
        impact.implementors.len(),
        impact
            .implementors
            .iter()
            .map(|n| &n.qualified_name)
            .collect::<Vec<_>>()
    );

    // Core assertion: a trait is a dynamic-dispatch surface → lower bound.
    assert_eq!(
        impact.epistemic,
        Boundary::LowerBound,
        "trait target must be reported as a lower bound, got {:?}",
        impact.epistemic
    );
    assert!(
        impact
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("dynamic-dispatch")),
        "epistemic_reasons must name dynamic dispatch, got {:?}",
        impact.epistemic_reasons
    );
}

/// Split out of `test_get_impact_reports_lower_bound_for_interface` (own
/// concern: edge-confidence bounds, not the epistemic-boundary verdict).
#[test]
fn test_get_impact_implementor_edges_have_valid_confidence() {
    let (_tmp_root, store, _graph_dir) = indexed_and_resolved("stage3c_epistemic_conf_", FIXTURE);
    let trait_qn = trait_qualified_name(&store);
    let impact = clustering::get_impact(&store, &trait_qn).expect("get_impact(trait)");

    // Every implementor edge carries a confidence in (0.0, 1.0]; declared
    // impl-block resolution stores 0.95. source: resolver.rs:658.
    for node in &impact.implementors {
        assert!(
            node.confidence > 0.0 && node.confidence <= 1.0,
            "implementor {} has out-of-range confidence {}",
            node.qualified_name,
            node.confidence
        );
    }
}

#[test]
fn test_get_impact_reports_exact_for_concrete_leaf() {
    let (_tmp_root, store, _graph_dir) = indexed_and_resolved("stage3c_epistemic_leaf_", FIXTURE);

    // `standalone_leaf` is a concrete Function with no callers and no
    // dynamic-dispatch surface — its (empty) dependent set is exhaustive.
    let qr = store
        .execute_query(
            "MATCH (f:Function) WHERE f.name = 'standalone_leaf' \
             RETURN f.qualified_name",
        )
        .expect("query leaf");
    assert!(!qr.rows.is_empty(), "fixture must produce standalone_leaf");
    let leaf_qn = qr.rows[0][0].clone();

    let impact = clustering::get_impact(&store, &leaf_qn).expect("get_impact(leaf)");

    assert_eq!(
        impact.epistemic,
        Boundary::Exact,
        "concrete leaf with no dependents must be exact, got {:?} (reasons: {:?})",
        impact.epistemic,
        impact.epistemic_reasons
    );
    assert!(
        impact.epistemic_reasons.is_empty(),
        "exact result must carry no reasons, got {:?}",
        impact.epistemic_reasons
    );
}

// ---------------------------------------------------------------------------
// issue #283 (a) — get_impact MCP envelope carries the structured count
// ---------------------------------------------------------------------------

/// A receiver call (`self.response_of(...)`) is exactly the shape #283
/// documents the static resolver as never binding: the parser extracts the
/// `CallSite` naming `response_of`, but `resolve_single_call` cannot resolve
/// a callee spelling containing `.`, so it stays unresolved. Pre-#283(a),
/// the `get_impact` MCP response for `response_of` showed `callers: []`
/// with no way to tell that apart from a genuinely uncalled method. This
/// test drives the real stdio wire (not the library struct — the other
/// tests in this file already cover that) and pins that the envelope now
/// carries `unresolved_callsites_naming_target` and a matching `next_steps`
/// hint.
const RECEIVER_CALL_FIXTURE: &str = r#"
pub struct TaskSet;

impl TaskSet {
    pub fn response_of(&self, i: i32) -> i32 {
        i
    }

    pub fn is_schedulable(&self) -> bool {
        self.response_of(0) >= 0
    }
}
"#;

/// Confirms the fixture reproduces the documented gap (belt-and-braces: if
/// a future resolver change starts binding `self.` receiver calls, this
/// fails loudly instead of the envelope assertion below silently passing a
/// count of 0 for the wrong reason).
fn assert_fixture_reproduces_the_283_gap(store: &GraphStore) {
    let target = clustering::get_impact(store, "src/lib.rs::TaskSet::response_of")
        .expect("get_impact(response_of)");
    assert!(
        target.callers.is_empty(),
        "fixture assumption violated: static resolver now binds self.response_of(); \
         got callers {:?}",
        target
            .callers
            .iter()
            .map(|n| &n.qualified_name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        target.unresolved_callsites_naming_target, 1,
        "exactly one CallSite (self.response_of) names the target and stays unresolved"
    );
}

/// Pins the issue #283 (a) MCP contract: the structured count and its
/// matching `next_steps` hint, on the real `get_impact` tool response.
fn assert_envelope_exposes_the_unresolved_callsite_hint(response: &Value) {
    assert_eq!(
        response["status"], "ok",
        "unexpected response: {response:?}"
    );
    assert_eq!(
        response["unresolved_callsites_naming_target"], 1,
        "MCP envelope must carry the structured count, got {response:?}"
    );
    let next_steps = response["next_steps"]
        .as_array()
        .unwrap_or_else(|| panic!("next_steps must be an array, got {response:?}"));
    assert!(
        next_steps.iter().any(|s| {
            let s = s.as_str().unwrap_or_default();
            s.contains("call site(s) name this symbol") && s.contains("lsp")
        }),
        "next_steps must hint at lsp_resolve when callers is empty but callsites name the \
         target, got {next_steps:?}"
    );
}

#[test]
fn get_impact_mcp_envelope_exposes_unresolved_callsites_naming_target() {
    let (_tmp_root, store, graph_dir) =
        indexed_and_resolved("stage3c_epistemic_mcp_envelope_", RECEIVER_CALL_FIXTURE);
    assert_fixture_reproduces_the_283_gap(&store);
    // LadybugDB takes an exclusive file lock on open; the server subprocess
    // below opens the same `graph_dir` and would fail with an IO/lock error
    // if this process's handle were still held. Drop it before spawning.
    drop(store);

    let mut server = Server::spawn();
    let response = server.call_tool(
        1,
        "get_impact",
        json!({
            "graph_path": graph_dir.to_string_lossy(),
            "qualified_name": "src/lib.rs::TaskSet::response_of",
        }),
    );
    assert_envelope_exposes_the_unresolved_callsite_hint(&response);
}
