// held_graph_tools_tests: issue #363. Which tools can return the handle
// refusal of #352 is measured, not kept by hand: a real graph is indexed, a
// running request's cached handle is held on it, and every tool that takes a
// graph is called through the dispatch table. A tool that answers with the
// refusal code must be listed in `HELD_GRAPH_TOOLS` (and so document it), and
// a listed tool must answer with it.

use crate::graph_cache::open_cached;
use crate::tool_profile::ToolProfile;
use crate::tool_schemas::HELD_GRAPH_TOOLS;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fx363\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn f() -> u32 {\n    g()\n}\n\npub fn g() -> u32 {\n    1\n}\n",
    )
    .expect("lib");
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "second",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .args(&args)
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed in the fixture");
    }
}

/// The fixture's paths: the crate, the output dir of the probed graph, that
/// graph, and a second graph for the two-graph tool.
struct Paths {
    code: PathBuf,
    out: PathBuf,
    graph: PathBuf,
    other: PathBuf,
}

/// Arguments that let each graph tool reach the point where it opens the
/// graph. A tool is probed only if it takes a graph.
fn probe_args(name: &str, p: &Paths) -> Option<Value> {
    let other = &p.other;
    let (code, out, graph) = (
        p.code.to_string_lossy(),
        p.out.to_string_lossy(),
        p.graph.to_string_lossy(),
    );
    let symbol = "src/lib.rs::f";
    Some(match name {
        "index_codebase" => json!({ "path": code, "output_dir": out, "language": "rust" }),
        "analyze_codebase" => json!({ "path": code, "output_dir": out, "language": "rust",
            "lsp": false, "dependency_scope": "none" }),
        "index_status" | "resolve_graph" | "cluster_graph" | "get_processes" => {
            json!({ "graph_path": graph })
        }
        "detect_changes" => json!({ "graph_path": graph, "codebase_path": code }),
        "query_graph" => {
            json!({ "graph_path": graph, "query": "MATCH (n:Function) RETURN count(n)" })
        }
        "get_symbol" | "get_impact" | "get_context" => {
            json!({ "graph_path": graph, "qualified_name": symbol })
        }
        "search_codebase" => json!({ "graph_path": graph, "query": "f" }),
        "ingest_traces" => json!({ "graph_path": graph, "traces": [] }),
        "index_history" => json!({ "graph_path": graph, "codebase_path": code }),
        "lsp_resolve" => json!({ "graph_path": graph, "codebase_path": code, "language": "rust" }),
        "prepare_prd_input" => json!({ "graph_path": graph, "output_dir": out,
            "feature_description": "change f" }),
        "validate_prd_against_graph" => {
            let prd = Path::new(&*out).join("prd.md");
            std::fs::write(&prd, "# PRD\n\nTouches `src/lib.rs::f`.\n").expect("prd");
            json!({ "graph_path": graph, "prd_path": prd.to_string_lossy() })
        }
        "check_security_gates" => json!({ "graph_path": graph, "changed_symbols": [symbol] }),
        "verify_semantic_diff" => json!({ "before_graph_path": graph,
            "after_graph_path": other.to_string_lossy() }),
        _ => return None,
    })
}

fn refused(response: &Value) -> bool {
    let text = response.to_string();
    text.contains("graph_handle_in_use") || text.contains("graph_cache_busy")
}

#[test]
fn the_tools_that_document_the_refusal_are_the_tools_that_return_it() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (code, out) = (tmp.path().join("code"), tmp.path().join("out"));
    let other_out = tmp.path().join("other");
    fixture(&code);
    let index = |out: &Path| {
        crate::dispatch_tool(
            "index_codebase",
            &json!({ "path": code.to_string_lossy(), "output_dir": out.to_string_lossy(),
                "language": "rust" }),
            ToolProfile::Full,
        )
        .expect("index_codebase is registered")
    };
    assert_eq!(index(&out)["status"], "ok");
    assert_eq!(index(&other_out)["status"], "ok");
    let (graph, other) = (out.join("graph"), other_out.join("graph"));
    let paths = Paths {
        code: code.clone(),
        out: out.clone(),
        graph: graph.clone(),
        other,
    };

    let tools = crate::tool_schemas::tools_list();
    let names: Vec<String> = tools["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    let mut returns_it = Vec::new();
    for name in &names {
        let Some(args) = probe_args(name, &paths) else {
            continue;
        };
        let held = open_cached(&graph).expect("cached open");
        let response = crate::dispatch_tool(name, &args, ToolProfile::Full).expect("registered");
        drop(held);
        if refused(&response) {
            returns_it.push(name.clone());
            continue;
        }
        // A probe that failed for another reason says nothing: its arguments
        // must reach the point where the tool opens the graph.
        assert_eq!(
            response["status"], "ok",
            "{name} did not reach the graph: {response}"
        );
    }
    let mut listed: Vec<String> = HELD_GRAPH_TOOLS.iter().map(|s| s.to_string()).collect();
    returns_it.sort();
    listed.sort();
    assert_eq!(returns_it, listed, "measured refusals vs HELD_GRAPH_TOOLS");
}
