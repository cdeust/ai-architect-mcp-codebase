use super::*;
use crate::tool_profile::ToolProfile;
use serde_json::json;
use std::path::{Path, PathBuf};

const MAIN_PY: &str = "from work import hub, step_one\n\n\
def a():\n    hub()\n\n\
def b():\n    hub()\n\n\
def c():\n    hub()\n\n\
def d():\n    hub()\n\n\
def main():\n    step_one()\n    vanished_helper()\n";

const WORK_PY: &str = "def hub():\n    return 1\n\n\
def step_one():\n    return hub()\n\n\
def broken(  \n    @#$%^& }{\n";

fn call(name: &str, args: Value) -> Value {
    crate::dispatch_tool(name, &args, ToolProfile::Full).expect("registered tool")
}

fn ok(name: &str, args: Value) -> Value {
    let response = call(name, args);
    assert_eq!(response["status"], "ok", "{name}: {response}");
    response
}

/// A Python repository with a hub, an unresolved call and a file
/// tree-sitter reads with errors, indexed through the tools; resolved and
/// clustered when `cluster` is set. Returns the temp dir and the graph path.
pub(super) fn fixture(cluster: bool) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("wiki_tool_")
        .tempdir()
        .expect("tempdir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.py"), MAIN_PY).unwrap();
    std::fs::write(src.join("work.py"), WORK_PY).unwrap();
    let out = tmp.path().join("out");
    ok(
        "index_codebase",
        json!({ "path": src.to_string_lossy(), "output_dir": out.to_string_lossy(),
            "language": "python" }),
    );
    let graph = out.join("graph");
    if cluster {
        ok(
            "resolve_graph",
            json!({ "graph_path": graph.to_string_lossy() }),
        );
        ok(
            "cluster_graph",
            json!({ "graph_path": graph.to_string_lossy() }),
        );
    }
    (tmp, graph)
}

pub(super) fn export_args(graph: &Path) -> Value {
    json!({ "graph_path": graph.to_string_lossy() })
}

/// The tool writes exactly the bytes `wiki::export` builds from the same
/// store and coverage record, names them by size and hash, counts the pages
/// by kind, and a second call writes the same bytes (criterion 6 through
/// the tool).
#[test]
fn the_tool_writes_the_skeleton_the_library_builds() {
    let (_tmp, graph) = fixture(true);
    let response = ok("export_wiki_skeleton", export_args(&graph));
    let artifact = graph.parent().unwrap().join(WIKI_SKELETON_FILE);
    assert_eq!(
        Path::new(response["artifact_path"].as_str().unwrap()),
        std::path::absolute(&artifact).unwrap()
    );
    let bytes = std::fs::read(&artifact).expect("artifact written");

    let record = coverage::load(&coverage::coverage_path(graph.parent().unwrap())).unwrap();
    let store = graph_cache::open_cached(&graph).unwrap();
    let expected = wiki::export(&store, &record).unwrap();
    drop(store);
    let expected_json = expected.skeleton.to_json().unwrap();
    assert_eq!(String::from_utf8(bytes.clone()).unwrap(), expected_json);
    assert_eq!(response["artifact_bytes"], bytes.len());
    assert_eq!(
        response["artifact_sha256"],
        hex_lower(&Sha256::digest(&bytes))
    );
    assert_eq!(response["schema_version"], wiki::SCHEMA_VERSION);
    assert_eq!(response["page_count"], expected.skeleton.pages().len());
    let by_kind = &response["pages_by_kind"];
    for (key, kind) in [
        ("community", PageKind::Community),
        ("process", PageKind::Process),
        ("god_node", PageKind::GodNode),
    ] {
        let n = expected
            .skeleton
            .pages()
            .iter()
            .filter(|p| p.kind == kind)
            .count();
        assert!(n > 0, "the fixture has no {kind} page");
        assert_eq!(by_kind[key], n, "{response}");
    }
    assert!(response.get("warnings").is_none(), "{response}");

    // The report is the library's, list by list.
    let report = &response["not_in_skeleton"];
    assert_eq!(report["missed_unanchored"]["total_count"], 0);
    assert_eq!(
        report["parse_incomplete_unattributed"]["items"],
        json!(expected.report.parse_incomplete_unattributed)
    );
    let fence = expected.report.god_node_fence.expect("fence");
    assert_eq!(
        report["god_node_fence"]["limit_quarters"],
        fence.limit_quarters()
    );

    let again = ok("export_wiki_skeleton", export_args(&graph));
    assert_eq!(again["artifact_sha256"], response["artifact_sha256"]);
    assert_eq!(std::fs::read(&artifact).unwrap(), bytes);
}

/// Without the coverage record the call fails and writes nothing: an empty
/// coverage block would read as "no gap".
#[test]
fn no_coverage_record_is_refused_and_nothing_is_written() {
    let (_tmp, graph) = fixture(true);
    let out = graph.parent().unwrap();
    std::fs::remove_file(coverage::coverage_path(out)).expect("sidecar exists");
    let response = call("export_wiki_skeleton", export_args(&graph));
    assert_eq!(response["status"], "error", "{response}");
    assert!(
        response["message"]
            .as_str()
            .unwrap()
            .contains("no usable coverage record"),
        "{response}"
    );
    assert!(!out.join(WIKI_SKELETON_FILE).exists());
}

/// A graph that was never clustered exports a valid skeleton with no
/// community page, and says why.
#[test]
fn an_unclustered_graph_exports_with_a_warning() {
    let (_tmp, graph) = fixture(false);
    let response = ok("export_wiki_skeleton", export_args(&graph));
    assert_eq!(response["pages_by_kind"]["community"], 0);
    assert_eq!(
        response["warnings"][0]["code"], "no_community",
        "{response}"
    );
    let artifact = graph.parent().unwrap().join(WIKI_SKELETON_FILE);
    let text = std::fs::read_to_string(artifact).unwrap();
    assert!(WikiSkeleton::from_json(&text).is_ok());
}

/// A graph path that does not exist, or no path, is refused and writes
/// nothing.
#[test]
fn a_missing_graph_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let response = call(
        "export_wiki_skeleton",
        export_args(&tmp.path().join("nothing")),
    );
    assert_eq!(response["status"], "error");
    assert!(!tmp.path().join(WIKI_SKELETON_FILE).exists());
    let response = call("export_wiki_skeleton", json!({}));
    assert_eq!(response["status"], "error");
}

/// Every list of the report is bounded and keeps its full count.
#[test]
fn a_long_report_list_is_bounded_with_its_count() {
    let report = ExportReport {
        missed_unattributed: (0..20_000).map(|i| format!("site_{i:05}")).collect(),
        ..ExportReport::default()
    };
    let view = report_view(&report);
    let list = &view["missed_unattributed"];
    assert_eq!(list["total_count"], 20_000);
    assert_eq!(list["truncated"], true);
    let shown = list["items"].as_array().unwrap().len();
    assert!(shown > 0 && shown < 20_000, "{shown}");
    assert_eq!(list["items"][0], "site_00000");
    assert_eq!(view["community_unanchored_members"]["truncated"], false);
    assert_eq!(view["god_node_fence"], Value::Null);
}
