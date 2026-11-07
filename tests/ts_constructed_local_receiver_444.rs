// ts_constructed_local_receiver_444 — issue #444 (first slice): in TypeScript and
// JavaScript, `const w = new Worker(); w.run()` left `w.run` unresolved, because the
// local-receiver typing (`CallSite.receiver_hint`) was produced for Rust and C++ only.
// Real indexer + resolver, no LSP.
//
// Every fixture has two classes that own `run`, so an edge into one of them can only
// come from the receiver's type, never from a name-only match.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

/// Writes `files` under a fresh tempdir's `src/`, indexes and resolves it.
fn index_and_resolve(
    tag: &str,
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix(&format!("ts_ctor_receiver_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = root.join("src");
    for (rel, body) in files {
        let p = src.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve_graph");
    (store, res, root)
}

/// The targets of every `Calls_Function_Method` edge out of the function `caller_qn`.
fn method_calls_from(store: &GraphStore, caller_qn: &str) -> Vec<String> {
    let mut targets: Vec<String> = store
        .execute_query(&format!(
            "MATCH (a:Function)-[:Calls_Function_Method]->(b:Method) \
             WHERE a.qualified_name = '{caller_qn}' RETURN b.qualified_name"
        ))
        .expect("query Calls_Function_Method")
        .rows
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    targets.sort();
    targets
}

const TS: &str = "export class Worker {\n\
    \x20\x20run(): number { return 1; }\n\
    }\n\
    export class Other {\n\
    \x20\x20run(): number { return 2; }\n\
    }\n\
    export function useConst(): number {\n\
    \x20\x20const w = new Worker();\n\
    \x20\x20return w.run();\n\
    }\n\
    export function useLet(): number {\n\
    \x20\x20let o = new Other();\n\
    \x20\x20return o.run();\n\
    }\n\
    export function rebound(): number {\n\
    \x20\x20let w = new Worker();\n\
    \x20\x20w = new Other();\n\
    \x20\x20return w.run();\n\
    }\n\
    export function shadowed(): number {\n\
    \x20\x20const w = new Worker();\n\
    \x20\x20{\n\
    \x20\x20\x20\x20const w = new Other();\n\
    \x20\x20\x20\x20return w.run();\n\
    \x20\x20}\n\
    }\n\
    export function unknownType(): number {\n\
    \x20\x20const w = make();\n\
    \x20\x20return w.run();\n\
    }\n";

#[test]
fn typescript_const_and_let_from_new_bind_to_the_constructed_class() {
    let (store, _res, _root) = index_and_resolve("ts", &[("workers.ts", TS)]);
    assert_eq!(
        method_calls_from(&store, "workers.ts::useConst"),
        vec!["workers.ts::Worker::run".to_string()]
    );
    assert_eq!(
        method_calls_from(&store, "workers.ts::useLet"),
        vec!["workers.ts::Other::run".to_string()]
    );
}

#[test]
fn typescript_receiver_the_function_does_not_prove_stays_unresolved() {
    let (store, _res, _root) = index_and_resolve("ts_neg", &[("workers.ts", TS)]);
    // Reassigned, shadowed in an inner block, or initialised by a call: the
    // receiver's type is not one `new` the function states, so no edge.
    for caller in ["rebound", "shadowed", "unknownType"] {
        assert_eq!(
            method_calls_from(&store, &format!("workers.ts::{caller}")),
            Vec::<String>::new(),
            "{caller} must not bind"
        );
    }
}

const JS: &str = "export class Worker {\n\
    \x20\x20run() { return 1; }\n\
    }\n\
    export class Other {\n\
    \x20\x20run() { return 2; }\n\
    }\n\
    export function useConst() {\n\
    \x20\x20const w = new Worker();\n\
    \x20\x20return w.run();\n\
    }\n";

#[test]
fn javascript_const_from_new_binds_to_the_constructed_class() {
    let (store, _res, _root) = index_and_resolve("js", &[("workers.js", JS)]);
    assert_eq!(
        method_calls_from(&store, "workers.js::useConst"),
        vec!["workers.js::Worker::run".to_string()]
    );
}
