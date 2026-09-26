//! Issue #366, part A. A graph can hold a language-server row an earlier run
//! wrote to a `#[cfg]` twin the build now compiles out (an edit of `Cargo.toml`
//! flips `cfg_active`, or a server with another cfg set answered). The next
//! `resolve_graph` must delete that row, caller-level and per-site, reopen the
//! site, and let the resolve decide it again, while every other row stays.

use crate::graph_store::{GraphStore, CFG_ACTIVE, CFG_INACTIVE, CFG_UNKNOWN};

const FAST: &str = "src/lib.rs::pick#cfg(feature=fast)";
const SLOW: &str = "src/lib.rs::pick#cfg(not(feature=fast))";
const UNIX: &str = "src/lib.rs::probe#cfg(unix)";
const CALLER: &str = "src/a.rs::caller";
const OTHER: &str = "src/a.rs::other";
const TO_FAST: &str = "src/a.rs::caller::call@3:5";
const TO_SLOW: &str = "src/a.rs::caller::call@4:5";
const TO_UNIX: &str = "src/a.rs::caller::call@5:5";
const STATIC_TO_FAST: &str = "src/a.rs::other::call@9:5";

fn function(store: &GraphStore, id: &str, start: u64) {
    let name = crate::graph_store::strip_cfg_gates(id)
        .rsplit("::")
        .next()
        .unwrap_or(id)
        .to_string();
    store
        .insert_node(
            "Function",
            &[
                ("id", &format!("'{id}'")),
                ("name", &format!("'{name}'")),
                ("qualified_name", &format!("'{id}'")),
                ("start_line", &start.to_string()),
                ("end_line", &(start + 2).to_string()),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert function");
}

/// A resolved call site and the row it holds.
struct Row<'a> {
    site: &'a str,
    callee: &'a str,
    target: &'a str,
    method: &'a str,
}

/// Inserts `row.site` as resolved, with its per-site row and the caller-level
/// row of the same method.
fn resolved_site(store: &GraphStore, row: &Row<'_>) {
    let line = row
        .site
        .rsplit('@')
        .next()
        .unwrap()
        .split(':')
        .next()
        .unwrap();
    store
        .insert_node(
            "CallSite",
            &[
                ("id", &format!("'{}'", row.site)),
                ("callee_name", &format!("'{}'", row.callee)),
                ("line", line),
                ("col", "5"),
                ("is_resolved", "true"),
                ("language", "'rust'"),
            ],
        )
        .expect("insert site");
    let props = [("confidence", "0.9"), ("resolution_method", row.method)];
    let caller = row.site.rsplit_once("::call@").unwrap().0;
    store
        .insert_edge_if_absent("Calls_Function_Function", caller, row.target, &props)
        .expect("caller row");
    store
        .insert_edge_if_absent("Calls_CallSite_Function", row.site, row.target, &props)
        .expect("site row");
}

fn stale_graph(dir: &std::path::Path) -> GraphStore {
    let store = GraphStore::open_or_create(&dir.join("db")).expect("open");
    store.create_schema().expect("schema");
    for (id, line) in [(FAST, 2), (SLOW, 5), (UNIX, 8), (CALLER, 1), (OTHER, 8)] {
        function(&store, id, line);
    }
    resolved_site(
        &store,
        &Row {
            site: TO_FAST,
            callee: "pick",
            target: FAST,
            method: "'lsp-definition'",
        },
    );
    resolved_site(
        &store,
        &Row {
            site: TO_SLOW,
            callee: "pick",
            target: SLOW,
            method: "'lsp-definition'",
        },
    );
    resolved_site(
        &store,
        &Row {
            site: TO_UNIX,
            callee: "probe",
            target: UNIX,
            method: "'lsp-definition'",
        },
    );
    resolved_site(
        &store,
        &Row {
            site: STATIC_TO_FAST,
            callee: "zzz",
            target: FAST,
            method: "'unique-match'",
        },
    );
    store
        .write_cfg_active(&[
            ("Function".to_string(), FAST.to_string(), CFG_INACTIVE),
            ("Function".to_string(), SLOW.to_string(), CFG_ACTIVE),
            ("Function".to_string(), UNIX.to_string(), CFG_UNKNOWN),
        ])
        .expect("cfg_active");
    store
}

fn rows(store: &GraphStore, rel: &str, from: &str) -> Vec<(String, String)> {
    store
        .execute_query(&format!(
            "MATCH (a {{id: '{from}'}})-[r:{rel}]->(t) RETURN t.id, r.resolution_method \
             ORDER BY t.id"
        ))
        .expect("query")
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect()
}

fn pair(target: &str, method: &str) -> (String, String) {
    (target.to_string(), method.to_string())
}

#[test]
fn resolve_graph_drops_a_language_server_row_to_a_compiled_out_twin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = stale_graph(dir.path());
    crate::resolver::resolve_graph(&store).expect("resolve");

    // The stale row to the compiled-out twin is gone, and the reopened site is
    // decided again: the default profile compiles `SLOW`.
    let site = rows(&store, "Calls_CallSite_Function", TO_FAST);
    assert!(
        !site.iter().any(|(t, _)| t == FAST),
        "the row to the compiled-out twin must be deleted: {site:?}"
    );
    assert_eq!(site, [pair(SLOW, "cfg-selected")]);
    // Caller level: no row of any method to the compiled-out twin, and a row
    // to the compiled twin (caller-level rows are one per target, so its
    // method may be another site's).
    let caller = rows(&store, "Calls_Function_Function", CALLER);
    assert!(
        !caller.iter().any(|(t, _)| t == FAST),
        "the caller must keep no row to the compiled-out twin: {caller:?}"
    );
    assert!(
        caller.iter().any(|(t, _)| t == SLOW),
        "the caller must reach the compiled twin: {caller:?}"
    );

    // Rows to a compiled or an undecided twin, and a row of another method
    // to the compiled-out twin, are untouched.
    assert!(
        rows(&store, "Calls_CallSite_Function", TO_SLOW).contains(&pair(SLOW, "lsp-definition"))
    );
    assert!(
        rows(&store, "Calls_CallSite_Function", TO_UNIX).contains(&pair(UNIX, "lsp-definition"))
    );
    assert_eq!(
        rows(&store, "Calls_CallSite_Function", STATIC_TO_FAST),
        [pair(FAST, "unique-match")]
    );
}
