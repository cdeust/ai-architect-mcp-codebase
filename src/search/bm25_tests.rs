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

/// One doc fixture: the bytes to put on disk, and the size the GRAPH will
/// record for them. The two are separate fields on purpose — their divergence
/// is exactly what the staleness tests below exercise (§4.4: five positional
/// arguments, two of them sizes, is a missing type).
struct DocFile<'a> {
    path: &'a str,
    content: &'a str,
    recorded_size: u64,
}

impl<'a> DocFile<'a> {
    /// The ordinary case: what the graph recorded agrees with what is on disk.
    fn new(path: &'a str, content: &'a str) -> Self {
        DocFile {
            path,
            content,
            recorded_size: content.len() as u64,
        }
    }

    /// Makes the graph's `size_bytes` DISAGREE with the file on disk — the
    /// stale snapshot the cap used to trust instead of reading.
    fn recorded_size(mut self, size: u64) -> Self {
        self.recorded_size = size;
        self
    }
}

/// Inserts a bare `File` node — no symbols, exactly the case a markdown/prose
/// file produces — and writes its content under `root` so `index_file_docs`
/// has real bytes to read.
fn insert_file(store: &GraphStore, root: &Path, doc: DocFile<'_>) {
    let path = doc.path;
    if let Some(parent) = root.join(path).parent() {
        fs::create_dir_all(parent).expect("mkdir fixture parent");
    }
    fs::write(root.join(path), doc.content).expect("write fixture file");
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
                ("size_bytes", &doc.recorded_size.to_string()),
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
        DocFile::new("docs/coding-standards.md", prose),
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
    insert_file(
        &store,
        &root,
        DocFile::new("data/dump.bin", "unrelated-marker-token"),
    );

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("build_index");
    assert_eq!(count, 0, ".bin is not in DOC_EXTENSIONS");

    let hits = query_index(&index_dir, "unrelated marker token", 10).expect("query");
    assert!(hits.is_empty());
}

/// Review finding 1. The cap used to be checked against the graph's recorded
/// `File.size_bytes` — a snapshot taken when the file was indexed — while the
/// read that followed had no bound of its own. A doc that grew past the cap
/// afterwards (an actively-written CHANGELOG, a regenerated spec dump) passed
/// the stale check and was then read whole, so the cap bounded nothing in
/// exactly the case it exists for.
///
/// This test fails on the pre-fix code: `size_bytes` says 5, the file on disk
/// is over the cap, and the whole thing gets indexed.
#[test]
fn a_doc_grown_past_the_cap_since_indexing_is_not_read() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_grown_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    let grown = "z".repeat(MAX_DOC_BYTES as usize + 1);
    insert_file(
        &store,
        &root,
        DocFile::new("CHANGELOG.md", &grown).recorded_size(5),
    );

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("build_index");
    assert_eq!(
        count, 0,
        "the cap must bound the bytes actually read, not a stale size_bytes"
    );
}

/// The other direction of the same staleness. A doc the graph recorded as
/// oversized but that has since shrunk is perfectly indexable, and the stale
/// snapshot used to exclude it. Now only the on-disk bytes decide.
///
/// This test fails on the pre-fix code, which skipped on `size_bytes` alone.
#[test]
fn a_stale_oversized_size_bytes_no_longer_hides_a_small_doc() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_shrunk_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    insert_file(
        &store,
        &root,
        DocFile::new("NOTES.md", "the flywheel calibration ledger")
            .recorded_size(MAX_DOC_BYTES + 1),
    );

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("build_index");
    assert_eq!(
        count, 1,
        "a small file on disk is indexable whatever the graph recorded"
    );
    let hits = query_index(&index_dir, "flywheel calibration ledger", 10).expect("query");
    assert_eq!(hits.len(), 1);
}

/// Review finding 5. `Err(_) => return Ok(0)` covered TWO failure modes with
/// one catch-all: the documented one (a doc file unreadable on disk) and the
/// undocumented one (the Cypher query itself breaking — a schema rename, a
/// database error). The second reported a normal-looking zero-count success,
/// so a wholly broken doc pass was indistinguishable from a repository with no
/// docs in it.
///
/// A store with no schema is the cheapest way to make that query fail for real
/// (`MATCH (n:File)` is a hard Binder error when the table does not exist).
/// This test fails on the pre-fix code, which returns `Ok(0)`.
#[test]
fn a_broken_file_query_surfaces_instead_of_reporting_zero_docs() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_query_err_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    // Deliberately NO create_schema: the File table does not exist.
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");

    let index_dir = tmp.path().join("bm25_index");
    let err = build_index(&store, &index_dir, &root)
        .expect_err("a failing File query must not be reported as zero docs");
    assert!(
        err.contains("bm25 doc pass"),
        "the error must name the pass that failed, got: {err}"
    );
}

/// A `File` the graph knows about but that is gone from disk is skipped, and
/// the build still succeeds — the pass stays best-effort per file. Pins the
/// half of finding 7's contract that must NOT change now that the query-failure
/// half propagates.
#[test]
fn an_unreadable_doc_file_does_not_abort_the_index() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_missing_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    let prose = "the readable one mentions parallax";
    insert_file(&store, &root, DocFile::new("present.md", prose));
    insert_file(
        &store,
        &root,
        DocFile::new("gone.md", "deleted after indexing"),
    );
    fs::remove_file(root.join("gone.md")).expect("rm fixture");

    let index_dir = tmp.path().join("bm25_index");
    let count = build_index(&store, &index_dir, &root).expect("an unreadable file must not fail");
    assert_eq!(count, 1, "only the file still on disk is indexed");
    assert_eq!(
        query_index(&index_dir, "parallax", 10)
            .expect("query")
            .len(),
        1
    );
}

/// Review finding 9. Symbol documents put `tokenize_symbol(qn)` into the shared
/// Tantivy fields; File documents put the raw path in, so a camelCase path
/// segment got none of the query-side splitting a symbol name gets — on the
/// very field the field boost privileges.
///
/// `qualified_name` must stay raw (it is the stored key `enrich_file_hit` binds
/// back to `File.id`), so the splitting is provided by the two indexed fields:
/// the tokenized file name, and the tokenized path prefixed onto `body`.
///
/// This test fails on the pre-fix code: neither "my design doc" nor "reference"
/// tokenizes out of `MyDesignDoc.md` / `docs/reference/…`.
#[test]
fn a_camel_case_doc_path_is_query_splittable() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_camel_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    // The body shares no token with the query — only the PATH can answer it.
    let prose = "Nothing here repeats the file name.";
    insert_file(
        &store,
        &root,
        DocFile::new("docs/reference/MyDesignDoc.md", prose),
    );

    let index_dir = tmp.path().join("bm25_index");
    assert_eq!(
        build_index(&store, &index_dir, &root).expect("build_index"),
        1
    );

    let hits = query_index(&index_dir, "my design doc", 10).expect("query");
    assert_eq!(
        hits.len(),
        1,
        "a camelCase file name must split like a symbol name"
    );
    assert_eq!(
        hits[0].qualified_name, "docs/reference/MyDesignDoc.md",
        "the STORED key stays the raw path — enrich_file_hit binds it to File.id"
    );

    let by_dir = query_index(&index_dir, "reference", 10).expect("query");
    assert_eq!(by_dir.len(), 1, "path segments must be reachable too");
}

/// `.markdown` is one of the three spellings `light_link::MD_EXTS` accepts, so
/// a `.markdown` file earns doc-link edges. It used to earn no BM25 body, which
/// made it findable by what points at it and not by what it says.
///
/// This test fails on the pre-fix code: `.markdown` was absent from
/// `DOC_EXTENSIONS`.
#[test]
fn the_markdown_spelling_is_indexed_like_md() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_doc_markdown_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    let prose = "an archaeology of the vestibular pipeline";
    insert_file(&store, &root, DocFile::new("NOTES.markdown", prose));

    let index_dir = tmp.path().join("bm25_index");
    assert_eq!(
        build_index(&store, &index_dir, &root).expect("build_index"),
        1
    );
    assert_eq!(
        query_index(&index_dir, "vestibular pipeline", 10)
            .expect("query")
            .len(),
        1
    );
}
