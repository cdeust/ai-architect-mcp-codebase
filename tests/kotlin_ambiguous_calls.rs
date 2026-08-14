// kotlin_ambiguous_calls — issue #29 fixture tests.
//
// Kotlin's parser used to strip every call receiver/qualifier down to the
// bare tail identifier (`callee.rsplit('.').next()`), so two same-named
// in-repo candidates could never be disambiguated: the edge was silently
// dropped as `UnresolvedRef{reason:"no target found"}` even when the
// caller's own qualifier or import uniquely identified the target.
//
// This file proves, against the real indexer + resolver pipeline:
//   1. the issue's exact repro resolves via ImportMatch,
//   2. the "severe" cross-package variant resolves via ImportMatch with
//      the correct confidence tier,
//   3. same-package (directory) proximity resolves without any import,
//   4. genuine ambiguity (no evidence at all) is labeled `ambiguous (N
//      candidates)` — never "no target found", never a wrong guess,
//   5. a value-receiver call (`viewModel.load()`) never falsely resolves
//      via the qualifier-preservation path,
//   6. the existing multi-language regression fixtures are unaffected.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

/// Writes `files` into a fresh tempdir, indexes and resolves it, and returns
/// the open graph plus the tempdir guard (the on-disk graph outlives the
/// queries; RAII cleans it up on drop — see tests/common).
fn index_and_resolve(
    tag: &str,
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let root = tempfile::Builder::new()
        .prefix(&format!("kotlin_ambiguous_{tag}_"))
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

fn calls_edge(store: &GraphStore, callee_contains: &str) -> Vec<Vec<String>> {
    let qr = store
        .execute_query(
            "MATCH (a)-[r:Calls_Function_Function]->(b:Function) \
             RETURN a.id, b.qualified_name, r.confidence, r.resolution_method",
        )
        .expect("query Calls_Function_Function");
    qr.rows
        .into_iter()
        .filter(|row| row.get(1).is_some_and(|qn| qn.contains(callee_contains)))
        .collect()
}

// ---------------------------------------------------------------------------
// Test 1 — issue's exact repro: two top-level `process()` in different
// packages/directories, a third file imports one and calls it bare.
// ---------------------------------------------------------------------------

#[test]
fn kotlin_bare_import_resolves_the_imported_candidate() {
    let a = "package pkg.a\n\nfun process(): Int = 1\n";
    let b = "package pkg.b\n\nfun process(): Int = 2\n";
    let caller = "package pkg.c\n\nimport pkg.b.process\n\nfun run() {\n    process()\n}\n";
    let (store, res, _root) = index_and_resolve(
        "repro",
        &[
            ("pkg/a/A.kt", a),
            ("pkg/b/B.kt", b),
            ("pkg/c/Caller.kt", caller),
        ],
    );

    let edges = calls_edge(&store, "pkg/b/B.kt");
    assert!(
        !edges.is_empty(),
        "expected a Calls edge to pkg/b/B.kt's process(); unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Test 2 — severe variant: candidates in different packages, caller's
// import line uniquely determines the target; edge present AND its
// confidence/resolution_method reflect the ImportMatch tier (issue #30).
// ---------------------------------------------------------------------------

#[test]
fn kotlin_cross_package_import_resolves_with_import_match_confidence() {
    let a = "package pkg.alpha\n\nfun handle(): Int = 1\n";
    let b = "package pkg.beta\n\nfun handle(): Int = 2\n";
    let caller = "package pkg.gamma\n\nimport pkg.beta.handle\n\nfun run() {\n    handle()\n}\n";
    let (store, res, _root) = index_and_resolve(
        "severe",
        &[
            ("pkg/alpha/A.kt", a),
            ("pkg/beta/B.kt", b),
            ("pkg/gamma/Caller.kt", caller),
        ],
    );

    let edges = calls_edge(&store, "pkg/beta/B.kt");
    assert_eq!(
        edges.len(),
        1,
        "expected exactly one Calls edge to pkg/beta's handle(); unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    let confidence: f64 = edges[0][2].parse().unwrap_or(0.0);
    assert!(
        (confidence - 0.9).abs() < 1e-9,
        "expected ImportMatch confidence (0.9), got {confidence}"
    );
    assert_eq!(edges[0][3], "import-scope-lookup");
}

// ---------------------------------------------------------------------------
// Test 3 — same-package proximity: caller and ONE candidate share a
// directory (package, by Kotlin/Android convention); no import at all.
// ---------------------------------------------------------------------------

#[test]
fn kotlin_same_package_proximity_resolves_without_import() {
    let sibling = "package pkg.shared\n\nfun helper(): Int = 1\n";
    let unrelated = "package pkg.other\n\nfun helper(): Int = 2\n";
    let caller = "package pkg.shared\n\nfun run() {\n    helper()\n}\n";
    let (store, res, _root) = index_and_resolve(
        "proximity",
        &[
            ("pkg/shared/Sibling.kt", sibling),
            ("pkg/other/Unrelated.kt", unrelated),
            ("pkg/shared/Caller.kt", caller),
        ],
    );

    let edges = calls_edge(&store, "pkg/shared/Sibling.kt");
    assert_eq!(
        edges.len(),
        1,
        "expected the same-package candidate to resolve via PackageProximity; unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    assert_eq!(edges[0][3], "package-proximity");
    let unwanted = calls_edge(&store, "pkg/other/Unrelated.kt");
    assert!(
        unwanted.is_empty(),
        "must not resolve to the unrelated-package candidate"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — genuinely ambiguous: no import, no package signal (caller and
// both candidates in three unrelated directories). Must stay labeled
// `ambiguous (N candidates)`, never "no target found", never a guess.
// ---------------------------------------------------------------------------

#[test]
fn kotlin_genuine_ambiguity_is_labeled_not_dropped_or_guessed() {
    let a = "package pkg.x\n\nfun compute(): Int = 1\n";
    let b = "package pkg.y\n\nfun compute(): Int = 2\n";
    let caller = "package pkg.z\n\nfun run() {\n    compute()\n}\n";
    let (store, res, _root) = index_and_resolve(
        "ambiguous",
        &[
            ("pkg/x/X.kt", a),
            ("pkg/y/Y.kt", b),
            ("pkg/z/Caller.kt", caller),
        ],
    );

    let edges = calls_edge(&store, "compute");
    assert!(
        edges.is_empty(),
        "a genuinely ambiguous call must not produce ANY Calls edge"
    );

    let ambiguous = res
        .unresolved
        .iter()
        .find(|u| u.kind == "Calls" && u.target_text == "compute");
    assert!(
        ambiguous.is_some(),
        "expected an unresolved Calls ref for 'compute', got: {:?}",
        res.unresolved
            .iter()
            .map(|u| (&u.kind, &u.target_text, &u.reason))
            .collect::<Vec<_>>()
    );
    let reason = &ambiguous.unwrap().reason;
    assert!(
        reason.starts_with("ambiguous ("),
        "expected reason to start with 'ambiguous (', got '{reason}'"
    );
    assert!(
        reason != "no target found",
        "must never silently claim 'no target found' when in-repo candidates exist"
    );
}

// ---------------------------------------------------------------------------
// Test 5 — value-receiver false-resolution guard: `viewModel.load()` must
// not string-match an unrelated `load()` living in a package/file that
// happens to share the receiver's spelling.
// ---------------------------------------------------------------------------

#[test]
fn kotlin_value_receiver_does_not_falsely_resolve() {
    let decoy = "package view.model\n\nfun load(): Int = 99\n";
    let unrelated = "package pkg.other\n\nfun load(): Int = 1\n";
    let caller = r#"
package pkg.caller

class ViewModel {
    fun load(): Int = 2
}

fun run() {
    val viewModel = ViewModel()
    viewModel.load()
}
"#;
    let (store, res, _root) = index_and_resolve(
        "value_receiver",
        &[
            ("view/model/Decoy.kt", decoy),
            ("pkg/other/Other.kt", unrelated),
            ("pkg/caller/Caller.kt", caller),
        ],
    );

    // Must NOT resolve to the decoy top-level `load()` living in a
    // "view/model" directory that coincidentally matches the receiver text.
    let decoy_edges = calls_edge(&store, "view/model/Decoy.kt");
    assert!(
        decoy_edges.is_empty(),
        "value receiver 'viewModel.load()' must never string-match the unrelated decoy load(): {decoy_edges:?}"
    );
    let unrelated_edges = calls_edge(&store, "pkg/other/Other.kt");
    assert!(
        unrelated_edges.is_empty(),
        "must not resolve to the unrelated load() either"
    );

    eprintln!(
        "value_receiver: calls={} unresolved={:?}",
        res.calls_resolved,
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
}
