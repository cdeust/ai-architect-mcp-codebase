// lsp_resolver — LSP-enhanced resolution pass for unresolved call sites.
//
// Queries a Language Server Protocol server for textDocument/definition
// to resolve method calls on inferred types that the static 3b resolver
// cannot handle. Runs AFTER resolve_graph as an optional enhancement.
//
// source: stages/stage-3b.md §7 — "method calls on inferred types" deferred to LSP

use crate::graph_store::{cypher_str, is_known_rel_table, GraphStore};
use crate::lsp_client::{self, LspClient, LspResolutionResult};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run LSP-enhanced resolution on unresolved call sites in the graph.
///
/// 1. Collects CallSite nodes that lack a Calls edge (unresolved).
/// 2. Starts the appropriate LSP server.
/// 3. For each unresolved site, queries textDocument/definition.
/// 4. Maps definition locations back to graph nodes, adds Calls edges.
pub fn resolve_with_lsp(
    store: &GraphStore,
    codebase_path: &Path,
    language: &str,
    lsp_command_override: Option<&str>,
    timeout: Duration,
) -> Result<LspResolutionResult, String> {
    let start = Instant::now();

    // Determine LSP command.
    // source: C3 fix — caller-provided `lsp_command_override` must be validated
    // against the allowlist BEFORE `Command::new` to prevent arbitrary binary
    // execution. `LspClient::start` also validates as defense-in-depth.
    let (cmd, default_args) = match lsp_command_override {
        Some(c) => {
            lsp_client::validate_lsp_command(c)?;
            (c, &[] as &[&str])
        }
        None => {
            let detected = lsp_client::detect_lsp_command(language)
                .ok_or(format!("no LSP server known for language '{language}'"))?;
            (detected.0, detected.1)
        }
    };

    if !lsp_client::is_command_available(cmd) {
        return Err(format!("lsp_not_found: {cmd} not found in PATH"));
    }

    // Collect unresolved call sites
    let unresolved = collect_unresolved_callsites(store)?;
    if unresolved.is_empty() {
        return Ok(LspResolutionResult {
            resolved_count: 0,
            failed_count: 0,
            skipped_count: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Start LSP server
    let mut client = LspClient::start(cmd, default_args, codebase_path, timeout)?;
    client.initialize(codebase_path)?;

    // Group call sites by file for efficient didOpen batching
    let by_file = group_by_file(&unresolved);

    // Build a position index of graph nodes for mapping definition results
    let node_index = build_node_position_index(store)?;

    // fleet-watch#18: definition URIs come back absolute (and on macOS the
    // server may answer under /private/var while the caller passed /var, or
    // vice versa). Canonicalize the root ONCE so uri_to_relative_path can
    // strip it from the canonicalized definition path and recover the same
    // root-relative key the indexer used.
    let canonical_root =
        std::fs::canonicalize(codebase_path).unwrap_or_else(|_| codebase_path.to_path_buf());

    let mut resolved_count = 0u64;
    let mut failed_count = 0u64;
    let mut skipped_count = 0u64;
    // §10.4 invariant: is_resolved flips when the callee resolved to a graph
    // target, whichever pass found it — mirrors resolver::calls. Without this
    // a rerun re-queries every LSP-resolved site.
    let mut newly_resolved: Vec<String> = Vec::new();
    let per_request_timeout = Duration::from_secs(5);

    for (file_path, sites) in &by_file {
        let abs_path = codebase_path.join(file_path);
        if !abs_path.exists() {
            skipped_count += sites.len() as u64;
            continue;
        }
        let file_uri = lsp_client::path_to_file_uri(&abs_path);
        let lang_id = language_id_for(language);

        // Read file content for didOpen
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => {
                skipped_count += sites.len() as u64;
                continue;
            }
        };

        if client.did_open(&file_uri, lang_id, &content).is_err() {
            skipped_count += sites.len() as u64;
            continue;
        }

        for site in sites {
            let result = client.get_definition(&file_uri, site.line, site.col);
            match result {
                Ok(Some(def)) => {
                    if try_add_lsp_edge(store, site, &def, &node_index, &canonical_root) {
                        resolved_count += 1;
                        newly_resolved.push(site.id.clone());
                    } else {
                        failed_count += 1;
                    }
                }
                Ok(None) => failed_count += 1,
                Err(e) => {
                    if e.contains("timeout") {
                        // Skip remaining sites in this file on timeout
                        skipped_count += 1;
                    } else {
                        failed_count += 1;
                    }
                }
            }

            // Respect per-request timeout budget
            if start.elapsed() > timeout.saturating_sub(per_request_timeout) {
                skipped_count += (sites.len() as u64).saturating_sub(resolved_count + failed_count);
                break;
            }
        }
    }

    let _ = client.shutdown();

    let id_refs: Vec<&str> = newly_resolved.iter().map(|s| s.as_str()).collect();
    store.mark_nodes_resolved("CallSite", &id_refs)?;

    Ok(LspResolutionResult {
        resolved_count,
        failed_count,
        skipped_count,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// Unresolved call site collection
// ---------------------------------------------------------------------------

struct UnresolvedCallSite {
    id: String,
    caller_qn: String,
    caller_label: String,
    #[allow(dead_code)] // retained for diagnostics and future logging
    callee_name: String,
    file_path: String,
    line: u64,
    col: u64,
}

fn collect_unresolved_callsites(store: &GraphStore) -> Result<Vec<UnresolvedCallSite>, String> {
    // fleet-watch#18 adjacent defect (b): "unresolved" was previously decided
    // at CALLER granularity — "does the enclosing function have ANY outgoing
    // Calls edge" — so a caller with ten sites and one static resolution had
    // its nine remaining sites skipped. The per-site marker the static
    // resolver actually maintains is `CallSite.is_resolved` (§10.4, flipped
    // by resolver::calls via mark_nodes_resolved); filter on it directly.
    let qr = store.execute_query(
        "MATCH (cs:CallSite) WHERE cs.is_resolved = false \
         RETURN cs.id, cs.callee_name, cs.line, cs.col",
    )?;

    let mut sites = Vec::new();
    for row in &qr.rows {
        if row.len() < 4 {
            continue;
        }
        let cs_id = &row[0];
        let callee = &row[1];
        let line: u64 = row[2].parse().unwrap_or(0);
        let col: u64 = row[3].parse().unwrap_or(0);

        let (file_path, caller_qn) = parse_callsite_id(cs_id);
        let caller_label = determine_caller_label(store, &caller_qn);

        sites.push(UnresolvedCallSite {
            id: cs_id.clone(),
            caller_qn,
            caller_label,
            callee_name: callee.clone(),
            file_path,
            line,
            col,
        });
    }
    Ok(sites)
}

fn extract_caller_from_callsite_id(cs_id: &str) -> String {
    if let Some(idx) = cs_id.rfind("::call@") {
        cs_id[..idx].to_string()
    } else {
        cs_id.to_string()
    }
}

fn parse_callsite_id(cs_id: &str) -> (String, String) {
    let caller_qn = extract_caller_from_callsite_id(cs_id);
    let file_path = extract_file_from_qn(&caller_qn);
    (file_path, caller_qn)
}

fn extract_file_from_qn(qn: &str) -> String {
    // fleet-watch#18 adjacent defect (a): a hardcoded .rs/.py/.ts/.tsx list
    // fell through to returning the WHOLE qn for the other seven parsed
    // languages (java/kt/swift/objc/c/cpp/go/rb), producing index keys that
    // can never equal a CallSite file_path. Delegate to the shared
    // all-language helper the static resolver already uses
    // (language_provider::ALL_EXTENSIONS = parser::Language::from_extension).
    crate::language_provider::extract_file_prefix(qn).unwrap_or_else(|| qn.to_string())
}

fn determine_caller_label(store: &GraphStore, caller_qn: &str) -> String {
    let esc = cypher_str(caller_qn);
    for label in &["Function", "Method"] {
        let cypher = format!("MATCH (n:{label}) WHERE n.qualified_name = {esc} RETURN n.id");
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                return label.to_string();
            }
        }
    }
    "Function".to_string()
}

fn group_by_file(sites: &[UnresolvedCallSite]) -> HashMap<String, Vec<&UnresolvedCallSite>> {
    let mut map: HashMap<String, Vec<&UnresolvedCallSite>> = HashMap::new();
    for site in sites {
        map.entry(site.file_path.clone()).or_default().push(site);
    }
    map
}

fn language_id_for(language: &str) -> &str {
    match language {
        "rust" => "rust",
        "python" => "python",
        "typescript" => "typescript",
        _ => "plaintext",
    }
}

// ---------------------------------------------------------------------------
// Node position index — maps (file, line) to (node_id, label)
// ---------------------------------------------------------------------------

struct NodePosition {
    id: String,
    label: String,
}

fn build_node_position_index(
    store: &GraphStore,
) -> Result<HashMap<(String, u64), NodePosition>, String> {
    let mut index = HashMap::new();
    for label in &["Function", "Method", "Struct", "Enum", "Trait"] {
        let qr = store.execute_query(&format!(
            "MATCH (n:{label}) RETURN n.id, n.qualified_name, n.start_line"
        ))?;
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let file = extract_file_from_qn(&row[1]);
            let line: u64 = row[2].parse().unwrap_or(0);
            index.insert(
                (file, line),
                NodePosition {
                    id: row[0].clone(),
                    label: label.to_string(),
                },
            );
        }
    }
    Ok(index)
}

// ---------------------------------------------------------------------------
// Edge insertion from LSP result
// ---------------------------------------------------------------------------

fn try_add_lsp_edge(
    store: &GraphStore,
    site: &UnresolvedCallSite,
    def: &lsp_client::DefinitionResult,
    node_index: &HashMap<(String, u64), NodePosition>,
    canonical_root: &Path,
) -> bool {
    // Convert LSP URI to a codebase-root-relative file path — the key space
    // of `node_index` (fleet-watch#18).
    let file_path = match uri_to_relative_path(&def.uri, canonical_root) {
        Some(p) => p,
        None => return false,
    };

    // Look up the definition in our node index.
    // LSP line is 0-based, our graph stores 1-based line numbers.
    let target_line = def.start_line + 1;

    // Try exact line match first, then scan nearby lines (+-2)
    let target = find_node_at_position(node_index, &file_path, target_line);
    let target = match target {
        Some(t) => t,
        None => return false,
    };

    // Build the edge type. Calls is Function|Method -> Function|Method;
    // ctor/variant/type-use targets degrade to Uses to preserve the
    // dependency edge instead of dropping it. source: stages/stage-3b.md §2.
    let rel_type = match (site.caller_label.as_str(), target.label.as_str()) {
        ("Function" | "Method", "Function" | "Method") => {
            format!("Calls_{}_{}", site.caller_label, target.label)
        }
        ("Function" | "Method", "Struct" | "Enum" | "Trait" | "TypeAlias") => {
            format!("Uses_{}_{}", site.caller_label, target.label)
        }
        _ => return false,
    };

    // Schema guard: dynamically formatted rel tables can outrun the
    // schema when a new caller/target label combination appears. Drop
    // rather than abort.
    if !is_known_rel_table(&rel_type) {
        eprintln!(
            "lsp_resolver: dropped edge with unknown rel table '{rel_type}' \
             ({} -> {}); add it to REL_TABLES in graph_store.rs",
            site.caller_qn, target.id
        );
        return false;
    }

    // Insert edge with LSP-backed confidence (0.9)
    store
        .insert_edge(
            &rel_type,
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

fn find_node_at_position<'a>(
    index: &'a HashMap<(String, u64), NodePosition>,
    file_path: &str,
    line: u64,
) -> Option<&'a NodePosition> {
    // Exact match
    if let Some(node) = index.get(&(file_path.to_string(), line)) {
        return Some(node);
    }
    // Try nearby lines (definition may start a few lines before the name)
    for offset in 1..=3 {
        if line >= offset {
            if let Some(node) = index.get(&(file_path.to_string(), line - offset)) {
                return Some(node);
            }
        }
        if let Some(node) = index.get(&(file_path.to_string(), line + offset)) {
            return Some(node);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::{NODE_CALL_SITE, NODE_FUNCTION};

    #[test]
    fn test_uri_to_relative_path_strips_canonical_root_and_percent_decodes() {
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
    fn extract_file_from_qn_covers_every_parsed_language() {
        // fleet-watch#18 adjacent defect (a): the old hardcoded list only
        // knew .rs/.py/.ts/.tsx and returned the whole qn for the rest.
        for (qn, want) in [
            ("src/main.rs::main", "src/main.rs"),
            ("app/Main.java::Main::run", "app/Main.java"),
            ("pkg/server.go::Handle", "pkg/server.go"),
            ("src/App.kt::App", "src/App.kt"),
            ("lib/util.cpp::frob", "lib/util.cpp"),
            ("lib/tool.rb::frob", "lib/tool.rb"),
            ("ui/View.swift::View::render", "ui/View.swift"),
        ] {
            assert_eq!(extract_file_from_qn(qn), want, "qn: {qn}");
        }
    }

    #[test]
    fn collect_skips_resolved_sites_but_keeps_unresolved_siblings() {
        // fleet-watch#18 adjacent defect (b): "unresolved" was decided at
        // caller granularity, so one statically resolved site hid every
        // sibling site of the same caller. The per-site `is_resolved` marker
        // (§10.4) is the signal the static resolver actually maintains.
        let dir = tempfile::Builder::new()
            .prefix("lsp_collect_test")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        store
            .insert_node(
                NODE_CALL_SITE,
                &[
                    ("id", "'src/a.rs::caller::call@5:4'"),
                    ("callee_name", "'x'"),
                    ("line", "5"),
                    ("col", "4"),
                    ("is_resolved", "true"),
                    ("language", "'rust'"),
                ],
            )
            .expect("insert resolved site");
        store
            .insert_node(
                NODE_CALL_SITE,
                &[
                    ("id", "'src/a.rs::caller::call@7:4'"),
                    ("callee_name", "'y'"),
                    ("line", "7"),
                    ("col", "4"),
                    ("is_resolved", "false"),
                    ("language", "'rust'"),
                ],
            )
            .expect("insert unresolved sibling");

        let sites = collect_unresolved_callsites(&store).expect("collect");
        assert_eq!(
            sites.len(),
            1,
            "only the is_resolved=false sibling must be collected"
        );
        assert_eq!(sites[0].id, "src/a.rs::caller::call@7:4");
        assert_eq!(sites[0].caller_qn, "src/a.rs::caller");
        assert_eq!(sites[0].file_path, "src/a.rs");
    }

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

    #[test]
    fn lsp_definition_inserts_calls_edge_via_relative_key() {
        // fleet-watch#18 end-to-end regression: pre-fix, the absolute
        // definition URI missed every node-index key, so the LSP pass had
        // inserted 0 edges — ever. Drives try_add_lsp_edge with a real store
        // and a percent-encoded absolute URI and asserts the edge lands on
        // the right caller/target pair.
        let dir = tempfile::Builder::new()
            .prefix("lsp edge test")
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
        let site = UnresolvedCallSite {
            id: "src/a.rs::caller::call@5:4".to_string(),
            caller_qn: "src/a.rs::caller".to_string(),
            caller_label: "Function".to_string(),
            callee_name: "target".to_string(),
            file_path: "src/a.rs".to_string(),
            line: 5,
            col: 4,
        };
        let def = lsp_client::DefinitionResult {
            uri: lsp_client::path_to_file_uri(&root.join("src/b.rs")),
            start_line: 9, // LSP is 0-based; the graph stores 1-based line 10
            start_col: 0,
        };
        assert!(
            try_add_lsp_edge(&store, &site, &def, &node_index, &root),
            "edge must be inserted from an absolute percent-encoded definition URI"
        );
        let qr = store
            .execute_query(
                "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) \
                 RETURN a.id, b.id",
            )
            .expect("query edge");
        assert_eq!(qr.rows.len(), 1, "exactly one Calls edge must exist");
        assert_eq!(qr.rows[0][0], "src/a.rs::caller");
        assert_eq!(qr.rows[0][1], "src/b.rs::target");
    }

    #[test]
    fn test_extract_caller_from_callsite_id() {
        assert_eq!(
            extract_caller_from_callsite_id("src/main.rs::main::call@5:4"),
            "src/main.rs::main"
        );
        assert_eq!(
            extract_caller_from_callsite_id("src/foo.rs::bar"),
            "src/foo.rs::bar"
        );
    }

    #[test]
    fn test_find_node_at_position_exact() {
        let mut index = HashMap::new();
        index.insert(
            ("src/main.rs".to_string(), 10),
            NodePosition {
                id: "fn1".to_string(),
                label: "Function".to_string(),
            },
        );
        let result = find_node_at_position(&index, "src/main.rs", 10);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "fn1");
    }

    #[test]
    fn test_find_node_at_position_nearby() {
        let mut index = HashMap::new();
        index.insert(
            ("src/main.rs".to_string(), 8),
            NodePosition {
                id: "fn2".to_string(),
                label: "Method".to_string(),
            },
        );
        let result = find_node_at_position(&index, "src/main.rs", 10);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "fn2");
    }

    #[test]
    fn test_find_node_at_position_not_found() {
        let index = HashMap::new();
        assert!(find_node_at_position(&index, "src/main.rs", 10).is_none());
    }

    #[test]
    fn test_language_id_for() {
        assert_eq!(language_id_for("rust"), "rust");
        assert_eq!(language_id_for("python"), "python");
        assert_eq!(language_id_for("unknown"), "plaintext");
    }
}
