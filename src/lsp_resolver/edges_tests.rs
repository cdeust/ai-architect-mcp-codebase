// lsp_resolver::edges tests, moved out of edges.rs to keep it under the
// 500-line cap (issue #366).

use super::super::sites::build_node_position_index;
use super::*;
use crate::graph_store::NODE_FUNCTION;

/// Inserts a Function node whose `name` is the last `::` segment of `id`
/// (mirrors the indexer's `<file>::<name>` qualified-name convention).
fn insert_function(store: &GraphStore, id: &str, start_line: &str) {
    let name = id.rsplit("::").next().unwrap_or(id);
    let end_line = format!("{}", start_line.parse::<u64>().unwrap_or(1) + 2);
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", &format!("'{id}'")),
                ("name", &format!("'{name}'")),
                ("qualified_name", &format!("'{id}'")),
                ("start_line", start_line),
                ("end_line", &end_line),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .unwrap_or_else(|e| panic!("insert Function {id}: {e}"));
}

/// Inserts the `CallSite` node a per-site row starts from.
fn insert_call_site(store: &GraphStore, id: &str) {
    store
        .insert_node(
            crate::graph_store::NODE_CALL_SITE,
            &[
                ("id", &format!("'{id}'")),
                ("callee_name", "'target'"),
                ("is_resolved", "false"),
            ],
        )
        .unwrap_or_else(|e| panic!("insert CallSite {id}: {e}"));
}

/// Every `Calls_CallSite_Function` row as (call site id, target id).
fn per_site_rows(store: &GraphStore) -> Vec<Vec<String>> {
    store
        .execute_query(
            "MATCH (s:CallSite)-[:Calls_CallSite_Function]->(b:Function) \
             RETURN s.id, b.id ORDER BY s.id",
        )
        .expect("query per-site rows")
        .rows
}

/// A store with `src/a.rs::caller` calling into `src/b.rs::target`, plus
/// the site, definition and context that connect them.
struct EdgeFixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    store: GraphStore,
    node_index: HashMap<(String, u64), NodePosition>,
    site: UnresolvedCallSite,
    def: lsp_client::DefinitionResult,
}

fn edge_fixture(prefix: &str) -> EdgeFixture {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/b.rs"), "pub fn target() {}").expect("write");
    let store = GraphStore::open_or_create(&root.join("db")).expect("open");
    store.create_schema().expect("schema");
    insert_function(&store, "src/a.rs::caller", "1");
    insert_function(&store, "src/b.rs::target", "10");
    let node_index = build_node_position_index(&store).expect("index");
    EdgeFixture {
        _dir: dir,
        site: UnresolvedCallSite {
            id: "src/a.rs::caller::call@5:4".to_string(),
            caller_qn: "src/a.rs::caller".to_string(),
            caller_label: "Function".to_string(),
            callee_name: "target".to_string(),
            file_path: "src/a.rs".to_string(),
            line: 5,
            col: 4,
        },
        def: lsp_client::DefinitionResult {
            uri: lsp_client::path_to_file_uri(&root.join("src/b.rs")),
            start_line: 9, // LSP is 0-based; the graph stores 1-based line 10
            start_col: 0,
        },
        root,
        store,
        node_index,
    }
}

fn calls_edges(store: &GraphStore) -> Vec<Vec<String>> {
    store
        .execute_query(
            "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) \
             RETURN a.id, b.id",
        )
        .expect("query edge")
        .rows
}

#[test]
fn uri_to_relative_path_strips_canonical_root_and_percent_decodes() {
    // fleet-watch#18 root cause: the pre-fix version returned the
    // absolute path verbatim, which never matched the root-relative
    // node-index keys. The tempdir prefix contains a space so the URI
    // round-trip exercises percent-decoding, and on macOS the tempdir
    // lives under /var (a symlink to /private/var), exercising the
    // canonicalize-both-sides requirement.
    let dir = tempfile::Builder::new()
        .prefix("lsp uri test")
        .tempdir()
        .expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    let file = root.join("src/main.rs");
    std::fs::write(&file, "fn main() {}").expect("write file");

    let uri = lsp_client::path_to_file_uri(&file);
    assert!(
        uri.contains("%20"),
        "encoded URI must contain the escaped space: {uri}"
    );
    assert_eq!(
        uri_to_relative_path(&uri, &root),
        Some("src/main.rs".to_string())
    );

    // A definition outside the root (stdlib, cargo registry) must yield
    // None rather than an out-of-root key.
    let outside = lsp_client::path_to_file_uri(Path::new("/usr/lib/foo.rs"));
    assert_eq!(uri_to_relative_path(&outside, &root), None);

    // Non-file scheme: None.
    assert_eq!(uri_to_relative_path("https://example.com", &root), None);
}

#[test]
fn an_explicit_localhost_authority_names_the_same_local_file() {
    // Review finding 6 (regression): RFC 8089 lets a server spell a local
    // file with an explicit `localhost` authority. Stripping a fixed
    // `file://` left a RELATIVE path, `strip_prefix(root)` failed, and the
    // pass inserted zero edges — the very symptom fleet-watch#18 was
    // opened to remove, through a spelling the LSP spec permits.
    let dir = tempfile::Builder::new()
        .prefix("lsp_authority")
        .tempdir()
        .expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/main.rs"), "fn main() {}").expect("write file");
    let uri = lsp_client::path_to_file_uri(&root.join("src/main.rs"));

    for authority in ["localhost", "LocalHost"] {
        assert_eq!(
            uri_to_relative_path(
                &uri.replacen("file://", &format!("file://{authority}"), 1),
                &root
            ),
            Some("src/main.rs".to_string()),
            "authority {authority} names this machine (case-insensitively)"
        );
    }
    // A remote authority is not a local path.
    assert_eq!(
        uri_to_relative_path(&uri.replacen("file://", "file://example.com", 1), &root),
        None
    );
}

#[test]
fn lsp_definition_inserts_calls_edge_via_relative_key() {
    // fleet-watch#18 end-to-end regression: pre-fix, the absolute
    // definition URI missed every node-index key, so the LSP pass had
    // inserted 0 edges — ever.
    let f = edge_fixture("lsp edge test");
    let ctx = SiteContext {
        node_index: &f.node_index,
        canonical_root: &f.root,
        twins: &crate::resolver::cfg_verdict::TwinView::default(),
    };
    assert!(
        try_add_lsp_edge(&f.store, &f.site, &f.def, &ctx),
        "edge must be inserted from an absolute percent-encoded definition URI"
    );
    let rows = calls_edges(&f.store);
    assert_eq!(rows.len(), 1, "exactly one Calls edge must exist");
    assert_eq!(rows[0][0], "src/a.rs::caller");
    assert_eq!(rows[0][1], "src/b.rs::target");
}

#[test]
fn an_lsp_resolved_site_had_no_per_site_target_row() {
    // Issue #335: the LSP pass wrote the caller-level edge only, so the
    // call site it resolved had no row naming its own target.
    let f = edge_fixture("lsp per-site test");
    insert_call_site(&f.store, &f.site.id);
    let ctx = SiteContext {
        node_index: &f.node_index,
        canonical_root: &f.root,
        twins: &crate::resolver::cfg_verdict::TwinView::default(),
    };
    assert!(try_add_lsp_edge(&f.store, &f.site, &f.def, &ctx));
    assert_eq!(
        per_site_rows(&f.store),
        vec![vec![f.site.id.clone(), "src/b.rs::target".to_string()]]
    );
}

#[test]
fn a_replayed_definition_does_not_duplicate_the_calls_edge() {
    // Review finding 3 (regression). The pass CREATEs one edge per call
    // site as it goes and flips `is_resolved` only at end of run, so an
    // interrupted run left edges written and sites still unresolved — and
    // the next run wrote every one of them again, inflating get_impact
    // with duplicates that nothing could tell apart or clean up. Two
    // sites in one caller reaching the same callee did it within a single
    // run. Replaying the same answer must be a no-op.
    let f = edge_fixture("lsp dup test");
    insert_call_site(&f.store, &f.site.id);
    insert_call_site(&f.store, "src/a.rs::caller::call@6:8");
    let ctx = SiteContext {
        node_index: &f.node_index,
        canonical_root: &f.root,
        twins: &crate::resolver::cfg_verdict::TwinView::default(),
    };
    assert!(try_add_lsp_edge(&f.store, &f.site, &f.def, &ctx));
    // Same site again (an interrupted run replayed), then a DIFFERENT
    // site of the same caller reaching the same callee (one run).
    assert!(try_add_lsp_edge(&f.store, &f.site, &f.def, &ctx));
    let sibling = UnresolvedCallSite {
        id: "src/a.rs::caller::call@6:8".to_string(),
        line: 6,
        col: 8,
        ..f.site
    };
    assert!(try_add_lsp_edge(&f.store, &sibling, &f.def, &ctx));
    assert_eq!(
        calls_edges(&f.store).len(),
        1,
        "the caller→callee edge must exist exactly once"
    );
    // One per-site row per call site, the replay adding none (#335).
    assert_eq!(per_site_rows(&f.store).len(), 2);
}

#[test]
fn find_node_at_position_matches_exactly_and_never_a_nearby_line() {
    // Soundness regression (PR #267 follow-up): the node index carries
    // no column, so a nearby-line scan cannot tell a real "definition
    // starts a few lines above the name" case apart from a coincidental
    // collision. Fail closed: exact line only.
    let mut index = HashMap::new();
    index.insert(
        ("src/main.rs".to_string(), 10),
        NodePosition {
            id: "fn1".to_string(),
            label: "Function".to_string(),
        },
    );
    assert_eq!(
        find_node_at_position(&index, "src/main.rs", 10)
            .expect("exact match")
            .id,
        "fn1"
    );

    let mut nearby = HashMap::new();
    nearby.insert(
        ("src/main.rs".to_string(), 8),
        NodePosition {
            id: "fn2".to_string(),
            label: "Method".to_string(),
        },
    );
    assert!(
        find_node_at_position(&nearby, "src/main.rs", 10).is_none(),
        "a node two lines away must NOT match — no fuzzy fallback"
    );

    assert!(find_node_at_position(&HashMap::new(), "src/main.rs", 10).is_none());
}

#[test]
fn a_definition_landing_on_an_unrelated_same_line_node_is_refused() {
    // The exact regression this fix closes: `extra_call_entries` (#87)
    // emits a speculative CallSite for a bare-identifier argument (here
    // standing in for the `i` in `self.response_of(i)`). rust-analyzer
    // resolves it to its own PARAMETER declaration — not itself an
    // indexed node — which happens to share a line with the ENCLOSING
    // method's own declaration. An exact line match therefore lands on
    // `caller` even though the call site never asked to resolve
    // `caller`. The identifier-name check must refuse this edge.
    let f = edge_fixture("lsp same line collision test");
    let ctx = SiteContext {
        node_index: &f.node_index,
        canonical_root: &f.root,
        twins: &crate::resolver::cfg_verdict::TwinView::default(),
    };
    let arg_site = UnresolvedCallSite {
        id: "src/a.rs::caller::call@1:9".to_string(),
        callee_name: "i".to_string(), // not "target" — a bare argument
        line: 1,                      // same line as `caller`'s own node
        col: 9,
        ..f.site
    };
    let def_on_callers_own_line = lsp_client::DefinitionResult {
        uri: lsp_client::path_to_file_uri(&f.root.join("src/a.rs")),
        start_line: 0, // LSP 0-based line 0 == graph's 1-based line 1
        start_col: 9,
    };
    assert!(
        !try_add_lsp_edge(&f.store, &arg_site, &def_on_callers_own_line, &ctx),
        "a definition whose own name does not match the call site's \
         identifier must never insert an edge"
    );
    assert!(
        calls_edges(&f.store).is_empty(),
        "no edge — fabricated or otherwise — may be inserted"
    );
}
