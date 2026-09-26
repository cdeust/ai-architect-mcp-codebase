//! Issue #358: a receiver typed through `use <crate>::X` is accepted only when
//! the Cargo facts recorded by the LATEST index pass name that crate as a library
//! of the repository. Before, the first pass that accepted it rewrote the hint,
//! and an incremental pass after a crate rename kept the edge for every file it
//! did not reparse, while a fresh full index of the same tree declined it.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against an older release, where the incremental
//! graph keeps the row and these tests must FAIL.
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "pub struct Set;\n\nimpl Set {\n    pub fn answer(&self) -> u32 {\n        1\n    }\n}\n\npub fn touch() -> u32 {\n    0\n}\n";

const TEST: &str = "use old_name::Set;\n\nfn make() -> Set {\n    old_name::Set\n}\n\n#[test]\nfn t() {\n    let s = make();\n    s.answer();\n}\n";

fn cargo_available() -> bool {
    Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
}

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

    fn analyze(&mut self, repo: &Path, out: &Path) {
        let analysis = self.call_tool(
            "analyze_codebase",
            json!({"path": repo, "output_dir": out, "language": "rust",
                   "dependency_scope": "none", "lsp": false}),
        );
        assert_eq!(analysis["status"], "ok", "{analysis}");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write(repo: &Path, rel: &str, text: &str) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `(site id, target id, resolution_method, confidence)` of every per-site row
/// of an `answer` call.
fn answer_rows(graph: &Path) -> Vec<Vec<String>> {
    let store = GraphStore::open_or_create(graph).unwrap();
    let mut rows = store
        .execute_query(
            "MATCH (c:CallSite)-[r:Calls_CallSite_Method]->(m:Method) \
             WHERE c.callee_name ENDS WITH 'answer' \
             RETURN c.id, m.qualified_name, r.resolution_method, r.confidence",
        )
        .unwrap()
        .rows;
    rows.sort();
    rows
}

#[test]
fn an_incremental_pass_after_a_crate_rename_agrees_with_a_fresh_full_index() {
    if !cargo_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    write(&repo, "Cargo.toml", &manifest("old_name"));
    write(&repo, "src/lib.rs", LIB);
    write(&repo, "tests/it.rs", TEST);
    let mut server = Server::spawn();
    server.analyze(&repo, &out);
    let before = answer_rows(&out.join("graph"));
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0][1], "src/lib.rs::Set::answer");
    assert_eq!(before[0][2], "receiver-return-type");

    // The rename touches only Cargo.toml and src/lib.rs: tests/it.rs, which
    // names the old crate, is not reparsed.
    write(&repo, "Cargo.toml", &manifest("new_name"));
    write(
        &repo,
        "src/lib.rs",
        &format!("{LIB}\npub fn added() -> u32 {{\n    2\n}}\n"),
    );
    let indexed = server.call_tool("index_codebase", json!({"path": repo, "output_dir": out}));
    assert_eq!(indexed["mode"], "incremental", "{indexed}");
    let resolved = server.call_tool("resolve_graph", json!({"graph_path": out.join("graph")}));
    assert!(resolved.get("error").is_none(), "{resolved}");
    let incremental = answer_rows(&out.join("graph"));

    let fresh = tmp.path().join("fresh");
    server.analyze(&repo, &fresh);
    let full = answer_rows(&fresh.join("graph"));
    assert!(
        full.is_empty(),
        "a fresh index declines the old crate name: {full:?}"
    );
    assert_eq!(
        incremental, full,
        "the graph depends on the order of the runs"
    );
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    graph: PathBuf,
}

impl Project {
    fn indexed(files: &[(&str, String)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("fixture");
        for (rel, text) in files {
            write(&root, rel, text);
        }
        let graph = tmp.path().join("graph");
        indexer::index_codebase(&root, &graph).expect("index");
        let manifest_path = tmp.path().join("manifest.json");
        indexer::write_full_manifest(&root, &manifest_path, &indexer::IndexOptions::default())
            .expect("manifest");
        Project {
            _tmp: tmp,
            root,
            graph,
        }
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).unwrap()
    }

    fn incremental(&self) -> Result<(), String> {
        let manifest_path = self.graph.parent().unwrap().join("manifest.json");
        let prior = indexer::manifest::load(&manifest_path).expect("manifest loads");
        indexer::index_incremental(
            &self.root,
            &self.graph,
            &manifest_path,
            &indexer::IndexOptions::default(),
            &prior,
        )
        .map(|_| ())
    }
}

/// A graph written before the evidence was recorded may hold a hint an earlier
/// pass rewrote as accepted: an incremental refresh would keep it, so it is
/// refused with the remedy, and so is a library full index over it.
#[test]
fn a_graph_without_the_crate_evidence_form_is_refused_for_writes() {
    let p = Project::indexed(&[
        ("Cargo.toml", manifest("mine")),
        ("src/lib.rs", LIB.to_string()),
    ]);
    let store = p.store();
    store
        .require_crate_evidence_metadata()
        .expect("a fresh graph has the form");
    store
        .execute_query("MATCH (m:GraphMarker {id: 'crate_evidence_form'}) DELETE m")
        .expect("drop the form row");
    drop(store);
    let refused = p
        .incremental()
        .expect_err("an incremental write is refused");
    assert!(refused.contains("crate evidence"), "{refused}");
    assert!(refused.contains("full reindex required"), "{refused}");
    let full = match indexer::index_codebase(&p.root, &p.graph) {
        Ok(_) => panic!("a full index over an old graph must be refused"),
        Err(message) => message,
    };
    assert!(full.contains("full reindex required"), "{full}");
}

/// The reset opens only the rows of the return-type tier: a receiver bound at
/// its `let` keeps its row, and only the return-type site is decided again.
#[test]
fn the_reset_removes_only_return_type_rows() {
    let lib = format!(
        "{LIB}\npub fn make() -> Set {{\n    Set\n}}\n\npub fn by_return() -> u32 {{\n    let a = make();\n    a.answer()\n}}\n\npub fn by_binding() -> u32 {{\n    let b: Set = Set;\n    b.answer()\n}}\n"
    );
    let p = Project::indexed(&[("Cargo.toml", manifest("mine")), ("src/lib.rs", lib)]);
    let store = p.store();
    resolver::resolve_graph(&store).expect("resolve");
    let methods = |store: &GraphStore| {
        let mut rows = store
            .execute_query(
                "MATCH (c:CallSite)-[r:Calls_CallSite_Method]->() \
                 WHERE c.callee_name ENDS WITH '.answer' RETURN c.callee_name, r.resolution_method",
            )
            .unwrap()
            .rows;
        rows.sort();
        rows
    };
    let pair = |a: &str, b: &str| vec![a.to_string(), b.to_string()];
    assert_eq!(
        methods(&store),
        vec![
            pair("a.answer", "receiver-return-type"),
            pair("b.answer", "receiver-local-binding")
        ]
    );
    store
        .reset_call_rows("receiver-return-type")
        .expect("reset");
    assert_eq!(
        methods(&store),
        vec![pair("b.answer", "receiver-local-binding")]
    );
    let open = store
        .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'a.answer' RETURN c.is_resolved")
        .unwrap()
        .rows;
    assert_eq!(open, vec![vec!["false".to_string()]]);
}
