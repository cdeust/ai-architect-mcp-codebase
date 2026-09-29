// cpp_headers_399 — issue #399. `.h` is shared by C, C++ and Objective-C.
// Before the fix, the default (all-file) index parsed every `.h` with the C
// grammar, so a C++ library whose headers are `.h` (ETL) lost its classes and
// methods; and `language: "cpp"` dropped every `.h` from the walk. The grammar
// of a `.h` now comes from the language filter, or, without one, from C++
// constructs in the header itself.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, manifest, IndexOptions};
use ai_architect_mcp::parser::Language;
use std::fs;
use std::path::{Path, PathBuf};
mod common;
use common::TempDirExt;

const TEMPLATE_HEADER: &str = "#pragma once\n\
namespace etl {\n\
template <typename T, int N>\n\
class vector {\n\
public:\n\
    void push_back(const T& v);\n\
    int size() const;\n\
};\n\
}\n";

const GUARDED_C_HEADER: &str = "#ifndef TASK_H\n#define TASK_H\n\
#ifdef __cplusplus\nextern \"C\" {\n#endif\n\
void *pvPortMalloc(unsigned long xSize);\n\
#ifdef __cplusplus\n}\n#endif\n#endif\n";

const C_SOURCE: &str =
    "#include \"task.h\"\nvoid *pvPortMalloc(unsigned long xSize) { return 0; }\n";

const CPP_SOURCE: &str = "#include \"vector.h\"\nint main() { return 0; }\n";

struct Fixture {
    _tmp: common::TestTempDir,
    src: PathBuf,
    graph: PathBuf,
}

fn fixture(files: &[(&str, &str)]) -> Fixture {
    let tmp = tempfile::Builder::new()
        .prefix("cpp_headers_399_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture");
    for (path, body) in files {
        write(&src, path, body);
    }
    let graph = tmp.path().join("graph");
    Fixture {
        _tmp: tmp,
        src,
        graph,
    }
}

fn write(root: &Path, path: &str, body: &str) {
    let p = root.join(path);
    fs::create_dir_all(p.parent().unwrap_or(Path::new("."))).expect("mkdir");
    fs::write(p, body).expect("write fixture");
}

fn index(f: &Fixture, filter: Option<Language>) -> GraphStore {
    let options = IndexOptions {
        language_filter: filter,
        ..IndexOptions::default()
    };
    indexer::index_codebase_with_language(&f.src, &f.graph, &options).expect("index");
    GraphStore::open_or_create(&f.graph).expect("open graph")
}

/// Sorted (label, id, language) of every symbol whose id starts with `file::`.
fn symbols_of(store: &GraphStore, file: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for label in ["Function", "Method", "Struct", "Class", "Module"] {
        let q =
            format!("MATCH (n:{label}) WHERE n.id STARTS WITH '{file}::' RETURN n.id, n.language");
        if let Ok(res) = store.execute_query(&q) {
            for r in res.rows {
                out.push((label.to_string(), r[0].clone(), r[1].clone()));
            }
        }
    }
    out.sort();
    out
}

fn file_indexed(store: &GraphStore, file: &str) -> bool {
    let q = format!("MATCH (f:File) WHERE f.id = '{file}' RETURN f.id");
    !store.execute_query(&q).expect("file query").rows.is_empty()
}

fn has_method(symbols: &[(String, String, String)], name: &str) -> bool {
    symbols.iter().any(|(label, id, lang)| {
        label == "Method" && id.contains(&format!("::{name}#")) && lang == "cpp"
    })
}

#[test]
fn a_templated_class_header_is_parsed_as_cpp_without_a_filter() {
    let f = fixture(&[("vector.h", TEMPLATE_HEADER), ("main.cpp", CPP_SOURCE)]);
    let store = index(&f, None);
    let symbols = symbols_of(&store, "vector.h");
    assert!(
        has_method(&symbols, "push_back"),
        "C++ methods expected: {symbols:?}"
    );
    assert!(
        has_method(&symbols, "size"),
        "C++ methods expected: {symbols:?}"
    );
}

#[test]
fn the_cpp_filter_keeps_and_parses_h_headers() {
    let f = fixture(&[("vector.h", TEMPLATE_HEADER), ("main.cpp", CPP_SOURCE)]);
    let store = index(&f, Some(Language::Cpp));
    assert!(
        file_indexed(&store, "vector.h"),
        "a .h must be walked under cpp"
    );
    let symbols = symbols_of(&store, "vector.h");
    assert!(
        has_method(&symbols, "push_back"),
        "C++ methods expected: {symbols:?}"
    );
}

#[test]
fn a_guarded_c_header_stays_c_without_a_filter() {
    let f = fixture(&[("task.h", GUARDED_C_HEADER), ("heap.c", C_SOURCE)]);
    let store = index(&f, None);
    let symbols = symbols_of(&store, "task.h");
    assert!(
        !symbols.is_empty() && symbols.iter().all(|s| s.2 == "c"),
        "a C header keeps the C grammar: {symbols:?}"
    );
    // Only the C walker records body_kind: the prototype proves the C grammar ran.
    let q = "MATCH (n:Function) WHERE n.id STARTS WITH 'task.h::' RETURN n.body_kind";
    let kinds: Vec<String> = store
        .execute_query(q)
        .expect("kinds")
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(
        kinds,
        vec!["prototype".to_string()],
        "the header prototype is a C prototype"
    );
}

#[test]
fn the_c_filter_keeps_headers_as_c() {
    let f = fixture(&[("vector.h", TEMPLATE_HEADER), ("task.h", GUARDED_C_HEADER)]);
    let store = index(&f, Some(Language::C));
    assert!(file_indexed(&store, "vector.h") && file_indexed(&store, "task.h"));
    let symbols = symbols_of(&store, "vector.h");
    assert!(
        symbols.iter().all(|s| s.2 == "c"),
        "the caller said C: every header is C: {symbols:?}"
    );
}

#[test]
fn a_header_that_changes_dialect_leaves_no_stale_nodes_on_an_incremental_index() {
    const C_VERSION: &str = "#ifndef W_H\n#define W_H\nint old_api(void);\n#endif\n";
    let f = fixture(&[("widget.h", C_VERSION), ("main.cpp", CPP_SOURCE)]);
    let options = IndexOptions::default();
    indexer::index_codebase_with_language(&f.src, &f.graph, &options).expect("full index");
    let manifest_path = manifest::manifest_path(f.graph.parent().expect("graph parent"));
    indexer::write_full_manifest(&f.src, &manifest_path, &options).expect("manifest");
    {
        let store = GraphStore::open_or_create(&f.graph).expect("open");
        let before = symbols_of(&store, "widget.h");
        assert!(
            before.iter().any(|s| s.1.contains("old_api") && s.2 == "c"),
            "{before:?}"
        );
    }

    write(&f.src, "widget.h", TEMPLATE_HEADER);
    let prior = manifest::load(&manifest_path).expect("load manifest");
    let inc = indexer::index_incremental(&f.src, &f.graph, &manifest_path, &options, &prior)
        .expect("incremental");
    assert_eq!(inc.changed, 1, "only widget.h changed");

    let store = GraphStore::open_or_create(&f.graph).expect("reopen");
    let after = symbols_of(&store, "widget.h");
    assert!(
        !after.iter().any(|s| s.1.contains("old_api")),
        "the C prototype must not survive the switch to C++: {after:?}"
    );
    assert!(
        has_method(&after, "push_back"),
        "the C++ version is indexed: {after:?}"
    );
}
