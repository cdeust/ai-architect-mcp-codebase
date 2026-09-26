//! Issue #370 over the real stdio wire: `let x = Type::assoc(..)` typed the
//! receiver as `Type` from the written path alone, without reading what `assoc`
//! returns. When `assoc` returns `Option<Type>`, `Result<Type, E>`, `Box<Type>`,
//! `impl Trait` or another type, the receiver is not a `Type` and the edge to
//! `Type::m` was wrong. The resolver now reads the declared return type of
//! `assoc` and keeps the edge only when it is `Self`, the type itself or the
//! type with generic arguments.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against a build without the fix, where the
//! declines must fail.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "mod other;

use crate::other::Far;

pub struct Set;
impl Set {
    pub fn new() -> Self {
        Set
    }
    pub fn m(&self) -> u8 {
        1
    }
    pub fn inside() -> u8 {
        let s = Self::new();
        s.m() // self-new
    }
}

pub struct Named;
impl Named {
    pub fn new() -> Named {
        Named
    }
    pub fn m(&self) -> u8 {
        2
    }
}

pub struct Gen<T>(T);
impl<T> Gen<T> {
    pub fn new(t: T) -> Gen<T> {
        Gen(t)
    }
    pub fn m(&self) -> u8 {
        3
    }
}

pub struct Opt;
impl Opt {
    pub fn new() -> Option<Opt> {
        None
    }
    pub fn m(&self) -> u8 {
        4
    }
}

pub struct Res;
impl Res {
    pub fn open() -> Result<Res, ()> {
        Err(())
    }
    pub fn m(&self) -> u8 {
        5
    }
}

pub struct Boxed;
impl Boxed {
    pub fn new() -> Box<Boxed> {
        Box::new(Boxed)
    }
    pub fn m(&self) -> u8 {
        6
    }
}

pub struct Shown;
impl Shown {
    pub fn new() -> impl std::fmt::Debug {
        0u8
    }
    pub fn m(&self) -> u8 {
        7
    }
}

pub struct Unrelated;
impl Unrelated {
    pub fn m(&self) -> u8 {
        8
    }
}

pub struct Maker;
impl Maker {
    pub fn make() -> Unrelated {
        Unrelated
    }
    pub fn m(&self) -> u8 {
        9
    }
}

pub enum Resp {
    Refused(u8),
    Met,
}
impl Resp {
    pub fn m(&self) -> u8 {
        10
    }
}

pub fn by_self() -> u8 {
    let s = Set::new();
    s.m() // by-self
}

pub fn by_name() -> u8 {
    let n = Named::new();
    n.m() // by-name
}

pub fn by_generic() -> u8 {
    let g = Gen::new(1u8);
    g.m() // by-generic
}

pub fn by_turbofish() -> u8 {
    let g = Gen::<u8>::new(1);
    g.m() // by-turbofish
}

pub fn by_option() -> u8 {
    let o = Opt::new();
    o.m() // by-option
}

pub fn by_result() -> u8 {
    let r = Res::open();
    r.m() // by-result
}

pub fn by_box() -> u8 {
    let b = Boxed::new();
    b.m() // by-box
}

pub fn by_impl_trait() -> u8 {
    let d = Shown::new();
    d.m() // by-impl
}

pub fn by_other_type() -> u8 {
    let x = Maker::make();
    x.m() // by-other
}

pub fn by_variant() -> u8 {
    let r = Resp::Refused(1);
    r.m() // by-variant
}

pub fn across_files() -> u8 {
    let f = Far::new();
    f.m() // across-files
}
";

const OTHER: &str = "pub struct Far;

impl Far {
    pub fn new() -> Self {
        Far
    }
    pub fn m(&self) -> u8 {
        11
    }
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
    repo: std::path::PathBuf,
    out: std::path::PathBuf,
    server: Server,
}

impl Analyzed {
    fn graph(&self) -> std::path::PathBuf {
        self.out.join("graph")
    }
}

/// The crate of the issue, analyzed by `analyze_codebase` with the static pass only.
fn analyzed() -> Analyzed {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"assoc370\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), LIB).unwrap();
    std::fs::write(repo.join("src/other.rs"), OTHER).unwrap();
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    Analyzed {
        _tmp: tmp,
        repo,
        out,
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
/// `.m()` call on the line marked `marker` of src/lib.rs.
fn rows_of(store: &GraphStore, marker: &str) -> Vec<(String, String, String)> {
    let line = line_of(marker);
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.line = {line} \
             AND cs.callee_name ENDS WITH '.m' AND cs.id STARTS WITH 'src/lib.rs::' \
             RETURN t.id, r.resolution_method, r.confidence"
        ))
        .unwrap_or_else(|e| panic!("query {marker}: {e}"))
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .collect()
}

fn bound(target: &str) -> Vec<(String, String, String)> {
    vec![(
        target.to_string(),
        "receiver-local-binding".to_string(),
        "0.87".to_string(),
    )]
}

#[test]
fn a_constructor_returning_the_type_keeps_its_edge() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph()).unwrap();
    assert_eq!(rows_of(&store, "by-self"), bound("src/lib.rs::Set::m"));
    assert_eq!(rows_of(&store, "by-name"), bound("src/lib.rs::Named::m"));
    assert_eq!(rows_of(&store, "by-generic"), bound("src/lib.rs::Gen::m"));
}

#[test]
fn a_constructor_defined_in_another_file_keeps_its_edge() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph()).unwrap();
    assert_eq!(
        rows_of(&store, "across-files"),
        bound("src/other.rs::Far::m")
    );
}

#[test]
fn an_enum_variant_constructor_types_the_receiver_as_its_enum() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph()).unwrap();
    assert_eq!(rows_of(&store, "by-variant"), bound("src/lib.rs::Resp::m"));
}

#[test]
fn a_function_that_does_not_return_the_type_gives_no_edge() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph()).unwrap();
    for marker in ["by-option", "by-result", "by-box", "by-impl", "by-other"] {
        assert_eq!(rows_of(&store, marker), vec![], "{marker}");
    }
}

/// Measured, not designed: the two forms the issue asked to record.
/// `Gen::<u8>::new(1)` puts a turbofish on the type path and `Self::new()`
/// names no type; neither gets a hint, before or after this change.
#[test]
fn a_turbofish_path_and_self_new_get_no_hint() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph()).unwrap();
    assert_eq!(rows_of(&store, "by-turbofish"), vec![]);
    assert_eq!(rows_of(&store, "self-new"), vec![]);
}

/// A graph written under the previous crate evidence form holds `Type::assoc`
/// hints no pass checked, so it is refused until a full reindex; the MCP full
/// reindex of `index_codebase` rebuilds it and records the current form.
#[test]
fn a_graph_of_the_previous_form_is_rebuilt_by_a_full_reindex() {
    let mut a = analyzed();
    {
        let store = GraphStore::open_or_create(&a.graph()).unwrap();
        store.require_crate_evidence_metadata().expect("current form");
        store
            .execute_query("MATCH (m:GraphMarker {id: 'crate_evidence_form'}) SET m.value = '1'")
            .expect("set the previous form");
        let refused = store
            .require_crate_evidence_metadata()
            .expect_err("form 1 is refused");
        assert!(refused.contains("full reindex required"), "{refused}");
    }
    let reindexed = a.server.call_tool(
        "index_codebase",
        json!({"path": a.repo, "output_dir": a.out, "language": "rust", "full": true}),
    );
    assert_eq!(reindexed["status"], "ok", "{reindexed}");
    let store = GraphStore::open_or_create(&a.graph()).unwrap();
    store
        .require_crate_evidence_metadata()
        .expect("the full reindex records the current form");
}
