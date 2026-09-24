// macro_data_344_345 — issues #344 and #345.
//
// #344: the Rust macro table named callees it had not verified. `println!`
// listed `Arguments::new_v1`, an internal the compiler no longer emits, and the
// whole assert family and `panic!` listed one `core::panicking` function
// although the callee is `panic` or `panic_fmt` by the arguments and
// `assert_failed` only for the comparison forms. Only callees checked in the
// std sources of the toolchain (and identical in the expansion of 1.93 to 1.98)
// stay; a site with no one callee is unresolved and says why.
//
// #345: `matches!`, `include_str!` and their kind call nothing, yet counted as
// unresolved references and lowered `resolution_rate`. They are not references.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::resolver;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
mod common;
mod macro_339_support;
use macro_339_support::{index_files, is_resolved, macro_rows, reason_on_line, rows_on_line};

const NO_STABLE: &str = "no stable target for this expansion";

/// Line numbers below are 1-based lines of this fixture.
const LIB: &str = "pub fn prints(a: u32) {
    println!(\"a {}\", a);
    eprintln!(\"a {}\", a);
}

pub fn comparisons(a: u32, b: u32) {
    assert_eq!(a, b);
    assert_ne!(a, b);
    debug_assert_ne!(a, b);
}

pub fn by_arguments(a: u32, b: u32) {
    assert!(a == b);
    debug_assert!(a == b);
    if a > 1 { panic!(\"x\"); }
    if a > 2 { todo!(); }
    if a > 3 { unimplemented!(); }
    if a > 4 { unreachable!(); }
}
";

const NO_CALLS: &str = "fn helper(x: u32) -> u32 { x }

pub fn calls_nothing(a: u32) -> bool {
    let _ = include_str!(\"lib.rs\");
    let _ = concat!(\"a\", \"b\");
    let _ = line!();
    matches!(a, 1)
}

pub fn a_call_in_the_arguments(a: u32) -> bool {
    matches!(helper(a), 1)
}
";

fn count_rows_to(store: &GraphStore, target: &str) -> u64 {
    let qr = store
        .execute_query(&format!(
            "MATCH (s:StdlibSymbol) WHERE s.canonical_path = '{target}' RETURN count(s)"
        ))
        .expect("count symbols");
    qr.rows[0][0].parse().expect("a count")
}

/// `println!` and `eprintln!` list one callee each; the format-arguments
/// constructor of the same expansion is a compiler internal that changed
/// between versions and is no longer a target.
#[test]
fn println_no_longer_lists_the_compiler_internal_new_v1() {
    let (store, _res, _tmp) = index_files(&[("lib.rs", LIB)]);
    let rows = macro_rows(&store, "src/lib.rs");
    let print = rows_on_line(&rows, 2);
    assert_eq!(print.len(), 1, "println! has one callee: {}", print.len());
    assert_eq!(print[0].target, "std::io::_print");
    let eprint = rows_on_line(&rows, 3);
    assert_eq!(eprint.len(), 1);
    assert_eq!(eprint[0].target, "std::io::_eprint");
    assert_eq!(count_rows_to(&store, "std::fmt::Arguments::new_v1"), 0);
}

/// The four comparison macros call `assert_failed` whatever their arguments,
/// `debug_assert_ne!` included (it had no entry).
#[test]
fn the_comparison_asserts_all_reach_assert_failed() {
    let (store, _res, _tmp) = index_files(&[("lib.rs", LIB)]);
    let rows = macro_rows(&store, "src/lib.rs");
    for line in [7, 8, 9] {
        let on = rows_on_line(&rows, line);
        assert_eq!(on.len(), 1, "line {line}");
        assert_eq!(
            on[0].target, "core::panicking::assert_failed",
            "line {line}"
        );
        assert!(is_resolved(&store, "src/lib.rs", line), "line {line}");
    }
}

/// `assert!`, `panic!`, `todo!`, `unimplemented!` and `unreachable!` call
/// `panic` or `panic_fmt` by their arguments: no row, not resolved, and the
/// reason says why. Before, they all got a resolved `core::panicking` target.
#[test]
fn a_macro_whose_callee_depends_on_its_arguments_gets_no_guessed_target() {
    let (store, res, _tmp) = index_files(&[("lib.rs", LIB)]);
    let rows = macro_rows(&store, "src/lib.rs");
    for line in [13, 14, 15, 16, 17, 18] {
        assert!(rows_on_line(&rows, line).is_empty(), "line {line}");
        assert!(!is_resolved(&store, "src/lib.rs", line), "line {line}");
        assert_eq!(
            reason_on_line(&res, "src/lib.rs", line).as_deref(),
            Some(NO_STABLE),
            "line {line}"
        );
    }
    assert_eq!(count_rows_to(&store, "core::panicking::panic"), 0);
}

/// A macro that calls nothing is in no count: not resolved, not unresolved,
/// not in `total_refs`. The same code without those lines has the same
/// `total_refs`, and the sites are reported on their own.
#[test]
fn a_macro_that_calls_nothing_is_not_a_reference() {
    let without = "fn helper(x: u32) -> u32 { x }

pub fn calls_nothing(a: u32) -> bool {
    a == 1
}

pub fn a_call_in_the_arguments(a: u32) -> bool {
    helper(a) == 1
}
";
    let (_s1, with_res, _t1) = index_files(&[("lib.rs", NO_CALLS)]);
    let (_s2, base_res, _t2) = index_files(&[("lib.rs", without)]);
    assert_eq!(with_res.no_call_macro_sites, 5);
    assert_eq!(base_res.no_call_macro_sites, 0);
    assert_eq!(with_res.total_refs, base_res.total_refs, "not references");
    assert_eq!(with_res.total_edges, base_res.total_edges);
    assert!(
        with_res
            .unresolved
            .iter()
            .all(|u| !is_no_call(&u.target_text)),
        "a no-call macro was reported unresolved"
    );
    assert_eq!(
        with_res.total_edges + with_res.unresolved.len() as u64,
        with_res.total_refs
    );
}

fn is_no_call(target: &str) -> bool {
    ["matches!", "include_str!", "concat!", "line!"].contains(&target)
}

/// `matches!(helper(a), 1)` holds a call and a pattern: the call is still a
/// call site of the enclosing function and resolves to `helper`.
#[test]
fn a_call_inside_the_arguments_of_matches_is_still_a_call_site() {
    let (store, _res, _tmp) = index_files(&[("lib.rs", NO_CALLS)]);
    let qr = store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.callee_name = 'helper' \
             RETURN cs.id, cs.is_resolved",
        )
        .expect("query");
    assert_eq!(qr.rows.len(), 1, "one call to helper: {:?}", qr.rows);
    assert!(qr.rows[0][0].contains("a_call_in_the_arguments"));
    assert_eq!(qr.rows[0][1], "true", "the call resolved to helper");
}

/// A second resolve of the same graph finds the same no-call sites.
#[test]
fn a_second_resolve_counts_the_same_no_call_sites() {
    let (store, res, _tmp) = index_files(&[("lib.rs", NO_CALLS)]);
    let again: resolver::ResolutionResult = resolver::resolve_graph(&store).expect("resolve");
    assert_eq!(res.no_call_macro_sites, again.no_call_macro_sites);
    assert_eq!(again.no_call_macro_sites, 5);
}

/// A live MCP server subprocess over its stdio wire; killed on drop.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ai-architect-mcp-codebase"))
            .args(["--profile", "full"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the server");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn call_tool(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                         "params": {"name": name, "arguments": arguments}});
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(&line).expect("a JSON-RPC line");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no content[0].text: {resp:?}"));
        serde_json::from_str(text).expect("tool response is JSON")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `analyze_codebase` and `resolve_graph` carry the counter beside the totals
/// it stays out of.
#[test]
fn the_tools_report_no_call_macro_sites() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (repo, out) = (tmp.path().join("repo"), tmp.path().join("out"));
    std::fs::create_dir_all(repo.join("src")).expect("mkdir");
    std::fs::write(repo.join("src/lib.rs"), NO_CALLS).expect("write");
    let mut server = Server::spawn();
    let analyzed = server.call_tool(
        1,
        "analyze_codebase",
        json!({"path": repo, "output_dir": out, "dependency_scope": "none"}),
    );
    assert_eq!(analyzed["resolve"]["no_call_macro_sites"], 5, "{analyzed}");
    let graph = out.join("graph");
    let resolved = server.call_tool(2, "resolve_graph", json!({ "graph_path": graph }));
    assert_eq!(resolved["no_call_macro_sites"], 5, "{resolved}");
}
