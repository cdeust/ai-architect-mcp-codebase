// lsp_resolver::edges — turning one `textDocument/definition` answer into a
// graph edge.
//
// Split from `lsp_resolver` when that file crossed the §4.1 500-line cap.
// This half owns the mapping from an LSP URI back to the indexer's key space
// and the label-pair rules that decide which relationship table an answer
// belongs in.

use super::sites::{NodePosition, UnresolvedCallSite};
use crate::graph_store::{call_rel_table, is_known_rel_table, GraphStore};
use crate::lsp_client;
use std::collections::HashMap;
use std::path::Path;

/// Read-only lookups every call site's edge insertion needs, grouped so
/// `LspPass::record` stays within the §4.4 parameter cap.
pub(super) struct SiteContext<'a> {
    pub(super) node_index: &'a HashMap<(String, u64), NodePosition>,
    pub(super) canonical_root: &'a Path,
}

pub(super) fn try_add_lsp_edge(
    store: &GraphStore,
    site: &UnresolvedCallSite,
    def: &lsp_client::DefinitionResult,
    ctx: &SiteContext<'_>,
) -> bool {
    // Convert LSP URI to a codebase-root-relative file path — the key space
    // of `node_index` (fleet-watch#18).
    let file_path = match uri_to_relative_path(&def.uri, ctx.canonical_root) {
        Some(p) => p,
        None => return false,
    };

    // Look up the definition in our node index.
    // LSP line is 0-based, our graph stores 1-based line numbers.
    let target_line = def.start_line + 1;

    // Exact line match only — no nearby-line scan. See `find_node_at_position`
    // for the fail-closed rationale.
    let target = find_node_at_position(ctx.node_index, &file_path, target_line);
    let target = match target {
        Some(t) => t,
        None => return false,
    };

    if !resolved_target_matches(target, site) {
        return false;
    }

    let Some(rel_type) = call_rel_table(&site.caller_label, &target.label) else {
        return false;
    };
    insert_lsp_edge(store, &rel_type, site, target)
}

/// Schema-guards `rel_type` and inserts the caller -> target edge at
/// LSP-backed confidence (0.9).
///
/// Schema guard: dynamically formatted rel tables can outrun the schema
/// when a new caller/target label combination appears. Drop rather than
/// abort.
///
/// Idempotent by construction (review finding 3): this pass writes one edge
/// per call site as it goes and flips `is_resolved` only at end of run, so
/// an interrupted run would otherwise be replayed into duplicate edges on
/// the next run — and two sites in one caller reaching the same callee
/// duplicate within a single run. An edge that is already there counts as
/// resolved, which is what it is.
fn insert_lsp_edge(
    store: &GraphStore,
    rel_type: &str,
    site: &UnresolvedCallSite,
    target: &NodePosition,
) -> bool {
    if !is_known_rel_table(rel_type) {
        eprintln!(
            "lsp_resolver: dropped edge with unknown rel table '{rel_type}' \
             ({} -> {}); add it to REL_TABLES in graph_store.rs",
            site.caller_qn, target.id
        );
        return false;
    }
    store
        .insert_edge_if_absent(
            rel_type,
            &site.caller_qn,
            &target.id,
            &[
                ("confidence", "0.9"),
                ("resolution_method", "'lsp-definition'"),
            ],
        )
        .is_ok()
}

/// Maps a definition URI onto the codebase-root-relative path the indexer
/// keyed the graph with (`file.strip_prefix(root)` — indexer/mod.rs).
///
/// fleet-watch#18 root cause: this used to return the ABSOLUTE path verbatim
/// ("the caller handles prefix matching" — no caller ever did), so every
/// node-index lookup missed and the LSP pass never inserted a single edge.
/// Percent-escapes are decoded (the server echoes the encoding
/// `path_to_file_uri` produced) and BOTH sides are canonicalized so a
/// macOS symlink alias (/var vs /private/var) cannot defeat the strip.
/// A definition outside the codebase root (stdlib, cargo registry,
/// node_modules) yields None — those targets are not in the graph.
fn uri_to_relative_path(uri: &str, canonical_root: &Path) -> Option<String> {
    let abs = lsp_client::file_uri_to_path(uri)?;
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    abs.strip_prefix(canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Exact (file, line) match only — no nearby-line scan. The node index
/// carries no column, so an inexact match cannot be told apart from a
/// coincidental same-line collision; guessing produced a fabricated
/// `total -> total` self-edge when a bare-argument CallSite (#87) resolved
/// to a PARAMETER declared on the same line as its enclosing method (PR
/// #267 follow-up). Failing closed costs recall on a genuinely multi-line
/// declaration; a wrong edge is worse than a missing one.
fn find_node_at_position<'a>(
    index: &'a HashMap<(String, u64), NodePosition>,
    file_path: &str,
    line: u64,
) -> Option<&'a NodePosition> {
    index.get(&(file_path.to_string(), line))
}

/// Defense in depth against a same-line collision `find_node_at_position`
/// cannot rule out on line-only data: even an EXACT line match can land on
/// the enclosing symbol's own declaration line (a parameter's
/// `targetSelectionRange` shares its function signature's line — see
/// `find_node_at_position`'s doc comment for the fabricated `total -> total`
/// self-edge this closes). The resolved node's own name (the last `::`
/// segment of its `id`, which the indexer sets equal to `qualified_name`)
/// must equal the identifier the call site actually asked about, or this is
/// that same collision wearing an exact-match line instead of a fuzzy one.
fn resolved_target_matches(target: &NodePosition, site: &UnresolvedCallSite) -> bool {
    let target_name = target.id.rsplit("::").next().unwrap_or(&target.id);
    target_name == site.identifier_name()
}

#[cfg(test)]
mod tests {
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
    fn a_replayed_definition_does_not_duplicate_the_calls_edge() {
        // Review finding 3 (regression). The pass CREATEs one edge per call
        // site as it goes and flips `is_resolved` only at end of run, so an
        // interrupted run left edges written and sites still unresolved — and
        // the next run wrote every one of them again, inflating get_impact
        // with duplicates that nothing could tell apart or clean up. Two
        // sites in one caller reaching the same callee did it within a single
        // run. Replaying the same answer must be a no-op.
        let f = edge_fixture("lsp dup test");
        let ctx = SiteContext {
            node_index: &f.node_index,
            canonical_root: &f.root,
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
}
