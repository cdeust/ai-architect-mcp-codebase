//! Issue #357 over the real stdio wire: inside a Cargo test, bench, example or
//! binary target, `crate` names that target, not the library. A receiver typed
//! through `use crate::X` keeps only candidates of the target the file belongs
//! to, so a root re-export of an external type no longer reaches the library's
//! namesake, while a test crate's own types still resolve. Workspace members and
//! a cargo-less tree are covered too (the second with the owner's decision: no
//! Cargo facts keeps today's behaviour).
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against an older build, where the external
//! re-export gets an edge and these tests must FAIL.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn cargo_available() -> bool {
    Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
}

/// A type named `name` with an `answer` method.
fn owner(name: &str) -> String {
    format!("pub struct {name};\n\nimpl {name} {{\n    pub fn answer(&self) -> u32 {{\n        1\n    }}\n}}\n")
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

fn write_all(repo: &Path, files: &[(&str, String)]) {
    for (rel, text) in files {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// Target ids of the per-site rows of the call `<callee>`.
fn targets(graph: &Path, callee: &str) -> Vec<String> {
    let store = GraphStore::open_or_create(graph).unwrap();
    let mut rows: Vec<String> = store
        .execute_query(&format!(
            "MATCH (c:CallSite)-[r:Calls_CallSite_Method]->(m:Method) \
             WHERE c.callee_name = '{callee}' RETURN m.qualified_name"
        ))
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    rows.sort();
    rows
}

/// Analyzes `files` in a fresh directory; the graph path and its guard.
fn analyzed(files: &[(&str, String)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    write_all(&repo, files);
    Server::spawn().analyze(&repo, &out);
    (tmp, out.join("graph"))
}

/// `use crate::<path>;` in a module, then a free function returning `ty` and a
/// call through it, named after `var`.
fn user_module(module: &str, path: &str, ty: &str, var: &str) -> String {
    format!(
        "mod {module} {{\n    use crate::{path};\n\n    pub fn make_{var}() -> {ty} {{\n        todo!()\n    }}\n\n    \
         pub fn run_{var}() -> u32 {{\n        let {var} = make_{var}();\n        {var}.answer()\n    }}\n}}\n"
    )
}

/// The fixture of the issue: the test crate's root re-exports an external `Set`
/// and a module reaches it through `crate::Set`.
#[test]
fn a_root_re_export_of_an_external_type_gets_no_edge_to_the_library() {
    if !cargo_available() {
        return;
    }
    let (_tmp, graph) = analyzed(&[
        ("Cargo.toml", manifest("fx6")),
        ("src/lib.rs", owner("Set")),
        (
            "tests/it.rs",
            format!(
                "pub use some_ext::Set;\n\n{}",
                user_module("inner", "Set", "Set", "s")
            ),
        ),
    ]);
    assert!(
        targets(&graph, "s.answer").is_empty(),
        "{:?}",
        targets(&graph, "s.answer")
    );
}

/// The same shape in a binary target's root.
#[test]
fn a_bin_root_re_export_of_an_external_type_gets_no_edge_to_the_library() {
    if !cargo_available() {
        return;
    }
    let (_tmp, graph) = analyzed(&[
        ("Cargo.toml", manifest("fx6")),
        ("src/lib.rs", owner("Set")),
        (
            "src/bin/x.rs",
            format!(
                "pub use ext::Set;\n\n{}\nfn main() {{}}\n",
                user_module("inner", "Set", "Set", "s")
            ),
        ),
    ]);
    assert!(targets(&graph, "s.answer").is_empty());
}

/// A test crate's own types, reached through `crate::X` and through
/// `crate::common::X`, resolve to its own definitions, even though the library
/// defines namesakes.
#[test]
fn a_test_crate_resolves_to_its_own_types() {
    if !cargo_available() {
        return;
    }
    let (_tmp, graph) = analyzed(&[
        ("Cargo.toml", manifest("fx6")),
        ("src/lib.rs", format!("{}\n{}", owner("Set"), owner("Tool"))),
        (
            "tests/it.rs",
            format!(
                "mod common;\n\n{}\n{}\n{}",
                owner("Set"),
                user_module("inner", "Set", "Set", "s"),
                user_module("deep", "common::Tool", "Tool", "t"),
            ),
        ),
        ("tests/common/mod.rs", owner("Tool")),
    ]);
    assert_eq!(
        targets(&graph, "s.answer"),
        vec!["tests/it.rs::Set::answer"]
    );
    assert_eq!(
        targets(&graph, "t.answer"),
        vec!["tests/common/mod.rs::Tool::answer"]
    );
}

/// A test crate whose root imports the type from the library of the repository
/// reaches the library through `crate::X`.
#[test]
fn a_root_import_from_the_library_reaches_the_library() {
    if !cargo_available() {
        return;
    }
    let (_tmp, graph) = analyzed(&[
        ("Cargo.toml", manifest("fx6")),
        ("src/lib.rs", owner("Set")),
        (
            "tests/it.rs",
            format!(
                "pub use fx6::Set;\n\n{}",
                user_module("inner", "Set", "Set", "s")
            ),
        ),
    ]);
    assert_eq!(targets(&graph, "s.answer"), vec!["src/lib.rs::Set::answer"]);
}

/// The library itself is unchanged: `crate::shapes::Set` resolves.
#[test]
fn a_library_file_keeps_its_crate_import() {
    if !cargo_available() {
        return;
    }
    let (_tmp, graph) = analyzed(&[
        ("Cargo.toml", manifest("fx6")),
        (
            "src/lib.rs",
            format!(
                "pub mod shapes;\n\n{}",
                user_module("inner", "shapes::Set", "Set", "s")
            ),
        ),
        ("src/shapes.rs", owner("Set")),
    ]);
    assert_eq!(
        targets(&graph, "s.answer"),
        vec!["src/shapes.rs::Set::answer"]
    );
}

/// No Cargo.toml: no target is known, so nothing restricts the lookup, as
/// before (owner's decision: the rule only guards targets cargo reports).
#[test]
fn a_cargo_less_tree_keeps_its_crate_import() {
    let (_tmp, graph) = analyzed(&[
        (
            "lib.rs",
            format!(
                "pub mod shapes;\n\n{}",
                user_module("inner", "shapes::Set", "Set", "s")
            ),
        ),
        ("shapes.rs", owner("Set")),
    ]);
    assert_eq!(targets(&graph, "s.answer"), vec!["shapes.rs::Set::answer"]);
}

/// Two workspace members: `use member_b::Set` from member_a resolves; after
/// member_b is renamed, it declines on the incremental path and on a fresh full
/// index alike (issue #358).
#[test]
fn a_workspace_member_import_follows_the_member_name() {
    if !cargo_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    let user = "use member_b::Set;\n\npub fn make() -> Set {\n    todo!()\n}\n\npub fn run() -> u32 {\n    let x = make();\n    x.answer()\n}\n";
    write_all(
        &repo,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n".into(),
            ),
            ("a/Cargo.toml", manifest("member_a")),
            ("a/src/lib.rs", user.into()),
            ("b/Cargo.toml", manifest("member_b")),
            ("b/src/lib.rs", owner("Set")),
        ],
    );
    let mut server = Server::spawn();
    server.analyze(&repo, &out);
    assert_eq!(
        targets(&out.join("graph"), "x.answer"),
        vec!["b/src/lib.rs::Set::answer"]
    );

    write_all(&repo, &[("b/Cargo.toml", manifest("member_c"))]);
    write_all(
        &repo,
        &[(
            "b/src/lib.rs",
            format!("{}\npub fn added() {{}}\n", owner("Set")),
        )],
    );
    let indexed = server.call_tool("index_codebase", json!({"path": repo, "output_dir": out}));
    assert_eq!(indexed["mode"], "incremental", "{indexed}");
    server.call_tool("resolve_graph", json!({"graph_path": out.join("graph")}));
    assert!(
        targets(&out.join("graph"), "x.answer").is_empty(),
        "incremental"
    );

    let fresh = tmp.path().join("fresh");
    server.analyze(&repo, &fresh);
    assert!(targets(&fresh.join("graph"), "x.answer").is_empty(), "full");
}
