//! Issue #398 over the real stdio wire: a call written with a path names the
//! owner the path names. `a::dup()` next to a crate-root `dup` was left
//! ambiguous, because the lookup by name compared `a::dup` with qualified
//! names built from file paths (`src/a.rs::dup`); and `Set::new()` in a module
//! whose only way to see a `Set` is `use ext::*` resolved to the crate's own
//! `Set::new`, a namesake.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against a release built before the fix.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "use ext::*;
pub mod a;
pub mod b;
pub mod c;
pub mod d;
#[path = \"placed.rs\"]
pub mod moved;
mod inner {
    pub fn dup() -> u32 {
        2
    }
}

pub struct Set;

#[derive(Default)]
pub struct Opts;

pub struct Other;

impl Default for Other {
    fn default() -> Self {
        Other
    }
}

impl Set {
    pub fn new() -> Set {
        Set
    }
}

pub fn dup() -> u32 {
    0
}

pub fn root_calls() -> u32 {
    let x = a::dup(); // relative
    let y = crate::a::dup(); // crate
    let z = self::a::dup(); // self
    let w = crate::dup(); // root
    let v = inner::dup(); // inline
    let _s = Set::new(); // own-type
    let m = moved::tick(); // moved
    let _o = Opts::default(); // derived
    let e = Set::extra(); // impl-elsewhere
    x + y + z + w + v + m + e
}
";

/// The file `mod moved` really is.
const PLACED: &str = "pub fn tick() -> u32 {
    3
}
";

/// A file at the location `moved` would have without `#[path]`: no target
/// compiles it.
const STRAY: &str = "pub fn tick() -> u32 {
    4
}
";

const A: &str = "pub fn dup() -> u32 {
    1
}

impl crate::Set {
    pub fn extra() -> u32 {
        5
    }
}

pub fn up() -> u32 {
    super::dup() // super
}
";

const B: &str = "use crate::a;

pub fn via_use() -> u32 {
    a::dup() // use
}
";

/// A module that sees `a` only through `use super::*`, in a root that also
/// globs a foreign crate: the root's own `mod a` is what `a` names.
const D: &str = "use super::*;

pub fn via_super() -> u32 {
    a::dup() // super-glob
}
";

const C: &str = "use ext::*;

pub fn through_glob() {
    let _s = Set::new(); // glob
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

/// The crate of the issue, analyzed with the static pass only.
fn analyzed() -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"probe398\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    for (file, text) in [
        ("lib.rs", LIB),
        ("a.rs", A),
        ("b.rs", B),
        ("c.rs", C),
        ("d.rs", D),
        ("placed.rs", PLACED),
        ("moved.rs", STRAY),
    ] {
        std::fs::write(repo.join("src").join(file), text).unwrap();
    }
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    let store = GraphStore::open_or_create(&out.join("graph")).unwrap();
    (tmp, store)
}

/// The targets of the per-site rows of the call marked `marker` in `file`.
fn targets(store: &GraphStore, file: &str, marker: &str) -> Vec<String> {
    let text = [
        ("lib.rs", LIB),
        ("a.rs", A),
        ("b.rs", B),
        ("c.rs", C),
        ("d.rs", D),
    ]
    .iter()
    .find(|(f, _)| *f == file)
    .map(|(_, t)| *t)
    .unwrap();
    let line = text
        .lines()
        .position(|l| l.contains(&format!("// {marker}")))
        .unwrap_or_else(|| panic!("no line holds // {marker}"))
        + 1;
    let mut out = Vec::new();
    for table in ["Calls_CallSite_Function", "Calls_CallSite_Method"] {
        let rows = store
            .execute_query(&format!(
                "MATCH (cs:CallSite)-[r:{table}]->(t) WHERE cs.line = {line} \
                 AND cs.id STARTS WITH 'src/{file}' RETURN t.id"
            ))
            .unwrap_or_else(|e| panic!("query {marker}: {e}"))
            .rows;
        out.extend(rows.into_iter().map(|r| r[0].clone()));
    }
    out
}

#[test]
fn a_module_path_names_the_function_of_that_module() {
    let (_tmp, store) = analyzed();
    for marker in ["relative", "crate", "self"] {
        assert_eq!(
            targets(&store, "lib.rs", marker),
            vec!["src/a.rs::dup".to_string()],
            "{marker}"
        );
    }
    assert_eq!(targets(&store, "lib.rs", "root"), vec!["src/lib.rs::dup"]);
    assert_eq!(
        targets(&store, "lib.rs", "inline"),
        vec!["src/lib.rs::inner::dup"]
    );
    assert_eq!(targets(&store, "a.rs", "super"), vec!["src/lib.rs::dup"]);
    assert_eq!(targets(&store, "b.rs", "use"), vec!["src/a.rs::dup"]);
    assert_eq!(targets(&store, "d.rs", "super-glob"), vec!["src/a.rs::dup"]);
}

/// `(unresolved_reason, unresolved_detail)` of the call on `line` of `file`.
fn reason(store: &GraphStore, file: &str, line: usize) -> (String, String) {
    let rows = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.line = {line} AND cs.id STARTS WITH 'src/{file}' \
             RETURN cs.unresolved_reason, cs.unresolved_detail"
        ))
        .unwrap()
        .rows;
    (rows[0][0].clone(), rows[0][1].clone())
}

#[test]
fn a_type_path_names_what_the_repository_gives_that_type() {
    let (_tmp, store) = analyzed();
    // `Opts::default` is derived: the repository defines no `default` of an
    // `Opts`, so the call must not take `Other::default`.
    assert_eq!(targets(&store, "lib.rs", "derived"), Vec::<String>::new());
    let line = LIB.lines().position(|l| l.contains("// derived")).unwrap() + 1;
    assert_eq!(
        reason(&store, "lib.rs", line),
        ("declined_by_scope".to_string(), "written_path".to_string())
    );
    // An `impl` in another module keeps its method reachable by the path.
    assert_eq!(
        targets(&store, "lib.rs", "impl-elsewhere"),
        vec!["src/a.rs::crate::Set::extra"]
    );
}

#[test]
fn a_path_never_names_a_file_no_target_compiles() {
    let (_tmp, store) = analyzed();
    let found = targets(&store, "lib.rs", "moved");
    assert!(
        !found.iter().any(|t| t.starts_with("src/moved.rs")),
        "{found:?}"
    );
}

#[test]
fn a_type_seen_only_through_a_foreign_glob_is_not_the_crate_s_namesake() {
    let (_tmp, store) = analyzed();
    assert_eq!(targets(&store, "c.rs", "glob"), Vec::<String>::new());
    let line = C.lines().position(|l| l.contains("// glob")).unwrap() + 1;
    assert_eq!(
        reason(&store, "c.rs", line),
        ("declined_by_scope".to_string(), "written_path".to_string())
    );
    assert_eq!(
        targets(&store, "lib.rs", "own-type"),
        vec!["src/lib.rs::Set::new"]
    );
}
