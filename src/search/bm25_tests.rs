//! Split from `bm25.rs` per §4.1 (500-line cap) when fleet-watch#112's
//! file-content indexing tests pushed it over.
use super::*;
use crate::graph_store::NODE_FILE;
use std::fs;

#[test]
fn test_tokenize_symbol() {
    assert_eq!(tokenize_symbol("handle_tool_call"), "handle tool call");
    assert_eq!(tokenize_symbol("GraphStore"), "graph store");
    assert_eq!(
        tokenize_symbol("src/main.rs::handle_tool_call"),
        "src main rs handle tool call"
    );
}

#[test]
fn test_extract_file_path() {
    assert_eq!(file_path_of("src/main.rs::main"), "src/main.rs");
    assert_eq!(file_path_of("src/lib.rs"), "src/lib.rs");
}

/// Inserts a bare `File` node — no symbols, exactly the case a
/// markdown/prose file produces — and writes `content` at `path` under
/// `root` so `index_file_docs` has real bytes to read.
fn insert_file(store: &GraphStore, root: &Path, path: &str, content: &str, size: u64) {
    if let Some(parent) = root.join(path).parent() {
        fs::create_dir_all(parent).expect("mkdir fixture parent");
    }
    fs::write(root.join(path), content).expect("write fixture file");
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &format!("'{path}'")),
                ("path", &format!("'{path}'")),
                ("name", &format!("'{name}'")),
                ("extension", &format!("'{ext}'")),
                ("size_bytes", &size.to_string()),
                ("parse_errors", "0"),
            ],
        )
        .expect("insert File node");
}

#[test]
fn indexes_and_finds_markdown_prose_content() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_content_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    let prose = "# Coding Standards\n\nEvery contributor must run the playwright browser test suite before merging.\n";
    insert_file(
        &store,
        &root,
        "docs/coding-standards.md",
        prose,
        prose.len() as u64,
    );

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("build_index");
    assert_eq!(count, 1, "the bare File node has no symbols, only its body");

    // "playwright" appears only in the doc's prose, never in a symbol
    // name or qualified_name — a hit here can only come from `body`.
    let hits = query_index(&index_dir, "playwright browser test", 10).expect("query");
    assert_eq!(hits.len(), 1, "must find the doc by its body content");
    assert_eq!(hits[0].label, "File");
    assert_eq!(hits[0].qualified_name, "docs/coding-standards.md");
    assert_eq!(hits[0].file_path, "docs/coding-standards.md");
}

#[test]
fn non_doc_extensions_are_not_body_indexed() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_ext_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    // A bare File node for a non-doc extension the parser did not turn
    // into symbols either (e.g. a data file) — must NOT be body-indexed.
    insert_file(&store, &root, "data/dump.bin", "unrelated-marker-token", 22);

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("build_index");
    assert_eq!(count, 0, ".bin is not in DOC_EXTENSIONS");

    let hits = query_index(&index_dir, "unrelated marker token", 10).expect("query");
    assert!(hits.is_empty());
}

#[test]
fn oversized_doc_is_skipped() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_cap_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    // Real file is tiny; the graph's recorded size_bytes is what the cap
    // checks (a cheap pre-read filter), so lie about it being oversized.
    insert_file(&store, &root, "HUGE.md", "small", MAX_DOC_BYTES + 1);

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("build_index");
    assert_eq!(count, 0, "size_bytes over MAX_DOC_BYTES must be skipped");
}
