//! Issue #355 over the real stdio wire: a receiver that is a tuple-struct
//! constructor, a struct literal or `Type::assoc(..)` written in place (or a
//! tuple constructor / struct literal bound by one `let`) was left unresolved by
//! the static pass although the receiver's type is written in the expression.
//!
//! The binary under test is `CARGO_BIN_EXE_*` unless `AP_TEST_BIN` names another
//! one, so the same test runs against the official 0.13.0 release, where the
//! five forms must stay unresolved and these tests must FAIL.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "mod other;

pub struct Tier(pub u8);
pub struct Named {
    pub n: u8,
}
pub struct Solo(pub u8);

impl Tier {
    pub fn new(n: u8) -> Tier {
        Tier(n)
    }
    pub fn join(&self, other: &Tier) -> u8 {
        self.0 + other.0
    }
}

impl Named {
    pub fn get(&self) -> u8 {
        self.n
    }
}

pub fn let_assoc_new() -> u8 {
    let t = Tier::new(1);
    t.join(&Tier(2)) // let-assoc
}

pub fn let_tuple_ctor() -> u8 {
    let t = Tier(1);
    t.join(&Tier(2)) // let-tuple
}

pub fn let_struct_literal() -> u8 {
    let v = Named { n: 3 };
    v.get() // let-literal
}

pub fn direct_tuple_ctor() -> u8 {
    Tier(1).join(&Tier(2)) // direct-tuple
}

pub fn direct_assoc_new() -> u8 {
    Tier::new(1).join(&Tier(2)) // direct-assoc
}

pub fn direct_struct_literal() -> u8 {
    Named { n: 3 }.get() // direct-literal
}

pub fn homonym_in_another_file() -> u8 {
    Solo(1).join() // homonym
}

pub fn shadowed_by_something_else() -> u8 {
    let t = Tier(1);
    let t = other_value();
    t.join(&Tier(2)) // shadowed
}
";

const OTHER: &str = "pub struct Solo(pub u8);

impl Solo {
    pub fn join(&self) -> u8 {
        self.0
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
    graph: std::path::PathBuf,
    server: Server,
}

/// The crate of the issue, analyzed by `analyze_codebase` with the static pass only.
fn analyzed() -> Analyzed {
    analyzed_crate(LIB, Some(OTHER))
}

/// A crate of `lib` (and an optional second file `src/other.rs`), analyzed with
/// the static pass only.
fn analyzed_crate(lib: &str, other: Option<&str>) -> Analyzed {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"shadow350\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), lib).unwrap();
    if let Some(other) = other {
        std::fs::write(repo.join("src/other.rs"), other).unwrap();
    }
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    Analyzed {
        _tmp: tmp,
        graph: out.join("graph"),
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
/// method call on the line marked `marker`.
fn rows_of(store: &GraphStore, marker: &str) -> Vec<(String, String, String)> {
    let line = line_of(marker);
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.line = {line} \
             AND (cs.callee_name ENDS WITH '.join' OR cs.callee_name ENDS WITH '.get') \
             RETURN t.id, r.resolution_method, r.confidence"
        ))
        .unwrap_or_else(|e| panic!("query {marker}: {e}"))
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .collect()
}

fn row(target: &str, method: &str, confidence: &str) -> Vec<(String, String, String)> {
    vec![(
        format!("src/lib.rs::{target}"),
        method.to_string(),
        confidence.to_string(),
    )]
}

fn written(target: &str) -> Vec<(String, String, String)> {
    row(target, "receiver-local-binding", "0.87")
}

fn by_signature(target: &str) -> Vec<(String, String, String)> {
    row(target, "receiver-return-type", "0.85")
}

#[test]
fn the_five_forms_resolve_statically_to_the_method_of_the_written_type() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(rows_of(&store, "let-assoc"), written("Tier::join"));
    assert_eq!(rows_of(&store, "let-tuple"), written("Tier::join"));
    assert_eq!(rows_of(&store, "let-literal"), written("Named::get"));
    assert_eq!(rows_of(&store, "direct-tuple"), written("Tier::join"));
    assert_eq!(rows_of(&store, "direct-assoc"), by_signature("Tier::join"));
    assert_eq!(rows_of(&store, "direct-literal"), written("Named::get"));
}

#[test]
fn a_namesake_type_in_another_file_gets_no_edge() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(rows_of(&store, "homonym"), vec![]);
}

#[test]
fn a_shadowing_let_of_something_else_gets_no_edge() {
    let a = analyzed();
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(rows_of(&store, "shadowed"), vec![]);
}

fn callers_of(a: &mut Analyzed, qualified_name: &str) -> Vec<String> {
    let impact = a.server.call_tool(
        "get_impact",
        json!({"graph_path": a.graph, "qualified_name": qualified_name}),
    );
    impact["callers"]
        .as_array()
        .unwrap_or_else(|| panic!("no callers: {impact}"))
        .iter()
        .map(|c| c["qualified_name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn get_impact_lists_the_four_tier_callers_before_the_language_server_runs() {
    let mut a = analyzed();
    let callers = callers_of(&mut a, "src/lib.rs::Tier::join");
    for expected in [
        "src/lib.rs::let_assoc_new",
        "src/lib.rs::let_tuple_ctor",
        "src/lib.rs::direct_tuple_ctor",
        "src/lib.rs::direct_assoc_new",
    ] {
        assert!(
            callers.iter().any(|c| c == expected),
            "{expected} in {callers:?}"
        );
    }
    for absent in [
        "src/lib.rs::homonym_in_another_file",
        "src/lib.rs::shadowed_by_something_else",
    ] {
        assert!(
            !callers.iter().any(|c| c == absent),
            "{absent} in {callers:?}"
        );
    }
}

#[test]
fn get_impact_lists_the_callers_of_the_struct_literal_receiver() {
    let mut a = analyzed();
    let callers = callers_of(&mut a, "src/lib.rs::Named::get");
    for expected in [
        "src/lib.rs::let_struct_literal",
        "src/lib.rs::direct_struct_literal",
    ] {
        assert!(
            callers.iter().any(|c| c == expected),
            "{expected} in {callers:?}"
        );
    }
}

/// Each source holds exactly ONE candidate that shares only the last segment of
/// its parent with the struct `Tier`, whose own `join` is absent: no edge may
/// go to it (with two look-alikes the result would be ambiguous, which would
/// hide the defect).
#[test]
fn a_look_alike_that_is_not_a_method_of_the_struct_gets_no_edge() {
    let sources = [
        // trait method of the same name in another module
        "pub struct Tier(pub u8);\n\
         pub mod t1 { pub trait Tier { fn join(&self); } }\n\
         pub fn f() { Tier(1).join() }\n",
        // function of a module named like the struct
        "pub struct Tier(pub u8);\n\
         pub mod t2 { pub mod Tier { pub fn join() {} } }\n\
         pub fn f() { Tier(1).join() }\n",
        // inherent impl of a path-qualified type of the same last segment
        "pub struct Tier(pub u8);\n\
         pub mod t3 { impl other::Tier { pub fn join(&self) {} } }\n\
         pub fn f() { Tier(1).join() }\n",
    ];
    for lib in sources {
        let a = analyzed_crate(lib, None);
        let store = GraphStore::open_or_create(&a.graph).unwrap();
        let rows = store
            .execute_query(
                "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) \
                 WHERE cs.callee_name = 'Tier(1).join' RETURN t.id",
            )
            .unwrap()
            .rows;
        assert!(rows.is_empty(), "{lib} gave {rows:?}");
    }
}

fn edges_of(a: &Analyzed, callee: &str) -> Vec<Vec<String>> {
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) \
             WHERE cs.callee_name = '{callee}' RETURN t.id, r.resolution_method"
        ))
        .unwrap()
        .rows
}

/// A method of a trait implemented for the struct in the same file is a method
/// of that struct: it resolves.
#[test]
fn a_trait_impl_method_in_the_same_file_resolves() {
    let lib = "pub struct Tier(pub u8);\n\
        pub trait Join { fn join(&self) -> u8; }\n\
        impl Join for Tier { fn join(&self) -> u8 { self.0 } }\n\
        pub fn f() -> u8 { Tier(1).join() }\n";
    let a = analyzed_crate(lib, None);
    assert_eq!(
        edges_of(&a, "Tier(1).join"),
        vec![vec![
            "src/lib.rs::Tier::join".to_string(),
            "receiver-local-binding".to_string()
        ]]
    );
}

/// The same trait impl in another file: no edge, exactly as before this change
/// (a callee text `Tier(1).join` names no entry by name, so no fallback ever
/// resolved such a site); the language server still resolves it.
#[test]
fn a_trait_impl_method_in_another_file_stays_without_an_edge() {
    let lib = "mod imp;\n\
        pub struct Tier(pub u8);\n\
        pub fn f() -> u8 { Tier(1).join() }\n";
    let imp = "use crate::Tier;\n\
        pub trait Join { fn join(&self) -> u8; }\n\
        impl Join for Tier { fn join(&self) -> u8 { self.0 } }\n";
    let a = analyzed_crate(lib, Some(imp));
    assert!(edges_of(&a, "Tier(1).join").is_empty());
}
