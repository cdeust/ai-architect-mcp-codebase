//! Issue #368 over the real stdio wire: a receiver typed by a path written with
//! more than one segment (`let s = b::Set::new();`) reached the resolver as its
//! last segment (`Set`), and the same-file tiebreak picked the caller file's own
//! `a::Set`. The parser now keeps the written path and the resolver keeps only
//! the owner the path names.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against a build without the fix.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "mod b;
mod two;

pub mod a {
    pub struct Set;
    impl Set {
        pub fn new() -> Self {
            Set
        }
        pub fn m(&self) -> u8 {
            1
        }
    }
}

pub mod c {
    pub fn from_super() -> u8 {
        let s = super::a::Set::new();
        s.m() // super-inline
    }
}

pub mod g {
    use crate::b::Set as S;
    pub fn aliased() -> u8 {
        let s = S::new();
        s.m() // alias
    }
}

pub fn from_b() -> u8 {
    let s = b::Set::new();
    s.m() // b-set
}

pub fn two_bindings() -> u8 {
    let s = a::Set::new();
    let first = s.m(); // two-a
    let s = b::Set::new();
    first + s.m() // two-b
}

pub fn from_crate() -> u8 {
    let s = crate::a::Set::new();
    s.m() // crate-a
}

pub fn from_root_super() -> u8 {
    let s = super::a::Set::new();
    s.m() // super-root
}

pub fn annotated(s: &b::Set) -> u8 {
    s.m() // annotated
}

pub fn unresolvable() -> u8 {
    let s = zz::Set::new();
    s.m() // unresolvable
}
";

const B: &str = "pub struct Set;
impl Set {
    pub fn new() -> Self {
        Set
    }
    pub fn m(&self) -> u8 {
        2
    }
}
";

const TWO: &str = "pub mod p {
    pub struct Set;
    impl Set {
        pub fn new() -> Self {
            Set
        }
        pub fn m(&self) -> u8 {
            3
        }
    }
}

pub mod q {
    pub struct Set;
    impl Set {
        pub fn new() -> Self {
            Set
        }
        pub fn m(&self) -> u8 {
            4
        }
    }
}

pub fn inline_q() -> u8 {
    let s = q::Set::new();
    s.m() // inline-q
}
";

const ALIB: &str = "pub struct Set;
impl Set {
    pub fn new() -> Self {
        Set
    }
    pub fn m(&self) -> u8 {
        1
    }
}
";

const APP: &str = "pub struct Set;
impl Set {
    pub fn new() -> Self {
        Set
    }
    pub fn m(&self) -> u8 {
        2
    }
}

pub fn use_lib() -> u8 {
    let s = alib::Set::new();
    s.m() // lib-path
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

/// Writes `files` under a fresh repo, analyzes it (static pass only) and opens
/// the graph. The temp dir and the server live as long as the returned tuple.
fn analyzed(files: &[(&str, &str)]) -> (tempfile::TempDir, Server, GraphStore) {
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
    let store = GraphStore::open_or_create(&graph(&out)).unwrap();
    (tmp, server, store)
}

fn graph(out: &Path) -> PathBuf {
    out.join("graph")
}

fn package(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
}

fn issue_crate() -> (tempfile::TempDir, Server, GraphStore) {
    let manifest = package("wp368");
    analyzed(&[
        ("Cargo.toml", &manifest),
        ("src/lib.rs", LIB),
        ("src/b.rs", B),
        ("src/two.rs", TWO),
    ])
}

fn line_of(text: &str, marker: &str) -> usize {
    text.lines()
        .position(|l| l.contains(&format!("// {marker}")))
        .unwrap_or_else(|| panic!("no line holds // {marker}"))
        + 1
}

/// Target ids of the per-site rows of the `.m()` call on the line of `file`
/// marked `marker`.
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

#[test]
fn a_path_to_another_module_no_longer_resolves_to_the_caller_file_namesake() {
    let (_tmp, _server, store) = issue_crate();
    assert_eq!(
        targets(&store, "src/lib.rs", LIB, "b-set"),
        ["src/b.rs::Set::m"]
    );
    assert_eq!(
        targets(&store, "src/lib.rs", LIB, "annotated"),
        ["src/b.rs::Set::m"]
    );
}

#[test]
fn each_of_two_bindings_resolves_to_the_path_it_writes() {
    let (_tmp, _server, store) = issue_crate();
    assert_eq!(
        targets(&store, "src/lib.rs", LIB, "two-a"),
        ["src/lib.rs::a::Set::m"]
    );
    assert_eq!(
        targets(&store, "src/lib.rs", LIB, "two-b"),
        ["src/b.rs::Set::m"]
    );
}

#[test]
fn two_inline_modules_of_one_file_are_told_apart() {
    let (_tmp, _server, store) = issue_crate();
    assert_eq!(
        targets(&store, "src/two.rs", TWO, "inline-q"),
        ["src/two.rs::q::Set::m"]
    );
}

#[test]
fn crate_and_super_paths_resolve_from_the_module_they_name() {
    let (_tmp, _server, store) = issue_crate();
    assert_eq!(
        targets(&store, "src/lib.rs", LIB, "crate-a"),
        ["src/lib.rs::a::Set::m"]
    );
    assert_eq!(
        targets(&store, "src/lib.rs", LIB, "super-inline"),
        ["src/lib.rs::a::Set::m"]
    );
}

#[test]
fn a_super_from_the_file_root_or_an_unknown_path_declines() {
    let (_tmp, _server, store) = issue_crate();
    assert!(targets(&store, "src/lib.rs", LIB, "super-root").is_empty());
    assert!(targets(&store, "src/lib.rs", LIB, "unresolvable").is_empty());
}

#[test]
fn an_alias_import_gives_no_edge() {
    // `use crate::b::Set as S; S::new()`: the hint is `S`, which names no type.
    // Recorded behaviour, unchanged by #368 (a one-segment hint).
    let (_tmp, _server, store) = issue_crate();
    assert!(targets(&store, "src/lib.rs", LIB, "alias").is_empty());
}

#[test]
fn the_hint_keeps_the_written_path() {
    let (_tmp, _server, store) = issue_crate();
    let line = line_of(LIB, "b-set");
    let rows = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.line = {line} AND cs.id STARTS WITH 'src/lib.rs::' \
             AND cs.callee_name ENDS WITH '.m' RETURN cs.receiver_hint, cs.receiver_hint_via"
        ))
        .unwrap()
        .rows;
    assert_eq!(rows, [["b::Set".to_string(), "assoc:new".to_string()]]);
}

#[test]
fn a_library_crate_path_resolves_into_that_library() {
    let root = "[workspace]\nmembers = [\"alib\", \"app\"]\nresolver = \"2\"\n";
    let app_manifest = format!(
        "{}[dependencies]\nalib = {{ path = \"../alib\" }}\n",
        package("app")
    );
    let alib_manifest = package("alib");
    let (_tmp, _server, store) = analyzed(&[
        ("Cargo.toml", root),
        ("alib/Cargo.toml", &alib_manifest),
        ("alib/src/lib.rs", ALIB),
        ("app/Cargo.toml", &app_manifest),
        ("app/src/lib.rs", APP),
    ]);
    assert_eq!(
        targets(&store, "app/src/lib.rs", APP, "lib-path"),
        ["alib/src/lib.rs::Set::m"]
    );
}
