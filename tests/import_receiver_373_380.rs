//! Issues #373 and #380 over the real stdio wire: a receiver whose type the
//! caller's module names through a `use` resolves to the owner that `use` names.
//!
//! - #380: `use b::Set;` next to an inline `mod a { struct Set }` of the same
//!   file used to resolve `s.m()` to `a::Set::m` through the same-file tiebreak.
//! - #373: inside the library, `use crate::Set;` used to reach any `Set` of the
//!   repository even when the root re-exports an external `Set`
//!   (`pub use ext::Set;`). A re-export is now followed: to a module of the
//!   repository, it names that module's type; to anything else, no edge.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against a build without the fix, where these
//! tests must FAIL.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// `Set` with `new` and `m`, returning `n` from `m`.
fn set_type(indent: &str, n: u8) -> String {
    format!(
        "{indent}pub struct Set;\n{indent}impl Set {{\n{indent}    pub fn new() -> Self {{\n\
         {indent}        Set\n{indent}    }}\n{indent}    pub fn m(&self) -> u8 {{\n\
         {indent}        {n}\n{indent}    }}\n{indent}}}\n"
    )
}

fn package(name: &str) -> String {
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
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Writes `files` under a fresh repo, analyzes it (static pass only) and opens
/// the graph. The temp dir and the server live as long as the returned tuple.
fn analyzed(files: &[(&str, String)]) -> (tempfile::TempDir, Server, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    for (path, text) in files {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    let store = GraphStore::open_or_create(&out.join("graph")).unwrap();
    (tmp, server, store)
}

fn line_of(text: &str, marker: &str) -> usize {
    text.lines()
        .position(|l| l.contains(&format!("// {marker}")))
        .unwrap_or_else(|| panic!("no line holds // {marker}"))
        + 1
}

/// Target ids of the `.m()` call on the line of `file` marked `marker`.
fn targets(store: &GraphStore, file: &str, text: &str, marker: &str) -> Vec<String> {
    let line = line_of(text, marker);
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.line = {line} \
             AND cs.callee_name ENDS WITH '.m' AND cs.id STARTS WITH '{file}::' RETURN t.id"
        ))
        .unwrap_or_else(|e| panic!("query {marker}: {e}"))
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect()
}

fn issue_380_lib() -> String {
    format!(
        "mod b;
use b::Set;

pub mod a {{
{}}}

pub fn bind(s: &Set) -> u8 {{
    s.m() // bind
}}

pub fn assoc() -> u8 {{
    let s = Set::new();
    s.m() // assoc
}}

pub mod g {{
    use crate::b::Set as S;
    pub fn aliased() -> u8 {{
        let s = S::new();
        s.m() // alias
    }}
}}

pub mod h {{
    use super::a::Set;
    pub fn up(s: &Set) -> u8 {{
        s.m() // super-import
    }}
}}

pub mod x {{
    use ext::Set;
    pub fn foreign(s: &Set) -> u8 {{
        s.m() // foreign
    }}
}}
",
        set_type("    ", 1)
    )
}

#[test]
fn a_use_of_another_module_beats_the_same_file_namesake() {
    let lib = issue_380_lib();
    let (_tmp, _server, store) = analyzed(&[
        ("Cargo.toml", package("im380")),
        ("src/lib.rs", lib.clone()),
        ("src/b.rs", set_type("", 2)),
    ]);
    for marker in ["bind", "assoc", "alias"] {
        assert_eq!(
            targets(&store, "src/lib.rs", &lib, marker),
            ["src/b.rs::Set::m"],
            "{marker}"
        );
    }
    assert_eq!(
        targets(&store, "src/lib.rs", &lib, "super-import"),
        ["src/lib.rs::a::Set::m"]
    );
    assert!(
        targets(&store, "src/lib.rs", &lib, "foreign").is_empty(),
        "a type imported from a crate outside the repository gets no edge"
    );
}

/// A module of the library that shows its type through `use crate::Set`, both
/// on a binding and on a return type.
const USER: &str = "pub mod user {
    use crate::Set;
    pub fn bind(s: &Set) -> u8 {
        s.m() // crate-bind
    }
    pub fn make() -> Set {
        todo!()
    }
    pub fn ret() -> u8 {
        let s = make();
        s.m() // crate-ret
    }
}
";

#[test]
fn a_root_re_export_of_an_external_type_gets_no_edge() {
    let lib = format!("pub use ext::Set;\nmod other;\n\n{USER}");
    let (_tmp, _server, store) = analyzed(&[
        ("Cargo.toml", package("im373")),
        ("src/lib.rs", lib.clone()),
        ("src/other.rs", set_type("", 1)),
    ]);
    assert!(targets(&store, "src/lib.rs", &lib, "crate-bind").is_empty());
    assert!(targets(&store, "src/lib.rs", &lib, "crate-ret").is_empty());
}

#[test]
fn a_root_re_export_of_a_module_resolves_into_that_module() {
    let lib = format!(
        "mod task;\npub use task::Set;\n\npub mod decoy {{\n{}}}\n\n{USER}",
        set_type("    ", 1)
    );
    let it = format!(
        "use im373b::Set;\n\nmod helper {{\n{}}}\n\n#[test]\nfn t() {{\n    let s = Set::new();\n    \
         assert_eq!(s.m(), 2); // from-test\n}}\n",
        set_type("    ", 3)
    );
    let (_tmp, _server, store) = analyzed(&[
        ("Cargo.toml", package("im373b")),
        ("src/lib.rs", lib.clone()),
        ("src/task.rs", set_type("", 2)),
        ("tests/it.rs", it.clone()),
    ]);
    for marker in ["crate-bind", "crate-ret"] {
        assert_eq!(
            targets(&store, "src/lib.rs", &lib, marker),
            ["src/task.rs::Set::m"],
            "{marker}"
        );
    }
    assert_eq!(
        targets(&store, "tests/it.rs", &it, "from-test"),
        ["src/task.rs::Set::m"],
        "`use <lib>::Set` follows the library root's re-export"
    );
}

#[test]
fn a_crate_name_import_reaches_a_type_the_library_root_defines() {
    // The dy-wcet shape: `use dy_wcet::{.., TaskSet}` in an integration test.
    let it = format!(
        "use im380c::Set;\n\nmod helper {{\n{}}}\n\n#[test]\nfn t() {{\n    let s = Set::new();\n    \
         assert_eq!(s.m(), 1); // root-type\n}}\n",
        set_type("    ", 3)
    );
    let (_tmp, _server, store) = analyzed(&[
        ("Cargo.toml", package("im380c")),
        ("src/lib.rs", set_type("", 1)),
        ("tests/it.rs", it.clone()),
    ]);
    assert_eq!(
        targets(&store, "tests/it.rs", &it, "root-type"),
        ["src/lib.rs::Set::m"]
    );
}

#[test]
fn a_cargo_less_tree_follows_a_module_import_and_keeps_a_crate_name_import() {
    let lib = issue_380_lib();
    let (_tmp, _server, store) = analyzed(&[
        ("src/lib.rs", lib.clone()),
        ("src/b.rs", set_type("", 2)),
    ]);
    assert_eq!(
        targets(&store, "src/lib.rs", &lib, "bind"),
        ["src/b.rs::Set::m"]
    );
    // Without Cargo facts, `somecrate` may be this very crate: the lookup stays
    // as it was before this change and keeps its edge.
    let other = "use somecrate::Set;\n\npub fn run() -> u8 {\n    let s = Set::new();\n    \
                 s.m() // unknown-root\n}\n"
        .to_string();
    let (_tmp2, _server2, store2) = analyzed(&[
        ("src/lib.rs", format!("mod other;\n{}", set_type("", 1))),
        ("src/other.rs", other.clone()),
    ]);
    assert_eq!(
        targets(&store2, "src/other.rs", &other, "unknown-root"),
        ["src/lib.rs::Set::m"]
    );
}
