//! Issue #356 over the real stdio wire: a call that names a type (a tuple-struct
//! constructor `Tier(1)`) was flagged `is_resolved = true` and had a symbol-level
//! `Uses_*_Struct` edge, but no per-site row, because no relationship table
//! targeted a Struct. `Calls_CallSite_Struct` holds it now.
use ai_architect_mcp::graph_store::GraphStore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LIB: &str = "pub struct Tier(pub u8);

impl Tier {
    pub fn join(&self, other: &Tier) -> u8 {
        self.0 + other.0
    }
}

pub fn let_ctor() -> u8 {
    let t = Tier(1);
    t.join(&Tier(2))
}

pub fn direct_ctor() -> u8 {
    Tier(1).join(&Tier(2))
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
    repo: PathBuf,
    out: PathBuf,
    graph: PathBuf,
    server: Server,
}

/// A crate made of `files` (path under `src/`, content), analyzed with the
/// static pass only.
fn analyzed(files: &[(&str, &str)]) -> Analyzed {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"ctor356\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    for (path, content) in files {
        std::fs::write(repo.join("src").join(path), content).unwrap();
    }
    let mut server = Server::spawn();
    let analysis = server.call_tool(
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "language": "rust",
               "dependency_scope": "none", "lsp": false}),
    );
    assert_eq!(analysis["status"], "ok", "{analysis}");
    Analyzed {
        graph: out.join("graph"),
        _tmp: tmp,
        repo,
        out,
        server,
    }
}

fn rows(store: &GraphStore, cypher: &str) -> Vec<Vec<String>> {
    store
        .execute_query(cypher)
        .unwrap_or_else(|e| panic!("{cypher}: {e}"))
        .rows
}

fn count(store: &GraphStore, cypher: &str) -> usize {
    rows(store, cypher)[0][0].parse().unwrap()
}

fn struct_rows(store: &GraphStore) -> usize {
    count(
        store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Struct]->(s:Struct) RETURN count(r)",
    )
}

fn per_site_rows(store: &GraphStore) -> usize {
    ["Function", "Method", "StdlibSymbol", "Struct"]
        .iter()
        .map(|t| {
            count(
                store,
                &format!("MATCH (c:CallSite)-[r:Calls_CallSite_{t}]->(x) RETURN count(r)"),
            )
        })
        .sum()
}

fn incremental(a: &mut Analyzed) {
    let index = a.server.call_tool(
        "index_codebase",
        json!({"path": a.repo, "output_dir": a.out, "language": "rust",
               "dependency_scope": "none"}),
    );
    assert_eq!(index["status"], "ok", "{index}");
    // A failed incremental pass falls back to a full rebuild, which would create
    // the table and hide the defect: the mode is part of what is tested.
    assert_eq!(index["mode"], "incremental", "{index}");
    let resolved = a
        .server
        .call_tool("resolve_graph", json!({"graph_path": a.graph}));
    assert!(resolved.get("error").is_none(), "{resolved}");
}

fn drop_struct_table(graph: &Path) {
    let store = GraphStore::open_or_create(graph).unwrap();
    rows(&store, "DROP TABLE Calls_CallSite_Struct");
}

#[test]
fn every_constructor_call_gets_a_per_site_row_naming_the_struct() {
    let a = analyzed(&[("lib.rs", LIB)]);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    let sites = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'Tier' AND c.is_resolved = true RETURN c.id",
    );
    assert_eq!(sites.len(), 4, "the four `Tier(..)` constructor sites");
    let targets = rows(
        &store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Struct]->(s:Struct) RETURN c.id, s.id",
    );
    assert_eq!(targets.len(), 4, "{targets:?}");
    assert!(targets.iter().all(|r| r[1] == "src/lib.rs::Tier"));
}

#[test]
fn per_site_rows_equal_resolved_sites() {
    let a = analyzed(&[("lib.rs", LIB)]);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    let resolved = count(
        &store,
        "MATCH (c:CallSite) WHERE c.is_resolved = true RETURN count(c)",
    );
    assert_eq!(per_site_rows(&store), resolved);
}

#[test]
fn get_impact_on_the_struct_lists_the_functions_that_construct_it() {
    let mut a = analyzed(&[("lib.rs", LIB)]);
    let impact = a.server.call_tool(
        "get_impact",
        json!({"graph_path": a.graph, "qualified_name": "src/lib.rs::Tier"}),
    );
    let users: Vec<String> = impact["users"]
        .as_array()
        .unwrap_or_else(|| panic!("no users: {impact}"))
        .iter()
        .map(|u| u["qualified_name"].as_str().unwrap().to_string())
        .collect();
    for expected in ["src/lib.rs::let_ctor", "src/lib.rs::direct_ctor"] {
        assert!(
            users.iter().any(|u| u == expected),
            "{expected} in {users:?}"
        );
    }
}

#[test]
fn the_new_rows_are_per_site_targets_and_move_no_edge_count() {
    let a = analyzed(&[("lib.rs", LIB)]);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    let with = store.graph_counts().unwrap();
    let n = struct_rows(&store) as u64;
    assert!(n > 0);
    rows(
        &store,
        "MATCH (:CallSite)-[r:Calls_CallSite_Struct]->(:Struct) DELETE r",
    );
    let without = store.graph_counts().unwrap();
    assert_eq!(with.edges, without.edges, "edge_count moved");
    assert_eq!(with.call_site_targets, without.call_site_targets + n);
}

/// `Kind::A(1)` builds the variant `A` of the enum `Kind`; a lone struct `A` in
/// another file must not receive the edge. A path-qualified call to that struct
/// still resolves, so the guard does not over-decline.
#[test]
fn an_enum_variant_call_gets_no_edge_to_a_struct_of_the_same_name() {
    let a = analyzed(&[
        (
            "lib.rs",
            "mod other;\npub enum Kind { A(u8), B }\n\
             pub fn make() -> Kind { Kind::A(1) }\n\
             pub fn control() -> other::A { other::A(1) }\n",
        ),
        ("other.rs", "pub struct A(pub u8);\n"),
    ]);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    let uses = |f: &str| {
        rows(
            &store,
            &format!(
                "MATCH (f:Function)-[r:Uses_Function_Struct]->(s:Struct) \
                 WHERE f.qualified_name = 'src/lib.rs::{f}' AND s.name = 'A' RETURN s.id"
            ),
        )
    };
    assert!(uses("make").is_empty(), "{:?}", uses("make"));
    assert_eq!(uses("control").len(), 1, "the control call must resolve");
    let site_rows = rows(
        &store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Struct]->(s:Struct) \
         WHERE c.callee_name = 'Kind::A' RETURN s.id",
    );
    assert!(site_rows.is_empty(), "{site_rows:?}");
}

#[test]
fn a_graph_without_the_table_is_filled_by_the_next_resolve_pass() {
    let a = analyzed(&[("lib.rs", LIB)]);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    let before = struct_rows(&store);
    assert_eq!(before, 4);
    rows(&store, "DROP TABLE Calls_CallSite_Struct");
    ai_architect_mcp::resolver::resolve_graph(&store).expect("resolve over an old graph");
    assert_eq!(struct_rows(&store), before);
}

#[test]
fn ensure_rel_tables_creates_only_the_missing_table_once() {
    let a = analyzed(&[("lib.rs", LIB)]);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(store.ensure_rel_tables().unwrap(), 0);
    rows(&store, "DROP TABLE Calls_CallSite_Struct");
    assert_eq!(store.ensure_rel_tables().unwrap(), 1);
    assert_eq!(store.ensure_rel_tables().unwrap(), 0);
    assert_eq!(
        struct_rows(&store),
        0,
        "an empty table is the starting point"
    );
}

#[test]
fn an_incremental_refresh_of_a_graph_without_the_table_is_backfilled() {
    let mut a = analyzed(&[("lib.rs", LIB)]);
    drop_struct_table(&a.graph);
    std::fs::write(a.repo.join("src/lib.rs"), format!("{LIB}\n// touched\n")).unwrap();
    incremental(&mut a);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    assert_eq!(struct_rows(&store), 4);
}

#[test]
fn a_constructor_site_reopens_when_the_file_of_its_struct_is_deleted() {
    let mut a = analyzed(&[
        (
            "lib.rs",
            "mod tier;\nuse tier::Tier;\npub fn build() -> Tier { Tier(1) }\n",
        ),
        ("tier.rs", "pub struct Tier(pub u8);\n"),
    ]);
    {
        let store = GraphStore::open_or_create(&a.graph).unwrap();
        let resolved = rows(
            &store,
            "MATCH (c:CallSite) WHERE c.callee_name = 'Tier' AND c.is_resolved = true RETURN c.id",
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(struct_rows(&store), 1);
    }
    std::fs::remove_file(a.repo.join("src/tier.rs")).unwrap();
    incremental(&mut a);
    let store = GraphStore::open_or_create(&a.graph).unwrap();
    let still = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'Tier' AND c.is_resolved = true RETURN c.id",
    );
    assert!(still.is_empty(), "the site kept a resolution: {still:?}");
}
