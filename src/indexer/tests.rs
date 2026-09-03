// indexer tests — moved verbatim from indexer/mod.rs (Fowler "Move") to keep
// the pipeline entry point under the §4.1 file cap. No behavior change.

use super::*;

#[test]
fn test_index_own_project() {
    use crate::test_support::TempDirExt;
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("indexer_test_graph_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&tmp);
    // Ensure the directory is fully gone before creating a fresh DB.
    assert!(!tmp.exists(), "failed to clean temp dir: {}", tmp.display());

    let result = index_codebase(Path::new("src"), &tmp).unwrap();

    assert!(
        result.files_indexed >= 3,
        "should index at least main.rs + graph_store.rs + lib.rs, got {}",
        result.files_indexed
    );
    assert!(
        result.node_count > 50,
        "should have many nodes, got {}",
        result.node_count
    );
    assert!(
        result.edge_count > 30,
        "should have many edges, got {}",
        result.edge_count
    );

    // Verify a known function exists via Cypher
    let store = GraphStore::open_or_create(&tmp).unwrap();
    let qr = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'main' RETURN f.name")
        .unwrap();
    assert!(!qr.rows.is_empty(), "should find main() in graph");
    assert_eq!(qr.rows[0][0], "main");

    // Verify file nodes exist
    let qr2 = store
        .execute_query("MATCH (f:File) RETURN count(f) AS cnt")
        .unwrap();
    assert!(
        !qr2.rows.is_empty() && qr2.rows[0][0].parse::<u64>().unwrap_or(0) > 0,
        "should have File nodes"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Shared fixture for the all-file-indexing tests below: code (AST) + JS
/// (light-link) + plain docs + structured data + BINARY docs, indexed
/// into a fresh graph. `tag` keeps each test's temp dirs distinct.
/// Split out of a single 96-line test (coding-standards §4.2) into two
/// focused tests that share this setup — one concern each (file-type
/// coverage vs. light-link edges), not a behavior change.
fn build_all_file_fixture(
    tag: &str,
) -> (
    crate::test_support::TestTempDir,
    crate::test_support::TestTempDir,
    IndexResult,
) {
    use crate::test_support::TempDirExt;
    use std::io::Write;
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let root = tempfile::Builder::new()
        .prefix(&format!("indexer_allfile_test_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("js")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();

    std::fs::write(root.join("mod.py"), "def f():\n    return 1\n").unwrap();
    std::fs::write(
        root.join("js/app.js"),
        "import { u } from './util.js';\nconst x = require('./util');\n",
    )
    .unwrap();
    std::fs::write(root.join("js/util.js"), "export const u = 1;\n").unwrap();
    // Markdown doc that references the python module and another doc.
    std::fs::write(
        root.join("docs/guide.md"),
        "# Guide\nSee [the code](../mod.py) and [arch](./arch.md).\n",
    )
    .unwrap();
    std::fs::write(root.join("docs/arch.md"), "# Arch\n").unwrap();
    std::fs::write(root.join("config.json"), "{\"k\": 1}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "plain text\n").unwrap();
    // Binary documents: arbitrary non-UTF8 bytes (a real .pdf/.docx header).
    std::fs::File::create(root.join("report.pdf"))
        .unwrap()
        .write_all(&[0x25, 0x50, 0x44, 0x46, 0x2d, 0x00, 0xff, 0xfe, 0x01])
        .unwrap();
    std::fs::File::create(root.join("spec.docx"))
        .unwrap()
        .write_all(&[0x50, 0x4b, 0x03, 0x04, 0x00, 0xff, 0x00, 0x12])
        .unwrap();

    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix(&format!("indexer_allfile_graph_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&tmp);
    let result = index_codebase(&root, &tmp).unwrap();
    (root, tmp, result)
}

#[test]
fn test_all_file_indexing_covers_every_file_type() {
    // All-file indexing: EVERY file becomes a File node — code, plain-text
    // docs, structured data, AND binary documents (.pdf/.docx).
    let (root, tmp, result) = build_all_file_fixture("counts");
    let store = GraphStore::open_or_create(&tmp).unwrap();

    // 9 files total — including the two BINARY documents.
    let files = store
        .execute_query("MATCH (f:File) RETURN count(f) AS n")
        .unwrap();
    assert_eq!(
        files.rows[0][0].parse::<u64>().unwrap(),
        9,
        "every file — code, text docs, data, AND binary .pdf/.docx — must be a File node"
    );

    // Each document type is a navigable File node (binary included).
    for id in [
        "docs/guide.md",
        "config.json",
        "notes.txt",
        "report.pdf",
        "spec.docx",
        "js/app.js",
    ] {
        let q = store
            .execute_query(&format!("MATCH (f:File) WHERE f.id = '{id}' RETURN f.id"))
            .unwrap();
        assert!(!q.rows.is_empty(), "missing File node for document: {id}");
    }

    assert!(result.node_count >= 9);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_all_file_indexing_light_links_markdown_and_js() {
    // Text docs get light links: Markdown `[..](path)` → References,
    // JS import/require → Imports.
    let (root, tmp, _result) = build_all_file_fixture("links");
    let store = GraphStore::open_or_create(&tmp).unwrap();

    // Markdown light-linking: guide.md references mod.py and arch.md.
    let refs = store
        .execute_query(
            "MATCH (a:File)-[:References_File_File]->(b:File) \
             WHERE a.id = 'docs/guide.md' RETURN b.id",
        )
        .unwrap();
    let ref_targets: Vec<&str> = refs.rows.iter().map(|r| r[0].as_str()).collect();
    assert!(
        ref_targets.contains(&"mod.py") && ref_targets.contains(&"docs/arch.md"),
        "guide.md should reference mod.py + docs/arch.md, got {ref_targets:?}"
    );

    // JS light-linking: app.js imports util.js.
    let imp = store
        .execute_query(
            "MATCH (a:File)-[:Imports_File_File]->(b:File) \
             WHERE a.id = 'js/app.js' RETURN b.id",
        )
        .unwrap();
    assert!(
        imp.rows.iter().any(|r| r[0] == "js/util.js"),
        "js/app.js should import js/util.js, got {:?}",
        imp.rows
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[cfg(unix)]
fn test_symlink_skipped() {
    // source: C4 fix — symlinks in the walked tree must not be followed.
    // We build a small directory with one real .rs file and one symlink
    // that points to a file OUTSIDE the tree (a decoy `/etc/hostname`).
    // collect_source_files must return only the real file.
    use crate::test_support::TempDirExt;
    use std::os::unix::fs::symlink;

    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let root = tempfile::Builder::new()
        .prefix("indexer_symlink_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // Real file: a simple Rust source.
    let real_file = root.join("real.rs");
    std::fs::write(&real_file, "fn main() {}\n").unwrap();

    // Symlink → /etc/hostname (exists on macOS, has the .rs extension
    // faked via the link name so the walker would pick it up if it
    // followed symlinks).
    let link = root.join("leaky.rs");
    // Guard: if the target doesn't exist, use another known file.
    let target = if Path::new("/etc/hostname").exists() {
        Path::new("/etc/hostname")
    } else {
        Path::new("/etc/passwd")
    };
    symlink(target, &link).unwrap();

    let outcome = collect_source_files(&root, WalkOptions::default()).unwrap();
    let files = outcome.files;
    // Only the real file is indexed; the symlink is skipped.
    assert_eq!(files.len(), 1, "symlink must not be collected: {files:?}");
    assert_eq!(files[0].file_name().unwrap(), "real.rs");

    let _ = std::fs::remove_dir_all(&root);
}

/// Fixture for `test_dependency_scope_walk`: root/app.rs,
/// root/node_modules/dep.rs, root/deps/dep2.rs, root/.git/hook.rs.
fn write_dependency_scope_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    std::fs::create_dir_all(root.join("deps")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("app.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("node_modules/dep.rs"), "fn dep() {}\n").unwrap();
    std::fs::write(root.join("deps/dep2.rs"), "fn dep2() {}\n").unwrap();
    std::fs::write(root.join(".git/hook.rs"), "fn hook() {}\n").unwrap();
}

#[test]
fn test_dependency_scope_walk() {
    // Proves DependencyScope toggles descent into build/dependency dirs
    // while always excluding `.git`.
    use crate::test_support::TempDirExt;
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let root = tempfile::Builder::new()
        .prefix("indexer_include_deps_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&root);
    write_dependency_scope_fixture(&root);

    let names = |opts: WalkOptions| -> Vec<String> {
        let mut v: Vec<String> = collect_source_files(&root, opts)
            .unwrap()
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };

    // Default (DependencyScope::None): node_modules, deps, and .git are all
    // pruned. `deps` regression: issue observed 2026-08-06 — Cortex repo's
    // gitignored deps/ (1.1 GB vendored Python site-packages) was walked
    // into before this entry existed.
    assert_eq!(names(WalkOptions::default()), vec!["app.rs"]);

    // Full: node_modules and deps are descended, .git stays out.
    let full = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::Full,
        exclude_dirs: ExcludeSet::default(),
    };
    assert_eq!(names(full), vec!["app.rs", "dep.rs", "dep2.rs"]);

    // PublicApi also descends at the walk level — the visibility filter
    // is applied at persistence time (see persist::tests), not here.
    let public_api = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::PublicApi,
        exclude_dirs: ExcludeSet::default(),
    };
    assert_eq!(names(public_api), vec!["app.rs", "dep.rs", "dep2.rs"]);

    let _ = std::fs::remove_dir_all(&root);
}

/// Fixture for `test_public_api_scope_filters_dependency_symbols_only`: a
/// project file (app.rs, one pub + one private fn) and a dependency file
/// under node_modules (dep.rs, one pub + one private fn).
fn write_public_api_scope_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    std::fs::write(
        root.join("app.rs"),
        "pub fn app_pub() {}\nfn app_private() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("node_modules/dep.rs"),
        "pub fn dep_pub() {}\nfn dep_private() {}\n",
    )
    .unwrap();
}

#[test]
fn test_public_api_scope_filters_dependency_symbols_only() {
    // PublicApi must drop the PRIVATE fn from dep.rs only — the project
    // file's private fn stays, and dep.rs's pub fn stays too.
    // source: ADR-4253701 §Decision 1.
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    use crate::test_support::TempDirExt;
    let root = tempfile::Builder::new()
        .prefix("indexer_public_api_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&root);
    write_public_api_scope_fixture(&root);

    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let graph_path = tempfile::Builder::new()
        .prefix("indexer_public_api_graph_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&graph_path);

    let options = IndexOptions {
        language_filter: None,
        dependency_scope: DependencyScope::PublicApi,
        exclude_dirs: ExcludeSet::default(),
    };
    index_codebase_with_language(&root, &graph_path, &options).expect("index should succeed");

    let store = GraphStore::open_or_create(&graph_path).unwrap();
    let qr = store
        .execute_query("MATCH (f:Function) RETURN f.name")
        .unwrap();
    let mut names: Vec<String> = qr.rows.into_iter().map(|r| r[0].clone()).collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "app_private".to_string(),
            "app_pub".to_string(),
            "dep_pub".to_string()
        ],
        "PublicApi must keep both project functions and only the pub dependency function"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&graph_path);
}

/// Cost-measurement probe for full-AST persistence: not a correctness
/// assertion (that's `test_all_file_indexing_covers_every_file_type` and
/// `tests/full_ast_completeness.rs`) — `#[ignore]`d, like this file's other
/// manual-run probes, so `cargo test` never runs it automatically. Indexes
/// this repo's own `src/` into a scratch dir under `std::env::temp_dir()`,
/// prints node/edge counts, wall-clock, and on-disk size, and cleans up its
/// scratch dir on every exit path (including a `panic!`/`expect` failure,
/// via a drop guard) so a manual run never leaks. Run with:
///   cargo test --release --lib indexer::tests::measure_full_ast_cost_on_own_src -- --ignored --nocapture
#[test]
#[ignore]
fn measure_full_ast_cost_on_own_src() {
    struct CleanupOnDrop(std::path::PathBuf);
    impl Drop for CleanupOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let scratch = std::env::temp_dir().join("ap-full-ast-cost-probe");
    let _guard = CleanupOnDrop(scratch.clone());
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(&scratch);

    let start = std::time::Instant::now();
    let result = super::index_codebase(Path::new("src"), &scratch).expect("index");
    let elapsed = start.elapsed();
    let du_bytes = dir_size(&scratch);
    eprintln!(
        "COST MEASUREMENT: files_indexed={} node_count={} edge_count={} elapsed_ms={} \
         reported_elapsed_ms={} on_disk_bytes={} on_disk_human={:.1} MiB",
        result.files_indexed,
        result.node_count,
        result.edge_count,
        elapsed.as_millis(),
        result.elapsed_ms,
        du_bytes,
        du_bytes as f64 / (1024.0 * 1024.0)
    );
}

/// Root cause 1 (fix/lsp-receiver-calls): `indexer::persist::nodes` used to
/// hardcode `CallSite.col` to `"0"` for every call site, regardless of where
/// the call actually sat on its line — `lsp_resolve` then queried
/// `textDocument/definition` at column 0 on every request, landing on
/// whatever token starts the line (indentation, a brace, an unrelated
/// identifier) instead of the call. Every parser spec now emits the call's
/// own 0-based tree-sitter column as the `lsp_col` property (see
/// `src/parser/spec/rust.rs::call_site` and its siblings), and
/// `append_label_properties` persists it as `CallSite.col`.
///
/// This fixture puts the call four columns deep in an indented method body,
/// so `col == 0` (the pre-fix value) and the correct value are
/// distinguishable — a bug that happened to leave `col` at a coincidental 0
/// would not be caught by an assertion that merely checks `> 0` against a
/// call at column 0.
#[test]
fn callsite_column_is_persisted_not_zero() {
    use crate::test_support::TempDirExt;

    let src_dir = tempfile::Builder::new()
        .prefix("callsite_col_src_")
        .tempdir()
        .expect("src tempdir");
    std::fs::write(
        src_dir.path().join("lib.rs"),
        "struct Widget;\n\
         impl Widget {\n\
         \x20\x20\x20\x20fn helper(&self, i: i32) -> i32 { i }\n\
         \x20\x20\x20\x20fn caller(&self) -> i32 {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20self.helper(1)\n\
         \x20\x20\x20\x20}\n\
         }\n",
    )
    .expect("write fixture");

    let graph_dir = tempfile::Builder::new()
        .prefix("callsite_col_graph_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&graph_dir);

    index_codebase(src_dir.path(), &graph_dir).expect("index");

    let store = GraphStore::open_or_create(&graph_dir).expect("open store");
    let qr = store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.callee_name = 'self.helper' \
             RETURN cs.col",
        )
        .expect("query CallSite.col");
    assert_eq!(qr.rows.len(), 1, "exactly one `self.helper(1)` call site");
    // Line 5, `        self.helper(1)` — 8 spaces of indentation before
    // `self`, 0-based column 8. The pre-fix code persisted "0" here
    // regardless of the call's real position.
    assert_eq!(
        qr.rows[0][0], "8",
        "CallSite.col must be the call's real 0-based column, not the \
         hardcoded 0 the pre-fix indexer wrote for every call site"
    );
}

#[cfg(test)]
fn dir_size(path: &std::path::Path) -> u64 {
    // lbug may materialize the database as a single file OR a directory
    // depending on size/config — measure whichever it produced.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() {
            return meta.len();
        }
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}
